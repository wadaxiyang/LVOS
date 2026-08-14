use std::{
    fmt,
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use uuid::Uuid;

const SERVER_SCHEMA_V1: &str = r"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS users (
    user_id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    sync_revision INTEGER NOT NULL DEFAULT 0 CHECK(sync_revision >= 0),
    created_at INTEGER NOT NULL,
    disabled_at INTEGER NULL
);
CREATE TABLE IF NOT EXISTS devices (
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    platform TEXT NOT NULL CHECK(platform IN ('windows', 'macos')),
    device_name TEXT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    PRIMARY KEY (user_id, device_id),
    FOREIGN KEY (user_id) REFERENCES users(user_id)
);
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    access_token_hash TEXT NOT NULL UNIQUE,
    refresh_token_hash TEXT NOT NULL UNIQUE,
    access_expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_refreshed_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    FOREIGN KEY (user_id, device_id) REFERENCES devices(user_id, device_id)
);
CREATE INDEX IF NOT EXISTS idx_sessions_user_device
    ON sessions(user_id, device_id);
CREATE INDEX IF NOT EXISTS idx_sessions_refresh_hash
    ON sessions(refresh_token_hash);
";

#[derive(Clone)]
pub struct ServerRepository {
    connection: Arc<Mutex<Connection>>,
}

