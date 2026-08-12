//! Synchronization contracts. Network and persistence implementations arrive later.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRevision(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncRevision(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncOperation {
    FavoriteUpsert,
    FavoriteDelete,
    QueryStatsUpsert,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncError {
    Offline,
    Unauthorized,
    DeviceRevoked,
    Conflict,
    InvalidResponse,
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Offline => "the sync server is offline",
            Self::Unauthorized => "the sync session is unauthorized",
            Self::DeviceRevoked => "the current device is revoked",
            Self::Conflict => "the favorite revision conflicts with the server",
            Self::InvalidResponse => "the sync server returned an invalid response",
        })
    }
}

impl Error for SyncError {}
