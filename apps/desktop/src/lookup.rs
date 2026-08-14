use std::{error::Error, fmt, sync::Arc};

use lvos_core::{LanguageCode, PreparedContent, UnixTimestamp};
use lvos_storage::{HistoryEntry, QueryStats, StoredContent, TranslationSnapshot};
use lvos_translation::{TranslationError, TranslationRequest, TranslationRouter};

use crate::{DatabaseWorker, DatabaseWorkerError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupMode {
    UseCache,
    Refresh,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupOutcome {
    pub history: HistoryEntry,
    pub query_stats: QueryStats,
    pub cache_hit: bool,
}

#[derive(Debug)]
pub struct LookupService {
    database: Arc<DatabaseWorker>,
    router: Option<TranslationRouter>,
}

impl LookupService {
    #[must_use]
    pub fn new(database: Arc<DatabaseWorker>, router: TranslationRouter) -> Self {
        Self {
            database,
            router: Some(router),
        }
    }

    /// Builds the local-cache path before a Provider has been configured.
    #[must_use]
    pub fn new_without_provider(database: Arc<DatabaseWorker>) -> Self {
        Self {
            database,
            router: None,
        }
    }

    /// Resolves a cache hit or performs one Provider request, then commits History and `QueryStats`.
    ///
    /// A refresh bypasses History but does not mutate the independent Favorite snapshot.
    ///
    /// # Errors
    /// Returns an error when Profile persistence or translation fails.
    pub async fn lookup(
        &self,
        content: PreparedContent,
        target_language: LanguageCode,
        mode: LookupMode,
        now: UnixTimestamp,
    ) -> Result<LookupOutcome, LookupError> {
        let key = content.content_key();
        if mode == LookupMode::UseCache {
            let cached = self
                .database
                .execute(move |database| database.history(key))
                .await?;
            if let Some(mut history) =
                cached.filter(|entry| entry.translation.target_lang == target_language)
            {
                history.last_queried_at = now;
                let stored = history.clone();
                let query_stats = self
                    .database
                    .execute(move |database| database.record_successful_query(&stored))
                    .await?;
                return Ok(LookupOutcome {
                    history,
                    query_stats,
                    cache_hit: true,
                });
            }
        }

        let translated = self
            .router
            .as_ref()
            .ok_or(TranslationError::MissingConfiguration)?
            .translate(&TranslationRequest {
                text: content.source_text().to_owned(),
                source_language: content.source_lang().clone(),
                target_language: target_language.clone(),
            })
            .await?;
        let history = HistoryEntry {
            content: StoredContent {
                content_key: content.content_key(),
                key_version: content.key_version(),
                kind: content.kind(),
                source_lang: content.source_lang().clone(),
                source_text: content.source_text().to_owned(),
                canonical_text: content.canonical_text().to_owned(),
            },
            translation: TranslationSnapshot {
                target_lang: target_language,
                translation: translated.text,
                provider: translated.provider.to_string(),
                updated_at: now,
            },
            last_queried_at: now,
        };
        let stored = history.clone();
        let query_stats = self
            .database
            .execute(move |database| database.record_successful_query(&stored))
            .await?;
        Ok(LookupOutcome {
            history,
            query_stats,
            cache_hit: false,
        })
    }
}

#[derive(Debug)]
pub enum LookupError {
    Database(DatabaseWorkerError),
    Translation(TranslationError),
}

impl fmt::Display for LookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "lookup persistence failed: {error}"),
            Self::Translation(error) => write!(formatter, "lookup translation failed: {error}"),
        }
    }
}

impl Error for LookupError {}

impl From<DatabaseWorkerError> for LookupError {
    fn from(value: DatabaseWorkerError) -> Self {
        Self::Database(value)
    }
}

impl From<TranslationError> for LookupError {
    fn from(value: TranslationError) -> Self {
        Self::Translation(value)
    }
}
