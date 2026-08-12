use lvos_core::{ContentKey, LanguageCode, TextKind, UnixTimestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredContent {
    pub content_key: ContentKey,
    pub key_version: u32,
    pub kind: TextKind,
    pub source_lang: LanguageCode,
    pub source_text: String,
    pub canonical_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationSnapshot {
    pub target_lang: LanguageCode,
    pub translation: String,
    pub provider: String,
    pub updated_at: UnixTimestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub content: StoredContent,
    pub translation: TranslationSnapshot,
    pub last_queried_at: UnixTimestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryStats {
    pub device_query_count: u64,
    pub first_queried_at: UnixTimestamp,
    pub last_queried_at: UnixTimestamp,
    pub last_synced_device_query_count: u64,
    pub server_total_query_count: u64,
    pub server_first_queried_at: Option<UnixTimestamp>,
    pub server_last_queried_at: Option<UnixTimestamp>,
    pub server_snapshot_at: Option<UnixTimestamp>,
}

impl QueryStats {
    #[must_use]
    pub fn effective_total(self) -> u64 {
        if self.server_snapshot_at.is_none() {
            self.device_query_count
        } else {
            self.server_total_query_count.saturating_add(
                self.device_query_count
                    .saturating_sub(self.last_synced_device_query_count),
            )
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Favorite {
    pub content: StoredContent,
    pub translation: TranslationSnapshot,
    pub created_at: UnixTimestamp,
    pub updated_at: UnixTimestamp,
    pub deleted_at: Option<UnixTimestamp>,
    pub entity_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileMetadata {
    pub profile_id: Uuid,
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    pub device_id: Uuid,
    pub platform: String,
    pub server_origin: Option<String>,
    pub last_server_revision: u64,
    pub created_at: UnixTimestamp,
    pub updated_at: UnixTimestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxOperation {
    FavoriteUpsert,
    FavoriteDelete,
    QueryStatsUpsert,
}

impl OutboxOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FavoriteUpsert => "favorite_upsert",
            Self::FavoriteDelete => "favorite_delete",
            Self::QueryStatsUpsert => "query_stats_upsert",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxEvent {
    pub event_id: Uuid,
    pub content_key: ContentKey,
    pub operation: OutboxOperation,
    pub payload_json: String,
    pub coalesce_key: String,
    pub base_entity_revision: Option<u64>,
    pub created_at: UnixTimestamp,
    pub updated_at: UnixTimestamp,
    pub attempt_count: u32,
    pub next_retry_at: Option<UnixTimestamp>,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct QueryStatsPayload<'a> {
    pub content_key: &'a str,
    pub device_query_count: u64,
    pub first_queried_at: i64,
    pub last_queried_at: i64,
}

#[derive(Serialize)]
pub(crate) struct FavoritePayload<'a> {
    pub content_key: &'a str,
    pub desired_state: &'a str,
    pub base_entity_revision: u64,
    pub query_stats: QueryStatsPayload<'a>,
}
