//! `SQLite` persistence for LVOS Desktop Profiles.

mod installation;
mod model;
mod profile;

pub use installation::{InstallationMetadata, InstallationStore, Platform};
pub use model::{
    Favorite, HistoryEntry, OutboxEvent, OutboxOperation, ProfileMetadata, QueryStats,
    StoredContent, TranslationSnapshot,
};
pub use profile::{BackupArtifact, ProfileDatabase, ProfilePaths, SCHEMA_VERSION};

use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
    InvalidIdentifier(&'static str),
    InvalidData(&'static str),
    MissingHistory,
    Migration {
        version: u32,
        source: rusqlite::Error,
    },
    Backup(Box<StorageError>),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "storage I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "storage JSON failed: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite operation failed: {error}"),
            Self::InvalidIdentifier(kind) => write!(formatter, "invalid {kind} identifier"),
            Self::InvalidData(message) => write!(formatter, "invalid stored data: {message}"),
            Self::MissingHistory => formatter.write_str("favorite content is missing from History"),
            Self::Migration { version, source } => {
                write!(formatter, "migration {version} failed: {source}")
            }
            Self::Backup(error) => write!(formatter, "consistent backup failed: {error}"),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Migration { source, .. } => Some(source),
            Self::Backup(error) => Some(error),
            Self::InvalidIdentifier(_) | Self::InvalidData(_) | Self::MissingHistory => None,
        }
    }
}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