impl fmt::Debug for ServerRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerRepository")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UserCredential {
    pub user_id: String,
    pub username: String,
    pub password_hash: String,
    pub sync_revision: i64,
    pub disabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Principal {
    pub session_id: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub platform: String,
    pub latest_revision: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionIdentity {
    pub session_id: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub platform: String,
    pub latest_revision: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeviceRecord {
    pub device_id: String,
    pub platform: String,
    pub device_name: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub revoked_at: Option<i64>,
}

impl ServerRepository {
    /// Opens a persistent `SQLite` repository and applies the Stage 08 identity schema.
    ///
    /// # Errors
    /// Returns a database error when the URL is invalid or `SQLite` initialization fails.
    pub fn open(database_url: &str, now: i64) -> Result<Self, RepositoryError> {
        let path = sqlite_path(database_url)?;
        if path != Path::new(":memory:")
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|_| RepositoryError::Database)?;
        }
        let connection = Connection::open(path).map_err(|_| RepositoryError::Database)?;
        Self::from_connection(connection, now)
    }

    /// Creates a shared in-memory repository for integration tests.
    ///
    /// # Errors
    /// Returns a database error if the schema cannot be initialized.
    pub fn in_memory(now: i64) -> Result<Self, RepositoryError> {
        let connection = Connection::open_in_memory().map_err(|_| RepositoryError::Database)?;
        Self::from_connection(connection, now)
    }

    fn from_connection(connection: Connection, now: i64) -> Result<Self, RepositoryError> {
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(|_| RepositoryError::Database)?;
        connection
            .execute_batch(SERVER_SCHEMA_V1)
            .map_err(|_| RepositoryError::Database)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, name, applied_at) VALUES(1, 'server_identity_auth', ?1)",
                [now],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn bootstrap_user(
        &self,
        username: &str,
        password_hash: &str,
        now: i64,
    ) -> Result<String, RepositoryError> {
        let connection = self.lock()?;
        if let Some(user_id) = connection
            .query_row(
                "SELECT user_id FROM users WHERE username = ?1",
                [username],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| RepositoryError::Database)?
        {
            return Ok(user_id);
        }
        let user_id = Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO users(user_id, username, password_hash, created_at) VALUES(?1, ?2, ?3, ?4)",
                params![user_id, username, password_hash, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(user_id)
    }

    pub(crate) fn user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserCredential>, RepositoryError> {
        self.lock()?
            .query_row(
                "SELECT user_id, username, password_hash, sync_revision, disabled_at IS NOT NULL FROM users WHERE username = ?1",
                [username],
                |row| {
                    Ok(UserCredential {
                        user_id: row.get(0)?,
                        username: row.get(1)?,
                        password_hash: row.get(2)?,
                        sync_revision: row.get(3)?,
                        disabled: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|_| RepositoryError::Database)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_session(
        &self,
        user: &UserCredential,
        device_id: &str,
        platform: &str,
        device_name: Option<&str>,
        access_hash: &str,
        refresh_hash: &str,
        access_expires_at: i64,
        now: i64,
    ) -> Result<SessionIdentity, RepositoryError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|_| RepositoryError::Database)?;
        let revoked_at: Option<Option<i64>> = transaction
            .query_row(
                "SELECT revoked_at FROM devices WHERE user_id = ?1 AND device_id = ?2",
                params![user.user_id, device_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| RepositoryError::Database)?;
        match revoked_at {
            Some(Some(_)) => return Err(RepositoryError::DeviceRevoked),
            Some(None) => {
                transaction
                    .execute(
                        "UPDATE devices SET platform = ?3, device_name = ?4, last_seen_at = ?5 WHERE user_id = ?1 AND device_id = ?2",
                        params![user.user_id, device_id, platform, device_name, now],
                    )
                    .map_err(|_| RepositoryError::Database)?;
            }
            None => {
                transaction
                    .execute(
                        "INSERT INTO devices(user_id, device_id, platform, device_name, created_at, last_seen_at) VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
                        params![user.user_id, device_id, platform, device_name, now],
                    )
                    .map_err(|_| RepositoryError::Database)?;
            }
        }
        let session_id = Uuid::new_v4().to_string();
        transaction
            .execute(
                "INSERT INTO sessions(session_id, user_id, device_id, access_token_hash, refresh_token_hash, access_expires_at, created_at, last_refreshed_at, last_seen_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7)",
                params![session_id, user.user_id, device_id, access_hash, refresh_hash, access_expires_at, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        transaction
            .commit()
            .map_err(|_| RepositoryError::Database)?;
        Ok(SessionIdentity {
            session_id,
            user_id: user.user_id.clone(),
            username: user.username.clone(),
            device_id: device_id.to_owned(),
            platform: platform.to_owned(),
            latest_revision: user.sync_revision,
        })
    }

    pub(crate) fn authenticate_access(
        &self,
        access_hash: &str,
        now: i64,
    ) -> Result<Principal, RepositoryError> {
        let connection = self.lock()?;
        let row = connection
            .query_row(
                "SELECT s.session_id, u.user_id, u.username, d.device_id, d.platform, u.sync_revision, s.access_expires_at, s.revoked_at, d.revoked_at, u.disabled_at FROM sessions s JOIN users u ON u.user_id = s.user_id JOIN devices d ON d.user_id = s.user_id AND d.device_id = s.device_id WHERE s.access_token_hash = ?1",
                [access_hash],
                |row| {
                    Ok((
                        Principal {
                            session_id: row.get(0)?,
                            user_id: row.get(1)?,
                            username: row.get(2)?,
                            device_id: row.get(3)?,
                            platform: row.get(4)?,
                            latest_revision: row.get(5)?,
                        },
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| RepositoryError::Database)?
            .ok_or(RepositoryError::SessionInvalid)?;
        if row.4.is_some() {
            return Err(RepositoryError::UserDisabled);
        }
        if row.3.is_some() {
            return Err(RepositoryError::DeviceRevoked);
        }
        if row.2.is_some() {
            return Err(RepositoryError::SessionRevoked);
        }
        if row.1 <= now {
            return Err(RepositoryError::AccessExpired);
        }
        connection
            .execute(
                "UPDATE sessions SET last_seen_at = ?2 WHERE session_id = ?1",
                params![row.0.session_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        connection
            .execute(
                "UPDATE devices SET last_seen_at = ?3 WHERE user_id = ?1 AND device_id = ?2",
                params![row.0.user_id, row.0.device_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(row.0)
    }

    pub(crate) fn rotate_refresh(
        &self,
        refresh_hash: &str,
        new_access_hash: &str,
        new_refresh_hash: &str,
        access_expires_at: i64,
        refresh_idle_ttl_seconds: i64,
        now: i64,
    ) -> Result<SessionIdentity, RepositoryError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|_| RepositoryError::Database)?;
        let row =
            refresh_identity(&transaction, refresh_hash)?.ok_or(RepositoryError::SessionInvalid)?;
        if row.user_disabled {
            return Err(RepositoryError::UserDisabled);
        }
        if row.device_revoked {
            return Err(RepositoryError::DeviceRevoked);
        }
        if row.session_revoked {
            return Err(RepositoryError::SessionRevoked);
        }
        if now.saturating_sub(row.last_seen_at) >= refresh_idle_ttl_seconds {
            transaction
                .execute(
                    "UPDATE sessions SET revoked_at = ?2 WHERE session_id = ?1",
                    params![row.identity.session_id, now],
                )
                .map_err(|_| RepositoryError::Database)?;
            transaction
                .commit()
                .map_err(|_| RepositoryError::Database)?;
            return Err(RepositoryError::RefreshExpired);
        }
        transaction
            .execute(
                "UPDATE sessions SET access_token_hash = ?2, refresh_token_hash = ?3, access_expires_at = ?4, last_refreshed_at = ?5, last_seen_at = ?5 WHERE session_id = ?1",
                params![row.identity.session_id, new_access_hash, new_refresh_hash, access_expires_at, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        transaction
            .execute(
                "UPDATE devices SET last_seen_at = ?3 WHERE user_id = ?1 AND device_id = ?2",
                params![row.identity.user_id, row.identity.device_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        transaction
            .commit()
            .map_err(|_| RepositoryError::Database)?;
        Ok(row.identity)
    }

    pub(crate) fn revoke_session(&self, session_id: &str, now: i64) -> Result<(), RepositoryError> {
        self.lock()?
            .execute(
                "UPDATE sessions SET revoked_at = COALESCE(revoked_at, ?2) WHERE session_id = ?1",
                params![session_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(())
    }

    pub(crate) fn devices_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<DeviceRecord>, RepositoryError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT device_id, platform, device_name, created_at, last_seen_at, revoked_at FROM devices WHERE user_id = ?1 ORDER BY created_at, device_id")
            .map_err(|_| RepositoryError::Database)?;
        statement
            .query_map([user_id], |row| {
                Ok(DeviceRecord {
                    device_id: row.get(0)?,
                    platform: row.get(1)?,
                    device_name: row.get(2)?,
                    created_at: row.get(3)?,
                    last_seen_at: row.get(4)?,
                    revoked_at: row.get(5)?,
                })
            })
            .map_err(|_| RepositoryError::Database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RepositoryError::Database)
    }

    pub(crate) fn revoke_device(
        &self,
        user_id: &str,
        device_id: &str,
        now: i64,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|_| RepositoryError::Database)?;
        let changed = transaction
            .execute(
                "UPDATE devices SET revoked_at = COALESCE(revoked_at, ?3) WHERE user_id = ?1 AND device_id = ?2",
                params![user_id, device_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        if changed == 0 {
            return Err(RepositoryError::NotFound);
        }
        transaction
            .execute(
                "UPDATE sessions SET revoked_at = COALESCE(revoked_at, ?3) WHERE user_id = ?1 AND device_id = ?2",
                params![user_id, device_id, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        transaction.commit().map_err(|_| RepositoryError::Database)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, RepositoryError> {
        self.connection
            .lock()
            .map_err(|_| RepositoryError::Database)
    }

    #[cfg(test)]
    pub(crate) fn raw_session_contains(&self, value: &str) -> Result<bool, RepositoryError> {
        let count: i64 = self
            .lock()?
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE access_token_hash = ?1 OR refresh_token_hash = ?1",
                [value],
                |row| row.get(0),
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(count != 0)
    }

    #[cfg(test)]
    pub(crate) fn expire_access_hash(
        &self,
        access_hash: &str,
        expired_at: i64,
    ) -> Result<(), RepositoryError> {
        self.lock()?
            .execute(
                "UPDATE sessions SET access_expires_at = ?2 WHERE access_token_hash = ?1",
                params![access_hash, expired_at],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn disable_user(&self, username: &str, now: i64) -> Result<(), RepositoryError> {
        self.lock()?
            .execute(
                "UPDATE users SET disabled_at = ?2 WHERE username = ?1",
                params![username, now],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_refresh_last_seen(
        &self,
        refresh_hash: &str,
        last_seen_at: i64,
    ) -> Result<(), RepositoryError> {
        self.lock()?
            .execute(
                "UPDATE sessions SET last_seen_at = ?2 WHERE refresh_token_hash = ?1",
                params![refresh_hash, last_seen_at],
            )
            .map_err(|_| RepositoryError::Database)?;
        Ok(())
    }
}

struct RefreshRow {
    identity: SessionIdentity,
    last_seen_at: i64,
    session_revoked: bool,
    device_revoked: bool,
    user_disabled: bool,
}

fn refresh_identity(
    transaction: &Transaction<'_>,
    refresh_hash: &str,
) -> Result<Option<RefreshRow>, RepositoryError> {
    transaction
        .query_row(
            "SELECT s.session_id, u.user_id, u.username, d.device_id, d.platform, u.sync_revision, s.last_seen_at, s.revoked_at IS NOT NULL, d.revoked_at IS NOT NULL, u.disabled_at IS NOT NULL FROM sessions s JOIN users u ON u.user_id = s.user_id JOIN devices d ON d.user_id = s.user_id AND d.device_id = s.device_id WHERE s.refresh_token_hash = ?1",
            [refresh_hash],
            |row| {
                Ok(RefreshRow {
                    identity: SessionIdentity {
                        session_id: row.get(0)?,
                        user_id: row.get(1)?,
                        username: row.get(2)?,
                        device_id: row.get(3)?,
                        platform: row.get(4)?,
                        latest_revision: row.get(5)?,
                    },
                    last_seen_at: row.get(6)?,
                    session_revoked: row.get(7)?,
                    device_revoked: row.get(8)?,
                    user_disabled: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|_| RepositoryError::Database)
}

fn sqlite_path(database_url: &str) -> Result<&Path, RepositoryError> {
    let value = database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url);
    if value.is_empty() {
        return Err(RepositoryError::Database);
    }
    Ok(Path::new(value))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryError {
    Database,
    NotFound,
    SessionInvalid,
    AccessExpired,
    RefreshExpired,
    SessionRevoked,
    DeviceRevoked,
    UserDisabled,
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Database => "the authentication repository operation failed",
            Self::NotFound => "the requested resource was not found",
            Self::SessionInvalid => "the authentication session is invalid",
            Self::AccessExpired => "the access token expired",
            Self::RefreshExpired => "the refresh session expired due to inactivity",
            Self::SessionRevoked => "the authentication session was revoked",
            Self::DeviceRevoked => "the device was revoked",
            Self::UserDisabled => "the user is disabled",
        })
    }
}

impl std::error::Error for RepositoryError {}
