//! Versioned, transport-neutral synchronization protocol contracts.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRevision(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncRevision(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperation {
    FavoriteUpsert,
    FavoriteDelete,
    QueryStatsUpsert,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteSnapshot {
    pub kind: String,
    pub source_lang: String,
    pub target_lang: String,
    pub source_text: String,
    pub canonical_text: String,
    pub translation: String,
    pub provider: String,
    pub favorited_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueryStatsSnapshot {
    pub query_count: u64,
    pub first_queried_at: i64,
    pub last_queried_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PushEvent {
    pub event_id: String,
    pub operation: SyncOperation,
    pub content_key: String,
    pub key_version: u32,
    pub base_entity_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite: Option<FavoriteSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_stats: Option<QueryStatsSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PushRequest {
    pub events: Vec<PushEvent>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    Applied,
    NoChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AggregateQueryStats {
    pub query_count: u64,
    pub first_queried_at: i64,
    pub last_queried_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PushAck {
    pub event_id: String,
    pub status: AckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_revision: Option<u64>,
    pub user_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_query_stats: Option<AggregateQueryStats>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PushResponse {
    pub acknowledgements: Vec<PushAck>,
    pub latest_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteRecord {
    pub content_key: String,
    pub key_version: u32,
    pub favorite: FavoriteSnapshot,
    pub deleted_at: Option<i64>,
    pub entity_revision: u64,
    pub server_received_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_query_stats: Option<AggregateQueryStats>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncChange {
    pub revision: u64,
    pub operation: SyncOperation,
    pub favorite: FavoriteRecord,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChangesResponse {
    pub changes: Vec<SyncChange>,
    pub next_revision: u64,
    pub latest_revision: u64,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoritesResponse {
    pub favorites: Vec<FavoriteRecord>,
    pub latest_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteStateRequest {
    pub active: bool,
    pub base_entity_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteStateResponse {
    pub favorite: FavoriteRecord,
    pub latest_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionNotice {
    pub latest_revision: u64,
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
