use std::{error::Error, fmt, sync::Arc};

use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use lvos_core::UnixTimestamp;
use lvos_storage::{InstallationMetadata, InstallationStore, Platform, StorageError};
use uuid::Uuid;

use crate::{DatabaseWorker, DatabaseWorkerError};

/// Coordinates explicit revoked-Device recovery across installation, Profiles, and credentials.
pub struct DeviceIdentityManager {
    installation: InstallationStore,
    database: Arc<DatabaseWorker>,
    credentials: Arc<dyn CredentialStore>,
}

impl fmt::Debug for DeviceIdentityManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentityManager")
            .field("installation", &self.installation)
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentityManager {
    #[must_use]
    pub fn new(
        installation: InstallationStore,
        database: Arc<DatabaseWorker>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            installation,
            database,
            credentials,
        }
    }

    /// Generates a new permanent installation Device identity only after explicit confirmation.
    ///
    /// Profile rows and Outbox events are retained. Old refresh credentials are removed after the
    /// durable identity replacement, and the caller must then reauthenticate the new Device.
    ///
    /// # Errors
    /// Returns an error without changing identity when confirmation is absent or Profile/install
    /// persistence fails. Credential cleanup errors are reported after the new identity is durable.
    pub async fn regenerate_after_revocation(
        &self,
        confirmed: bool,
        platform: Platform,
        device_name: &str,
        now: UnixTimestamp,
    ) -> Result<InstallationMetadata, DeviceIdentityError> {
        if !confirmed {
            return Err(DeviceIdentityError::ConfirmationRequired);
        }
        let current = self.installation.load_or_create(platform, device_name)?;
        let profiles = self.database.profile_metadata().await?;
        let replacement = Uuid::new_v4();
        self.database
            .replace_profile_device_identity(current.device_id, replacement, now)
            .await?;
        let installation = match self
            .installation
            .replace_revoked_identity(current.device_id, replacement)
        {
            Ok(installation) => installation,
            Err(error) => {
                let _ = self
                    .database
                    .replace_profile_device_identity(replacement, current.device_id, now)
                    .await;
                return Err(DeviceIdentityError::Storage(error));
            }
        };
        for profile in profiles {
            let (Some(user_id), Some(server_origin)) = (profile.user_id, profile.server_origin)
            else {
                continue;
            };
            self.credentials.delete(&CredentialScope {
                server_origin,
                user_id: user_id.to_string(),
                device_id: current.device_id.to_string(),
                key: CredentialKey::ServerRefreshToken,
            })?;
        }
        Ok(installation)
    }
}

#[derive(Debug)]
pub enum DeviceIdentityError {
    ConfirmationRequired,
    Database(DatabaseWorkerError),
    Storage(StorageError),
    CredentialStore(AuthError),
}

impl fmt::Display for DeviceIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfirmationRequired => {
                formatter.write_str("Device identity replacement requires explicit confirmation")
            }
            Self::Database(error) => write!(formatter, "Profile identity update failed: {error}"),
            Self::Storage(error) => {
                write!(formatter, "installation identity update failed: {error}")
            }
            Self::CredentialStore(error) => {
                write!(formatter, "old session cleanup failed: {error}")
            }
        }
    }
}

impl Error for DeviceIdentityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::CredentialStore(error) => Some(error),
            Self::ConfirmationRequired => None,
        }
    }
}

impl From<DatabaseWorkerError> for DeviceIdentityError {
    fn from(value: DatabaseWorkerError) -> Self {
        Self::Database(value)
    }
}

impl From<StorageError> for DeviceIdentityError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<AuthError> for DeviceIdentityError {
    fn from(value: AuthError) -> Self {
        Self::CredentialStore(value)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, num::NonZeroUsize, sync::Mutex};

    use lvos_core::{LanguageCode, ValidationPolicy, prepare_content};
    use lvos_storage::{HistoryEntry, ProfileMetadata, StoredContent, TranslationSnapshot};

    use super::*;

    #[derive(Debug, Default)]
    struct MemoryCredentials(Mutex<HashMap<CredentialScope, Vec<u8>>>);

