use std::{num::NonZeroUsize, str::FromStr};

use lvos_core::{ContentKey, LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_sync::{
    AggregateQueryStats, FavoriteRecord, FavoriteSnapshot, PushAck, PushEvent, QueryStatsSnapshot,
    SyncChange, SyncOperation,
};
use rusqlite::{OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::{
    Favorite, OutboxEvent, OutboxOperation, ProfileDatabase, StorageError, StoredContent,
    SyncDiagnostics, TranslationSnapshot,
};

const MAX_REMOTE_CONTENT_CHARACTERS: usize = 16_384;
const CONFLICT_ERROR: &str = "favorite_conflict";

#[derive(Clone, Debug)]
pub struct AcknowledgedEvent {
    pub event: OutboxEvent,
    pub acknowledgement: PushAck,
    pub sent_query_count: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictResolution {
    Converged,
    ReplayPrepared,
    ManualRetryRequired,
}

impl ProfileDatabase {
    /// Lists a bounded retry-ready Outbox batch without loading the complete queue.
    ///
    /// # Errors
    /// Returns an error for malformed persisted rows or `SQLite` failure.
    pub fn ready_outbox_events(
        &self,
        now: UnixTimestamp,
        limit: u32,
    ) -> Result<Vec<OutboxEvent>, StorageError> {
        if limit == 0 {
            return Err(StorageError::InvalidData("Outbox batch limit is zero"));
        }
        let mut statement = self.connection.prepare(
            "SELECT event_id, content_key, operation, payload_json, coalesce_key,
             base_entity_revision, created_at, updated_at, attempt_count, next_retry_at,
             last_error, conflict_replay_count FROM sync_outbox
             WHERE (next_retry_at IS NULL OR next_retry_at <= ?1)
               AND (last_error IS NULL OR last_error != ?2)
             ORDER BY created_at, event_id LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![now.as_seconds(), CONFLICT_ERROR, i64::from(limit)],
            read_outbox,
        )?;
        rows.map(|row| row.map_err(StorageError::from).and_then(parse_outbox))
            .collect()
    }

    /// Converts a persisted Outbox intent into the current V1 protocol snapshot.
    ///
    /// # Errors
    /// Returns an error when the referenced local Favorite or `QueryStats` row is missing.
    pub fn push_event(&self, event: &OutboxEvent) -> Result<PushEvent, StorageError> {
        let favorite = self.favorite_by_key(event.content_key)?;
        let stats = self.query_stats(event.content_key)?;
        let query_stats = stats.and_then(|value| {
            (value.device_query_count > 0).then(|| QueryStatsSnapshot {
                query_count: value.device_query_count,
                first_queried_at: value.first_queried_at.as_seconds(),
                last_queried_at: value.last_queried_at.as_seconds(),
                updated_at: value.last_queried_at.as_seconds(),
            })
        });
        let favorite_snapshot = match event.operation {
            OutboxOperation::FavoriteUpsert => {
                let value = favorite
                    .as_ref()
                    .filter(|value| value.deleted_at.is_none())
                    .ok_or(StorageError::InvalidData("active Favorite is missing"))?;
                Some(protocol_snapshot(value))
            }
            OutboxOperation::FavoriteDelete | OutboxOperation::QueryStatsUpsert => None,
        };
        if event.operation == OutboxOperation::QueryStatsUpsert && query_stats.is_none() {
            return Err(StorageError::InvalidData("sync QueryStats are missing"));
        }
        Ok(PushEvent {
            event_id: event.event_id.to_string(),
            operation: protocol_operation(event.operation),
            content_key: event.content_key.to_string(),
            key_version: favorite
                .as_ref()
                .map_or(lvos_core::CONTENT_KEY_VERSION, |value| {
                    value.content.key_version
                }),
            base_entity_revision: event.base_entity_revision.unwrap_or(0),
            favorite: favorite_snapshot,
            query_stats,
        })
    }

    /// Applies exact event acknowledgements without deleting a newer coalesced local intent.
    ///
    /// # Errors
    /// Returns an error if an acknowledgement is inconsistent or the transaction cannot commit.
    pub fn acknowledge_events(
        &mut self,
        acknowledged: &[AcknowledgedEvent],
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        for item in acknowledged {
            if item.acknowledgement.event_id != item.event.event_id.to_string() {
                return Err(StorageError::InvalidData("acknowledgement event mismatch"));
            }
            if let Some(revision) = item.acknowledgement.entity_revision {
                transaction.execute(
                    "UPDATE favorites SET entity_revision=?2 WHERE content_key=?1",
                    params![item.event.content_key.to_string(), to_i64(revision)?],
                )?;
                transaction.execute(
                    "UPDATE sync_outbox SET base_entity_revision=?2
                     WHERE content_key=?1 AND event_id != ?3
                       AND operation IN ('favorite_upsert','favorite_delete')",
                    params![
                        item.event.content_key.to_string(),
                        to_i64(revision)?,
                        item.event.event_id.to_string()
                    ],
                )?;
            }
            if let Some(aggregate) = &item.acknowledgement.aggregate_query_stats {
                update_aggregate(
                    &transaction,
                    item.event.content_key,
                    aggregate,
                    item.sent_query_count,
                    now,
                )?;
            }
            transaction.execute(
                "DELETE FROM sync_outbox WHERE event_id=?1",
                [item.event.event_id.to_string()],
            )?;
        }
        transaction.execute(
            "UPDATE profile_meta SET last_successful_sync_at=?1,last_sync_error=NULL,updated_at=?1
             WHERE singleton_id=1",
            [now.as_seconds()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Persists bounded retry metadata for a failed batch.
    ///
    /// # Errors
    /// Returns an error if the retry metadata cannot be written atomically.
    pub fn mark_retry(
        &mut self,
        events: &[OutboxEvent],
        next_retry_at: UnixTimestamp,
        error: &str,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        for event in events {
            transaction.execute(
                "UPDATE sync_outbox SET attempt_count=attempt_count+1,next_retry_at=?2,
                 last_error=?3,updated_at=?4 WHERE event_id=?1",
                params![
                    event.event_id.to_string(),
                    next_retry_at.as_seconds(),
                    bounded_error(error),
                    now.as_seconds()
                ],
            )?;
        }
        transaction.execute(
            "UPDATE profile_meta SET last_sync_error=?1,updated_at=?2 WHERE singleton_id=1",
            params![bounded_error(error), now.as_seconds()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Applies one bounded page and advances the durable cursor in the same transaction.
    ///
    /// Remote application never creates an Outbox event.
    ///
    /// # Errors
    /// Returns an error without advancing the cursor when any change is invalid.
    pub fn apply_remote_page(
        &mut self,
        changes: &[SyncChange],
        next_revision: u64,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        let current_cursor: i64 = transaction.query_row(
            "SELECT last_server_revision FROM profile_meta WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )?;
        if next_revision < to_u64(current_cursor)? {
            return Err(StorageError::InvalidData("remote cursor moved backwards"));
        }
        let mut expected = to_u64(current_cursor)?;
        for change in changes {
            let next_expected = expected
                .checked_add(1)
                .ok_or(StorageError::InvalidData("remote revision overflow"))?;
            if change.revision != next_expected || change.revision > next_revision {
                return Err(StorageError::InvalidData(
                    "remote change revision is invalid",
                ));
            }
            apply_remote_change(&transaction, change, now)?;
            expected = change.revision;
        }
        if expected != next_revision {
            return Err(StorageError::InvalidData(
                "remote page cursor is inconsistent",
            ));
        }
        transaction.execute(
            "UPDATE profile_meta SET last_server_revision=?1,last_successful_sync_at=?2,
             last_sync_error=NULL,updated_at=?2 WHERE singleton_id=1",
            params![to_i64(next_revision)?, now.as_seconds()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Rebases the last local Favorite intent at most once against a Server conflict.
    ///
    /// # Errors
    /// Returns an error when the conflict does not identify a valid pending Favorite event.
    pub fn resolve_conflict(
        &mut self,
        event: &OutboxEvent,
        current: Option<&FavoriteRecord>,
        now: UnixTimestamp,
    ) -> Result<ConflictResolution, StorageError> {
        if !matches!(
            event.operation,
            OutboxOperation::FavoriteUpsert | OutboxOperation::FavoriteDelete
        ) {
            return Err(StorageError::InvalidData(
                "QueryStats cannot have a Favorite conflict",
            ));
        }
        let transaction = self.connection.transaction()?;
        let local = local_favorite(&transaction, event.content_key)?
            .ok_or(StorageError::InvalidData("conflicted Favorite is missing"))?;
        if current.is_some_and(|remote| desired_converged(event.operation, &local, remote)) {
            if let Some(remote) = current {
                write_remote_favorite(&transaction, remote, now)?;
            }
            transaction.execute(
                "DELETE FROM sync_outbox WHERE coalesce_key=?1",
                [format!("favorite:{}", event.content_key)],
            )?;
            transaction.commit()?;
            return Ok(ConflictResolution::Converged);
        }
        let Some(remote) = current else {
            mark_manual_conflict(&transaction, event.content_key, now)?;
            transaction.commit()?;
            return Ok(ConflictResolution::ManualRetryRequired);
        };
        let replay_count: i64 = transaction
            .query_row(
                "SELECT conflict_replay_count FROM sync_outbox WHERE coalesce_key=?1",
                [format!("favorite:{}", event.content_key)],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::InvalidData(
                "conflicted Outbox intent is missing",
            ))?;
        update_aggregate_from_record(&transaction, remote, now)?;
        if replay_count == 0 {
            transaction.execute(
                "UPDATE favorites SET entity_revision=?2 WHERE content_key=?1",
                params![
                    event.content_key.to_string(),
                    to_i64(remote.entity_revision)?
                ],
            )?;
            transaction.execute(
                "UPDATE sync_outbox SET base_entity_revision=?2,conflict_replay_count=1,
                 attempt_count=0,next_retry_at=NULL,last_error=NULL,updated_at=?3
                 WHERE coalesce_key=?1",
                params![
                    format!("favorite:{}", event.content_key),
                    to_i64(remote.entity_revision)?,
                    now.as_seconds()
                ],
            )?;
            transaction.commit()?;
            Ok(ConflictResolution::ReplayPrepared)
        } else {
            mark_manual_conflict(&transaction, event.content_key, now)?;
            transaction.commit()?;
            Ok(ConflictResolution::ManualRetryRequired)
        }
    }

    /// Persists current SSE connectivity for the Sync settings surface.
    ///
    /// # Errors
    /// Returns an error when the Profile metadata cannot be updated.
    pub fn set_sse_connected(
        &mut self,
        connected: bool,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE profile_meta SET sse_connected=?1,updated_at=?2 WHERE singleton_id=1",
            params![connected, now.as_seconds()],
        )?;
        Ok(())
    }

    /// Persists a bounded user-visible synchronization error without changing Outbox retry state.
    ///
    /// # Errors
    /// Returns an error when the Profile metadata cannot be updated.
    pub fn set_sync_error(&mut self, error: &str, now: UnixTimestamp) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE profile_meta SET last_sync_error=?1,updated_at=?2 WHERE singleton_id=1",
            params![bounded_error(error), now.as_seconds()],
        )?;
        Ok(())
    }

    /// Returns persisted user-visible sync diagnostics.
    ///
    /// # Errors
    /// Returns an error for malformed metadata or `SQLite` failure.
    pub fn sync_diagnostics(&self) -> Result<SyncDiagnostics, StorageError> {
        let row = self.connection.query_row(
            "SELECT last_server_revision,last_successful_sync_at,last_sync_error,sse_connected,
             (SELECT COUNT(*) FROM sync_outbox) FROM profile_meta WHERE singleton_id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;
        Ok(SyncDiagnostics {
            last_server_revision: to_u64(row.0)?,
            pending_outbox: to_u64(row.4)?,
            last_successful_sync_at: row.1.map(UnixTimestamp::from_seconds),
            last_error: row.2,
            sse_connected: row.3,
        })
    }
}

type OutboxRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    i64,
);

fn read_outbox(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn parse_outbox(row: OutboxRow) -> Result<OutboxEvent, StorageError> {
    Ok(OutboxEvent {
        event_id: Uuid::parse_str(&row.0).map_err(|_| StorageError::InvalidIdentifier("event"))?,
        content_key: parse_key(&row.1)?,
        operation: parse_operation(&row.2)?,
        payload_json: row
            .3
            .ok_or(StorageError::InvalidData("outbox payload is null"))?,
        coalesce_key: row
            .4
            .ok_or(StorageError::InvalidData("outbox coalesce key is null"))?,
        base_entity_revision: row.5.map(to_u64).transpose()?,
        created_at: UnixTimestamp::from_seconds(row.6),
        updated_at: UnixTimestamp::from_seconds(row.7),
        attempt_count: u32::try_from(row.8)
            .map_err(|_| StorageError::InvalidData("negative attempt count"))?,
        next_retry_at: row.9.map(UnixTimestamp::from_seconds),
        last_error: row.10,
        conflict_replay_count: u32::try_from(row.11)
            .map_err(|_| StorageError::InvalidData("negative conflict replay count"))?,
    })
}

fn apply_remote_change(
    transaction: &Transaction<'_>,
    change: &SyncChange,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    validate_remote(&change.favorite)?;
    let key = parse_key(&change.favorite.content_key)?;
    let pending = transaction
        .query_row(
            "SELECT operation FROM sync_outbox WHERE coalesce_key=?1",
            [format!("favorite:{key}")],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(operation) = pending {
        let operation = parse_operation(&operation)?;
        let local = local_favorite(transaction, key)?
            .ok_or(StorageError::InvalidData("pending Favorite is missing"))?;
        if desired_converged(operation, &local, &change.favorite) {
            write_remote_favorite(transaction, &change.favorite, now)?;
            transaction.execute(
                "DELETE FROM sync_outbox WHERE coalesce_key=?1",
                [format!("favorite:{key}")],
            )?;
        } else {
            transaction.execute(
                "UPDATE favorites SET entity_revision=?2 WHERE content_key=?1",
                params![key.to_string(), to_i64(change.favorite.entity_revision)?],
            )?;
            transaction.execute(
                "UPDATE sync_outbox SET base_entity_revision=?2,updated_at=?3
                 WHERE coalesce_key=?1",
                params![
                    format!("favorite:{key}"),
                    to_i64(change.favorite.entity_revision)?,
                    now.as_seconds()
                ],
            )?;
            update_aggregate_from_record(transaction, &change.favorite, now)?;
        }
    } else {
        write_remote_favorite(transaction, &change.favorite, now)?;
    }
    Ok(())
}

fn write_remote_favorite(
    transaction: &Transaction<'_>,
    remote: &FavoriteRecord,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    let key = parse_key(&remote.content_key)?;
    transaction.execute(
        "INSERT INTO favorites(content_key,key_version,kind,source_lang,target_lang,source_text,
         canonical_text,translation,provider,created_at,updated_at,deleted_at,entity_revision)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
         ON CONFLICT(content_key) DO UPDATE SET key_version=excluded.key_version,
         kind=excluded.kind,source_lang=excluded.source_lang,target_lang=excluded.target_lang,
         source_text=excluded.source_text,canonical_text=excluded.canonical_text,
         translation=excluded.translation,provider=excluded.provider,created_at=excluded.created_at,
         updated_at=excluded.updated_at,deleted_at=excluded.deleted_at,
         entity_revision=excluded.entity_revision",
        params![
            remote.content_key,
            i64::from(remote.key_version),
            remote.favorite.kind,
            remote.favorite.source_lang,
            remote.favorite.target_lang,
            remote.favorite.source_text,
            remote.favorite.canonical_text,
            remote.favorite.translation,
            remote.favorite.provider,
            remote.favorite.favorited_at,
            remote.favorite.updated_at,
            remote.deleted_at,
            to_i64(remote.entity_revision)?,
        ],
    )?;
    update_aggregate_from_record(transaction, remote, now)?;
    if transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM query_stats WHERE content_key=?1)",
        [key.to_string()],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let baseline = remote
        .aggregate_query_stats
        .as_ref()
        .map_or(remote.favorite.favorited_at, |value| value.first_queried_at);
    transaction.execute(
        "INSERT INTO query_stats(content_key,device_query_count,first_queried_at,last_queried_at)
         VALUES(?1,0,?2,?2)",
        params![key.to_string(), baseline],
    )?;
    update_aggregate_from_record(transaction, remote, now)
}

fn update_aggregate_from_record(
    transaction: &Transaction<'_>,
    remote: &FavoriteRecord,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    if let Some(aggregate) = &remote.aggregate_query_stats {
        update_aggregate(
            transaction,
            parse_key(&remote.content_key)?,
            aggregate,
            None,
            now,
        )?;
    }
    Ok(())
}

fn update_aggregate(
    transaction: &Transaction<'_>,
    key: ContentKey,
    aggregate: &AggregateQueryStats,
    sent_query_count: Option<u64>,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM query_stats WHERE content_key=?1)",
        [key.to_string()],
        |row| row.get(0),
    )?;
    if !exists {
        transaction.execute(
            "INSERT INTO query_stats(content_key,device_query_count,first_queried_at,last_queried_at)
             VALUES(?1,0,?2,?3)",
            params![key.to_string(), aggregate.first_queried_at, aggregate.last_queried_at],
        )?;
    }
    transaction.execute(
        "UPDATE query_stats SET last_synced_device_query_count=MAX(last_synced_device_query_count,?2),
         server_total_query_count=?3,server_first_queried_at=?4,server_last_queried_at=?5,
         server_snapshot_at=?6 WHERE content_key=?1",
        params![
            key.to_string(),
            to_i64(sent_query_count.unwrap_or(0))?,
            to_i64(aggregate.query_count)?,
            aggregate.first_queried_at,
            aggregate.last_queried_at,
            now.as_seconds()
        ],
    )?;
    Ok(())
}

fn local_favorite(
    transaction: &Transaction<'_>,
    key: ContentKey,
) -> Result<Option<Favorite>, StorageError> {
    let row = transaction
        .query_row(
            "SELECT key_version,kind,source_lang,target_lang,source_text,canonical_text,
             translation,provider,created_at,updated_at,deleted_at,entity_revision
             FROM favorites WHERE content_key=?1",
            [key.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            },
        )
        .optional()?;
    row.map(|row| {
        Ok(Favorite {
            content: StoredContent {
                content_key: key,
                key_version: u32::try_from(row.0)
                    .map_err(|_| StorageError::InvalidData("key version"))?,
                kind: match row.1.as_str() {
                    "word" => lvos_core::TextKind::Word,
                    "text" => lvos_core::TextKind::Text,
                    _ => return Err(StorageError::InvalidData("text kind")),
                },
                source_lang: LanguageCode::parse(&row.2)
                    .map_err(|_| StorageError::InvalidData("source language"))?,
                source_text: row.4,
                canonical_text: row.5,
            },
            translation: TranslationSnapshot {
                target_lang: LanguageCode::parse(&row.3)
                    .map_err(|_| StorageError::InvalidData("target language"))?,
                translation: row.6,
                provider: row.7,
                updated_at: UnixTimestamp::from_seconds(row.9),
            },
            created_at: UnixTimestamp::from_seconds(row.8),
            updated_at: UnixTimestamp::from_seconds(row.9),
            deleted_at: row.10.map(UnixTimestamp::from_seconds),
            entity_revision: to_u64(row.11)?,
        })
    })
    .transpose()
}

fn desired_converged(
    operation: OutboxOperation,
    local: &Favorite,
    remote: &FavoriteRecord,
) -> bool {
    match operation {
        OutboxOperation::FavoriteDelete => remote.deleted_at.is_some(),
        OutboxOperation::FavoriteUpsert => {
            remote.deleted_at.is_none() && protocol_snapshot(local) == remote.favorite
        }
        OutboxOperation::QueryStatsUpsert => false,
    }
}

fn validate_remote(remote: &FavoriteRecord) -> Result<(), StorageError> {
    if remote.key_version != lvos_core::CONTENT_KEY_VERSION || remote.entity_revision == 0 {
        return Err(StorageError::InvalidData(
            "remote Favorite version is invalid",
        ));
    }
    let source_lang = LanguageCode::parse(&remote.favorite.source_lang)
        .map_err(|_| StorageError::InvalidData("remote source language"))?;
    LanguageCode::parse(&remote.favorite.target_lang)
        .map_err(|_| StorageError::InvalidData("remote target language"))?;
    let policy = ValidationPolicy::new(
        NonZeroUsize::new(MAX_REMOTE_CONTENT_CHARACTERS).unwrap_or(NonZeroUsize::MIN),
    );
    let prepared = prepare_content(&remote.favorite.source_text, source_lang, policy)
        .map_err(|_| StorageError::InvalidData("remote source content"))?;
    if prepared.content_key().to_string() != remote.content_key
        || prepared.canonical_text() != remote.favorite.canonical_text
        || prepared.kind().protocol_name() != remote.favorite.kind
    {
        return Err(StorageError::InvalidData(
            "remote content identity mismatch",
        ));
    }
    Ok(())
}

fn protocol_snapshot(favorite: &Favorite) -> FavoriteSnapshot {
    FavoriteSnapshot {
        kind: favorite.content.kind.protocol_name().to_owned(),
        source_lang: favorite.content.source_lang.to_string(),
        target_lang: favorite.translation.target_lang.to_string(),
        source_text: favorite.content.source_text.clone(),
        canonical_text: favorite.content.canonical_text.clone(),
        translation: favorite.translation.translation.clone(),
        provider: favorite.translation.provider.clone(),
        favorited_at: favorite.created_at.as_seconds(),
        updated_at: favorite.updated_at.as_seconds(),
    }
}

fn protocol_operation(operation: OutboxOperation) -> SyncOperation {
    match operation {
        OutboxOperation::FavoriteUpsert => SyncOperation::FavoriteUpsert,
        OutboxOperation::FavoriteDelete => SyncOperation::FavoriteDelete,
        OutboxOperation::QueryStatsUpsert => SyncOperation::QueryStatsUpsert,
    }
}

fn parse_operation(value: &str) -> Result<OutboxOperation, StorageError> {
    match value {
        "favorite_upsert" => Ok(OutboxOperation::FavoriteUpsert),
        "favorite_delete" => Ok(OutboxOperation::FavoriteDelete),
        "query_stats_upsert" => Ok(OutboxOperation::QueryStatsUpsert),
        _ => Err(StorageError::InvalidData("outbox operation")),
    }
}

fn parse_key(value: &str) -> Result<ContentKey, StorageError> {
    ContentKey::from_str(value).map_err(|_| StorageError::InvalidData("content key"))
}

fn mark_manual_conflict(
    transaction: &Transaction<'_>,
    key: ContentKey,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    transaction.execute(
        "UPDATE sync_outbox SET last_error=?2,next_retry_at=NULL,updated_at=?3
         WHERE coalesce_key=?1",
        params![format!("favorite:{key}"), CONFLICT_ERROR, now.as_seconds()],
    )?;
    transaction.execute(
        "UPDATE profile_meta SET last_sync_error=?1,updated_at=?2 WHERE singleton_id=1",
        params![CONFLICT_ERROR, now.as_seconds()],
    )?;
    Ok(())
}

fn bounded_error(error: &str) -> &str {
    error.get(..error.len().min(512)).unwrap_or(error)
}

fn to_u64(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidData("negative integer"))
}

fn to_i64(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::InvalidData("integer exceeds SQLite range"))
}
