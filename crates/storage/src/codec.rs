//! Shared codecs for values crossing the `SQLite` boundary.

use std::str::FromStr;

use lvos_core::ContentKey;

use crate::{OutboxOperation, StorageError};

pub(crate) fn parse_content_key(value: &str) -> Result<ContentKey, StorageError> {
    ContentKey::from_str(value).map_err(|_| StorageError::InvalidData("content key"))
}

pub(crate) fn parse_outbox_operation(value: &str) -> Result<OutboxOperation, StorageError> {
    match value {
        "favorite_upsert" => Ok(OutboxOperation::FavoriteUpsert),
        "favorite_delete" => Ok(OutboxOperation::FavoriteDelete),
        "query_stats_upsert" => Ok(OutboxOperation::QueryStatsUpsert),
        _ => Err(StorageError::InvalidData("outbox operation")),
    }
}

pub(crate) fn sqlite_u64(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidData("negative integer"))
}

pub(crate) fn sqlite_i64(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::InvalidData("integer exceeds SQLite range"))
}