    impl CredentialStore for MemoryCredentials {
        fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| AuthError::CredentialStore)?
                .get(scope)
                .cloned())
        }

        fn contains(&self, scope: &CredentialScope) -> Result<bool, AuthError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| AuthError::CredentialStore)?
                .contains_key(scope))
        }

        fn set(&self, scope: &CredentialScope, secret: &[u8]) -> Result<(), AuthError> {
            self.0
                .lock()
                .map_err(|_| AuthError::CredentialStore)?
                .insert(scope.clone(), secret.to_vec());
            Ok(())
        }

        fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError> {
            self.0
                .lock()
                .map_err(|_| AuthError::CredentialStore)?
                .remove(scope);
            Ok(())
        }
    }

    #[tokio::test]
    async fn confirmed_revocation_recovery_preserves_profile_and_outbox() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
        let installation = InstallationStore::new(directory.path());
        let original = installation
            .load_or_create(Platform::Macos, "Test Mac")
            .unwrap_or_else(|error| unreachable!("installation fixture: {error}"));
        let user_id = Uuid::new_v4();
        let metadata = ProfileMetadata {
            profile_id: Uuid::new_v4(),
            user_id: Some(user_id),
            username: Some("alice".to_owned()),
            device_id: original.device_id,
            platform: "macos".to_owned(),
            server_origin: Some("https://sync.example".to_owned()),
            last_server_revision: 7,
            created_at: UnixTimestamp::from_seconds(100),
            updated_at: UnixTimestamp::from_seconds(100),
        };
        let database = Arc::new(
            DatabaseWorker::start(directory.path().to_path_buf())
                .unwrap_or_else(|error| unreachable!("worker fixture: {error}")),
        );
        database
            .switch_profile(metadata)
            .await
            .unwrap_or_else(|error| unreachable!("profile fixture: {error}"));
        let history = history();
        let key = history.content.content_key;
        database
            .execute(move |profile| {
                profile.record_successful_query(&history)?;
                profile.favorite(key, UnixTimestamp::from_seconds(101))?;
                Ok(())
            })
            .await
            .unwrap_or_else(|error| unreachable!("favorite fixture: {error}"));
        let before = database
            .execute(|profile| profile.outbox_events())
            .await
            .unwrap_or_default();
        let credentials = Arc::new(MemoryCredentials::default());
        let old_scope = CredentialScope {
            server_origin: "https://sync.example".to_owned(),
            user_id: user_id.to_string(),
            device_id: original.device_id.to_string(),
            key: CredentialKey::ServerRefreshToken,
        };
        credentials
            .set(&old_scope, b"refresh-secret")
            .unwrap_or_else(|error| unreachable!("credential fixture: {error}"));
        let store: Arc<dyn CredentialStore> = credentials.clone();
        let manager = DeviceIdentityManager::new(installation, Arc::clone(&database), store);
        assert!(matches!(
            manager
                .regenerate_after_revocation(
                    false,
                    Platform::Macos,
                    "Test Mac",
                    UnixTimestamp::from_seconds(102)
                )
                .await,
            Err(DeviceIdentityError::ConfirmationRequired)
        ));
        let replacement = manager
            .regenerate_after_revocation(
                true,
                Platform::Macos,
                "Test Mac",
                UnixTimestamp::from_seconds(102),
            )
            .await
            .unwrap_or_else(|error| unreachable!("replacement fixture: {error}"));
        assert_ne!(replacement.device_id, original.device_id);
        let profile = database
            .execute(|profile| profile.metadata())
            .await
            .unwrap_or_else(|error| unreachable!("metadata fixture: {error}"));
        assert_eq!(profile.device_id, replacement.device_id);
        assert_eq!(profile.last_server_revision, 7);
        let after = database
            .execute(|profile| profile.outbox_events())
            .await
            .unwrap_or_default();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].event_id, before[0].event_id);
        assert!(!credentials.contains(&old_scope).unwrap_or(true));
    }

    fn history() -> HistoryEntry {
        let prepared = prepare_content(
            "Preserve me",
            LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
            ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN)),
        )
        .unwrap_or_else(|error| unreachable!("fixture: {error}"));
        HistoryEntry {
            content: StoredContent {
                content_key: prepared.content_key(),
                key_version: prepared.key_version(),
                kind: prepared.kind(),
                source_lang: prepared.source_lang().clone(),
                source_text: prepared.source_text().to_owned(),
                canonical_text: prepared.canonical_text().to_owned(),
            },
            translation: TranslationSnapshot {
                target_lang: LanguageCode::parse("zh-CN")
                    .unwrap_or_else(|error| unreachable!("fixture: {error}")),
                translation: "保留".to_owned(),
                provider: "fixture".to_owned(),
                updated_at: UnixTimestamp::from_seconds(100),
            },
            last_queried_at: UnixTimestamp::from_seconds(100),
        }
    }
}
