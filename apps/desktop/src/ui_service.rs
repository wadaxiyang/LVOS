use std::{error::Error, fmt, str::FromStr, sync::Arc};

use lvos_core::{ContentKey, UnixTimestamp};
use lvos_storage::{PortableImportPlan, PortableImportResult};

use crate::{DatabaseWorker, SyncWorkerHandle};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiRecordData {
    pub key: ContentKey,
    pub source: String,
    pub translation: String,
    pub count: u64,
    pub favorite: bool,
    pub metadata: String,
}

/// Asynchronous application boundary for data-backed Desktop UI actions.
///
/// Every operation is queued onto the dedicated database worker, so Slint callbacks can spawn
/// these futures without blocking the event-loop thread.
#[derive(Clone, Debug)]
pub struct UiDataService {
    database: Arc<DatabaseWorker>,
    sync: Option<SyncWorkerHandle>,
}

impl UiDataService {
    #[must_use]
    pub fn new(database: Arc<DatabaseWorker>) -> Self {
        Self {
            database,
            sync: None,
        }
    }

    #[must_use]
    pub fn with_sync_worker(mut self, sync: SyncWorkerHandle) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Loads matching History records with effective learning counts and Favorite status.
    ///
    /// # Errors
    /// Returns an error when the active Profile cannot be read.
    pub async fn history(
        &self,
        term: String,
        limit: u32,
    ) -> Result<Vec<UiRecordData>, UiDataError> {
        self.database
            .execute(move |database| {
                database
                    .search_history(&term, limit)?
                    .into_iter()
                    .map(|entry| {
                        let key = entry.content.content_key;
                        let count = database
                            .query_stats(key)?
                            .map_or(0, lvos_storage::QueryStats::effective_total);
                        let favorite = database
                            .favorite_by_key(key)?
                            .is_some_and(|item| item.deleted_at.is_none());
                        Ok(UiRecordData {
                            key,
                            source: entry.content.source_text,
                            translation: entry.translation.translation,
                            count,
                            favorite,
                            metadata: entry.translation.provider,
                        })
                    })
                    .collect()
            })
            .await
            .map_err(UiDataError::Database)
    }

    /// Loads active Favorites with their effective learning counts.
    ///
    /// # Errors
    /// Returns an error when the active Profile cannot be read.
    pub async fn favorites(
        &self,
        term: String,
        limit: u32,
    ) -> Result<Vec<UiRecordData>, UiDataError> {
        self.database
            .execute(move |database| {
                database
                    .search_favorites(&term, limit)?
                    .into_iter()
                    .map(|favorite| {
                        let key = favorite.content.content_key;
                        let count = database
                            .query_stats(key)?
                            .map_or(0, lvos_storage::QueryStats::effective_total);
                        Ok(UiRecordData {
                            key,
                            source: favorite.content.source_text,
                            translation: favorite.translation.translation,
                            count,
                            favorite: true,
                            metadata: favorite.translation.provider,
                        })
                    })
                    .collect()
            })
            .await
            .map_err(UiDataError::Database)
    }

    /// Applies an explicit Favorite toggle and returns the resulting active state.
    ///
    /// # Errors
    /// Returns an error for an invalid key or failed Profile mutation.
    pub async fn set_favorite(
        &self,
        key: String,
        active: bool,
        now: UnixTimestamp,
    ) -> Result<bool, UiDataError> {
        let key = ContentKey::from_str(&key).map_err(|_| UiDataError::InvalidContentKey)?;
        let result = self
            .database
            .execute(move |database| {
                if active {
                    database.favorite(key, now)?;
                } else {
                    database.unfavorite(key, now)?;
                }
                Ok(active)
            })
            .await
            .map_err(UiDataError::Database)?;
        if let Some(sync) = &self.sync {
            sync.wake();
        }
        Ok(result)
    }

    /// Clears History while preserving Favorite-domain data according to the storage contract.
    ///
    /// # Errors
    /// Returns an error when the Profile mutation fails.
    pub async fn clear_history(&self) -> Result<(), UiDataError> {
        self.database
            .execute(lvos_storage::ProfileDatabase::clear_history)
            .await
            .map_err(UiDataError::Database)
    }

    /// Produces the bounded portable JSON payload for a native save-file workflow.
    ///
    /// # Errors
    /// Returns an error when the active Profile cannot be exported.
    pub async fn export_portable_json(&self) -> Result<Vec<u8>, UiDataError> {
        self.database
            .export_portable_json()
            .await
            .map_err(UiDataError::Database)
    }

    /// Fully validates a selected portable JSON document and returns its merge preview ticket.
    ///
    /// # Errors
    /// Returns an error when the active Profile cannot be read or the document is invalid.
    pub async fn preview_portable_import(
        &self,
        bytes: Vec<u8>,
    ) -> Result<PortableImportPlan, UiDataError> {
        self.database
            .preview_portable_import(bytes)
            .await
            .map_err(UiDataError::Database)
    }

    /// Applies a confirmed preview and wakes Favorite synchronization afterward.
    ///
    /// # Errors
    /// Returns an error when the active Profile changed or the import transaction fails.
    pub async fn apply_portable_import(
        &self,
        plan: PortableImportPlan,
        now: UnixTimestamp,
    ) -> Result<PortableImportResult, UiDataError> {
        let result = self
            .database
            .apply_portable_import(plan, now)
            .await
            .map_err(UiDataError::Database)?;
        if let Some(sync) = &self.sync {
            sync.wake();
        }
        Ok(result)
    }
}

#[derive(Debug)]
pub enum UiDataError {
    InvalidContentKey,
    Database(crate::DatabaseWorkerError),
}

impl fmt::Display for UiDataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContentKey => formatter.write_str("invalid Content identity"),
            Self::Database(error) => write!(formatter, "Desktop data operation failed: {error}"),
        }
    }
}

impl Error for UiDataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::InvalidContentKey => None,
        }
    }
}
