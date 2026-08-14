use std::{fmt, fmt::Write as _};

use lvos_sync::{
    AckStatus, AggregateQueryStats, ChangesResponse, FavoriteRecord, FavoriteSnapshot, PushAck,
    PushEvent, PushResponse, QueryStatsSnapshot, SyncChange, SyncOperation,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::{RepositoryError, ServerRepository};

#[derive(Clone, Debug)]
pub(crate) struct MutationResult<T> {
    pub value: T,
    pub changed_revision: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct SyncConflict {
    pub event_id: Option<String>,
    pub current: Option<FavoriteRecord>,
    pub latest_revision: u64,
}

#[derive(Debug)]
pub(crate) enum SyncRepositoryError {
    Repository(RepositoryError),
    Conflict(Box<SyncConflict>),
    InvalidEvent,
}

impl fmt::Display for SyncRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository(_) => "the sync repository operation failed",
            Self::Conflict(_) => "the favorite revision conflicts with the server",
            Self::InvalidEvent => "the synchronization event is invalid",
        })
    }
}

impl From<RepositoryError> for SyncRepositoryError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

impl ServerRepository {
    pub(crate) fn push_events(
        &self,
        user_id: &str,
        device_id: &str,
        events: &[PushEvent],
        now: i64,
    ) -> Result<MutationResult<PushResponse>, SyncRepositoryError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|_| RepositoryError::Database)?;
        let mut acknowledgements = Vec::with_capacity(events.len());
        let mut changed_revision = None;
        for event in events {
            let fingerprint = event_fingerprint(event)?;
            if let Some(ack) = processed_ack(&transaction, user_id, event, &fingerprint)? {
                acknowledgements.push(ack);
                continue;
            }
            let outcome = apply_event(&transaction, user_id, device_id, event, now)?;
            changed_revision = changed_revision.max(outcome.changed_revision);
            store_ack(
                &transaction,
                user_id,
                event,
                &fingerprint,
                &outcome.ack,
                now,
            )?;
            acknowledgements.push(outcome.ack);
        }
        let latest_revision = user_revision(&transaction, user_id)?;
        transaction
            .commit()
            .map_err(|_| RepositoryError::Database)?;
        Ok(MutationResult {
            value: PushResponse {
                acknowledgements,
                latest_revision,
            },
            changed_revision,
        })
    }

    pub(crate) fn changes_since(
        &self,
        user_id: &str,
        since: u64,
        limit: usize,
    ) -> Result<ChangesResponse, SyncRepositoryError> {
        let connection = self.lock()?;
        let latest_revision = user_revision(&connection, user_id)?;
        let since_database = i64::try_from(since).map_err(|_| SyncRepositoryError::InvalidEvent)?;
        let limit = i64::try_from(limit).map_err(|_| SyncRepositoryError::InvalidEvent)?;
        let mut statement = connection
            .prepare(
                "SELECT revision, content_key, operation FROM change_log
                 WHERE user_id = ?1 AND revision > ?2 ORDER BY revision LIMIT ?3",
            )
            .map_err(|_| RepositoryError::Database)?;
        let rows = statement
            .query_map(params![user_id, since_database, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| RepositoryError::Database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RepositoryError::Database)?;
        let mut changes = Vec::with_capacity(rows.len());
        for (revision, content_key, operation) in rows {
            let favorite = favorite_record(&connection, user_id, &content_key)?
                .ok_or(SyncRepositoryError::InvalidEvent)?;
            changes.push(SyncChange {
                revision: u64::try_from(revision).map_err(|_| SyncRepositoryError::InvalidEvent)?,
                operation: parse_operation(&operation)?,
                favorite,
            });
        }
        let next_revision = changes.last().map_or(since, |change| change.revision);
        Ok(ChangesResponse {
            has_more: next_revision < latest_revision,
            changes,
            next_revision,
            latest_revision,
        })
    }

    pub(crate) fn active_favorites(
        &self,
        user_id: &str,
    ) -> Result<(Vec<FavoriteRecord>, u64), SyncRepositoryError> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT content_key FROM favorites WHERE user_id = ?1 AND deleted_at IS NULL
                 ORDER BY favorited_at DESC, content_key",
            )
            .map_err(|_| RepositoryError::Database)?;
        let keys = statement
            .query_map([user_id], |row| row.get::<_, String>(0))
            .map_err(|_| RepositoryError::Database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RepositoryError::Database)?;
        let mut favorites = Vec::with_capacity(keys.len());
        for key in keys {
            favorites.push(
                favorite_record(&connection, user_id, &key)?
                    .ok_or(SyncRepositoryError::InvalidEvent)?,
            );
        }
        Ok((favorites, user_revision(&connection, user_id)?))
    }

    pub(crate) fn set_favorite_state(
        &self,
        user_id: &str,
        content_key: &str,
        active: bool,
        base_entity_revision: u64,
        now: i64,
    ) -> Result<MutationResult<FavoriteRecord>, SyncRepositoryError> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|_| RepositoryError::Database)?;
        let current = favorite_record(&transaction, user_id, content_key)?
            .ok_or(RepositoryError::NotFound)?;
        if current.entity_revision != base_entity_revision {
            return Err(SyncRepositoryError::Conflict(Box::new(SyncConflict {
                event_id: None,
                current: Some(current),
                latest_revision: user_revision(&transaction, user_id)?,
            })));
        }
        let is_active = current.deleted_at.is_none();
        let changed_revision = if is_active == active {
            None
        } else {
            let entity_revision = current
                .entity_revision
                .checked_add(1)
                .ok_or(SyncRepositoryError::InvalidEvent)?;
            transaction
                .execute(
                    "UPDATE favorites SET deleted_at = ?3, updated_at = ?4,
                     entity_revision = ?5, server_received_at = ?4
                     WHERE user_id = ?1 AND content_key = ?2",
                    params![
                        user_id,
                        content_key,
                        if active { None } else { Some(now) },
                        now,
                        i64::try_from(entity_revision)
                            .map_err(|_| SyncRepositoryError::InvalidEvent)?
                    ],
                )
                .map_err(|_| RepositoryError::Database)?;
            Some(record_change(
                &transaction,
                user_id,
                content_key,
                entity_revision,
                if active {
                    SyncOperation::FavoriteUpsert
                } else {
                    SyncOperation::FavoriteDelete
                },
                now,
            )?)
        };
        let result = favorite_record(&transaction, user_id, content_key)?
            .ok_or(SyncRepositoryError::InvalidEvent)?;
        transaction
            .commit()
            .map_err(|_| RepositoryError::Database)?;
        Ok(MutationResult {
            value: result,
            changed_revision,
        })
    }
}

struct EventOutcome {
    ack: PushAck,
    changed_revision: Option<u64>,
}

#[allow(clippy::too_many_lines)]
fn apply_event(
    transaction: &Transaction<'_>,
    user_id: &str,
    device_id: &str,
    event: &PushEvent,
    now: i64,
) -> Result<EventOutcome, SyncRepositoryError> {
    let current = favorite_record(transaction, user_id, &event.content_key)?;
    if matches!(
        event.operation,
        SyncOperation::FavoriteUpsert | SyncOperation::FavoriteDelete
    ) {
        let actual = current
            .as_ref()
            .map_or(0, |favorite| favorite.entity_revision);
        if actual != event.base_entity_revision {
            return Err(SyncRepositoryError::Conflict(Box::new(SyncConflict {
                event_id: Some(event.event_id.clone()),
                current,
                latest_revision: user_revision(transaction, user_id)?,
            })));
        }
    }

    let mut entity_changed = false;
    let mut entity_revision = current.as_ref().map(|favorite| favorite.entity_revision);
    match event.operation {
        SyncOperation::FavoriteUpsert => {
            let snapshot = event
                .favorite
                .as_ref()
                .ok_or(SyncRepositoryError::InvalidEvent)?;
            let next_revision = entity_revision
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(SyncRepositoryError::InvalidEvent)?;
            let matches = current
                .as_ref()
                .is_some_and(|value| value.favorite == *snapshot && value.deleted_at.is_none());
            if !matches {
                upsert_favorite(transaction, user_id, event, snapshot, next_revision, now)?;
                entity_revision = Some(next_revision);
                entity_changed = true;
            }
        }
        SyncOperation::FavoriteDelete => {
            if event.favorite.is_some() {
                return Err(SyncRepositoryError::InvalidEvent);
            }
            let Some(value) = current else {
                return Err(SyncRepositoryError::InvalidEvent);
            };
            if value.deleted_at.is_none() {
                let next_revision = value
                    .entity_revision
                    .checked_add(1)
                    .ok_or(SyncRepositoryError::InvalidEvent)?;
                transaction
                    .execute(
                        "UPDATE favorites SET deleted_at = ?3, updated_at = ?3,
                         entity_revision = ?4, server_received_at = ?3
                         WHERE user_id = ?1 AND content_key = ?2",
                        params![
                            user_id,
                            event.content_key,
                            now,
                            i64::try_from(next_revision)
                                .map_err(|_| SyncRepositoryError::InvalidEvent)?
                        ],
                    )
                    .map_err(|_| RepositoryError::Database)?;
                entity_revision = Some(next_revision);
                entity_changed = true;
            }
        }
        SyncOperation::QueryStatsUpsert => {
            if event.favorite.is_some() || current.is_none() {
                return Err(SyncRepositoryError::InvalidEvent);
            }
        }
    }

    let stats_changed = if let Some(stats) = &event.query_stats {
        merge_query_stats(transaction, user_id, device_id, &event.content_key, stats)?
    } else {
        false
    };
    if event.operation == SyncOperation::QueryStatsUpsert && event.query_stats.is_none() {
        return Err(SyncRepositoryError::InvalidEvent);
    }
    let aggregate = if event.query_stats.is_some() {
        aggregate_stats(transaction, user_id, &event.content_key)?
    } else {
        None
    };
    let changed = entity_changed || stats_changed;
    let user_revision = if changed {
        record_change(
            transaction,
            user_id,
            &event.content_key,
            entity_revision.ok_or(SyncRepositoryError::InvalidEvent)?,
            event.operation,
            now,
        )?
    } else {
        user_revision(transaction, user_id)?
    };
    Ok(EventOutcome {
        ack: PushAck {
            event_id: event.event_id.clone(),
            status: if changed {
                AckStatus::Applied
            } else {
                AckStatus::NoChange
            },
            entity_revision: if event.operation == SyncOperation::QueryStatsUpsert {
                None
            } else {
                entity_revision
            },
            user_revision,
            aggregate_query_stats: aggregate,
        },
        changed_revision: changed.then_some(user_revision),
    })
}

fn upsert_favorite(
    transaction: &Transaction<'_>,
    user_id: &str,
    event: &PushEvent,
    snapshot: &FavoriteSnapshot,
    entity_revision: u64,
    now: i64,
) -> Result<(), SyncRepositoryError> {
    transaction
        .execute(
            "INSERT INTO favorites(user_id, content_key, key_version, kind, source_lang,
             target_lang, source_text, canonical_text, translation, provider, favorited_at,
             updated_at, deleted_at, entity_revision, server_received_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13, ?14)
             ON CONFLICT(user_id, content_key) DO UPDATE SET key_version=excluded.key_version,
             kind=excluded.kind, source_lang=excluded.source_lang, target_lang=excluded.target_lang,
             source_text=excluded.source_text, canonical_text=excluded.canonical_text,
             translation=excluded.translation, provider=excluded.provider,
             favorited_at=excluded.favorited_at, updated_at=excluded.updated_at, deleted_at=NULL,
             entity_revision=excluded.entity_revision, server_received_at=excluded.server_received_at",
            params![
                user_id,
                event.content_key,
                i64::from(event.key_version),
                snapshot.kind,
                snapshot.source_lang,
                snapshot.target_lang,
                snapshot.source_text,
                snapshot.canonical_text,
                snapshot.translation,
                snapshot.provider,
                snapshot.favorited_at,
                snapshot.updated_at,
                i64::try_from(entity_revision).map_err(|_| SyncRepositoryError::InvalidEvent)?,
                now,
            ],
        )
        .map_err(|_| RepositoryError::Database)?;
    Ok(())
}

fn merge_query_stats(
    transaction: &Transaction<'_>,
    user_id: &str,
    device_id: &str,
    content_key: &str,
    stats: &QueryStatsSnapshot,
) -> Result<bool, SyncRepositoryError> {
    let query_count =
        i64::try_from(stats.query_count).map_err(|_| SyncRepositoryError::InvalidEvent)?;
    let existing = transaction
        .query_row(
            "SELECT query_count, first_queried_at, last_queried_at, updated_at
             FROM device_query_stats WHERE user_id=?1 AND device_id=?2 AND content_key=?3",
            params![user_id, device_id, content_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|_| RepositoryError::Database)?;
    let merged = existing.map_or(
        (
            query_count,
            stats.first_queried_at,
            stats.last_queried_at,
            stats.updated_at,
        ),
        |value| {
            (
                value.0.max(query_count),
                value.1.min(stats.first_queried_at),
                value.2.max(stats.last_queried_at),
                value.3.max(stats.updated_at),
            )
        },
    );
    if existing == Some(merged) {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO device_query_stats(user_id, device_id, content_key, query_count,
             first_queried_at, last_queried_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(user_id, device_id, content_key) DO UPDATE SET
             query_count=excluded.query_count, first_queried_at=excluded.first_queried_at,
             last_queried_at=excluded.last_queried_at, updated_at=excluded.updated_at",
            params![
                user_id,
                device_id,
                content_key,
                merged.0,
                merged.1,
                merged.2,
                merged.3
            ],
        )
        .map_err(|_| RepositoryError::Database)?;
    Ok(true)
}

fn record_change(
    transaction: &Transaction<'_>,
    user_id: &str,
    content_key: &str,
    entity_revision: u64,
    operation: SyncOperation,
    now: i64,
) -> Result<u64, SyncRepositoryError> {
    transaction
        .execute(
            "UPDATE users SET sync_revision = sync_revision + 1 WHERE user_id = ?1",
            [user_id],
        )
        .map_err(|_| RepositoryError::Database)?;
    let revision = user_revision(transaction, user_id)?;
    transaction
        .execute(
            "INSERT INTO change_log(user_id, revision, content_key, entity_revision, operation, changed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                user_id,
                i64::try_from(revision).map_err(|_| SyncRepositoryError::InvalidEvent)?,
                content_key,
                i64::try_from(entity_revision).map_err(|_| SyncRepositoryError::InvalidEvent)?,
                operation_name(operation),
                now,
            ],
        )
        .map_err(|_| RepositoryError::Database)?;
    Ok(revision)
}

fn processed_ack(
    transaction: &Transaction<'_>,
    user_id: &str,
    event: &PushEvent,
    fingerprint: &str,
) -> Result<Option<PushAck>, SyncRepositoryError> {
    let row = transaction
        .query_row(
            "SELECT event_fingerprint, ack_status, entity_revision, user_revision,
             aggregate_query_count, aggregate_first_queried_at, aggregate_last_queried_at
             FROM processed_sync_events WHERE user_id=?1 AND event_id=?2",
            params![user_id, event.event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|_| RepositoryError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.0 != fingerprint {
        return Err(SyncRepositoryError::InvalidEvent);
    }
    let aggregate_query_stats = match (row.4, row.5, row.6) {
        (Some(count), Some(first), Some(last)) => Some(AggregateQueryStats {
            query_count: u64::try_from(count).map_err(|_| SyncRepositoryError::InvalidEvent)?,
            first_queried_at: first,
            last_queried_at: last,
        }),
        (None, None, None) => None,
        _ => return Err(SyncRepositoryError::InvalidEvent),
    };
    Ok(Some(PushAck {
        event_id: event.event_id.clone(),
        status: if row.1 == "applied" {
            AckStatus::Applied
        } else {
            AckStatus::NoChange
        },
        entity_revision: row
            .2
            .map(u64::try_from)
            .transpose()
            .map_err(|_| SyncRepositoryError::InvalidEvent)?,
        user_revision: u64::try_from(row.3).map_err(|_| SyncRepositoryError::InvalidEvent)?,
        aggregate_query_stats,
    }))
}

fn store_ack(
    transaction: &Transaction<'_>,
    user_id: &str,
    event: &PushEvent,
    fingerprint: &str,
    ack: &PushAck,
    now: i64,
) -> Result<(), SyncRepositoryError> {
    let aggregate = ack.aggregate_query_stats.as_ref();
    transaction
        .execute(
            "INSERT INTO processed_sync_events(user_id, event_id, event_fingerprint, operation,
             ack_status, entity_revision, user_revision, aggregate_query_count,
             aggregate_first_queried_at, aggregate_last_queried_at, processed_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                user_id,
                event.event_id,
                fingerprint,
                operation_name(event.operation),
                if ack.status == AckStatus::Applied {
                    "applied"
                } else {
                    "no_change"
                },
                ack.entity_revision
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| SyncRepositoryError::InvalidEvent)?,
                i64::try_from(ack.user_revision).map_err(|_| SyncRepositoryError::InvalidEvent)?,
                aggregate
                    .map(|value| i64::try_from(value.query_count))
                    .transpose()
                    .map_err(|_| SyncRepositoryError::InvalidEvent)?,
                aggregate.map(|value| value.first_queried_at),
                aggregate.map(|value| value.last_queried_at),
                now,
            ],
        )
        .map_err(|_| RepositoryError::Database)?;
    Ok(())
}

fn favorite_record(
    connection: &Connection,
    user_id: &str,
    content_key: &str,
) -> Result<Option<FavoriteRecord>, SyncRepositoryError> {
    let favorite = connection
        .query_row(
            "SELECT key_version, kind, source_lang, target_lang, source_text, canonical_text,
             translation, provider, favorited_at, updated_at, deleted_at, entity_revision,
             server_received_at FROM favorites WHERE user_id=?1 AND content_key=?2",
            params![user_id, content_key],
            |row| {
                Ok(FavoriteRecord {
                    content_key: content_key.to_owned(),
                    key_version: row.get(0)?,
                    favorite: FavoriteSnapshot {
                        kind: row.get(1)?,
                        source_lang: row.get(2)?,
                        target_lang: row.get(3)?,
                        source_text: row.get(4)?,
                        canonical_text: row.get(5)?,
                        translation: row.get(6)?,
                        provider: row.get(7)?,
                        favorited_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    },
                    deleted_at: row.get(10)?,
                    entity_revision: {
                        let value = row.get::<_, i64>(11)?;
                        u64::try_from(value)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, value))?
                    },
                    server_received_at: row.get(12)?,
                    aggregate_query_stats: None,
                })
            },
        )
        .optional()
        .map_err(|_| RepositoryError::Database)?;
    favorite
        .map(|mut value| {
            value.aggregate_query_stats = aggregate_stats(connection, user_id, content_key)?;
            Ok(value)
        })
        .transpose()
}

fn aggregate_stats(
    connection: &Connection,
    user_id: &str,
    content_key: &str,
) -> Result<Option<AggregateQueryStats>, SyncRepositoryError> {
    let row = connection
        .query_row(
            "SELECT SUM(query_count), MIN(first_queried_at), MAX(last_queried_at)
             FROM device_query_stats WHERE user_id=?1 AND content_key=?2",
            params![user_id, content_key],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .map_err(|_| RepositoryError::Database)?;
    match row {
        (Some(count), Some(first), Some(last)) => Ok(Some(AggregateQueryStats {
            query_count: u64::try_from(count).map_err(|_| SyncRepositoryError::InvalidEvent)?,
            first_queried_at: first,
            last_queried_at: last,
        })),
        (None, None, None) => Ok(None),
        _ => Err(SyncRepositoryError::InvalidEvent),
    }
}

fn user_revision(connection: &Connection, user_id: &str) -> Result<u64, SyncRepositoryError> {
    let value: i64 = connection
        .query_row(
            "SELECT sync_revision FROM users WHERE user_id=?1",
            [user_id],
            |row| row.get(0),
        )
        .map_err(|_| RepositoryError::Database)?;
    u64::try_from(value).map_err(|_| SyncRepositoryError::InvalidEvent)
}

fn event_fingerprint(event: &PushEvent) -> Result<String, SyncRepositoryError> {
    let bytes = serde_json::to_vec(event).map_err(|_| SyncRepositoryError::InvalidEvent)?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|_| SyncRepositoryError::InvalidEvent)?;
    }
    Ok(encoded)
}

pub(crate) const fn operation_name(operation: SyncOperation) -> &'static str {
    match operation {
        SyncOperation::FavoriteUpsert => "favorite_upsert",
        SyncOperation::FavoriteDelete => "favorite_delete",
        SyncOperation::QueryStatsUpsert => "query_stats_upsert",
    }
}

fn parse_operation(value: &str) -> Result<SyncOperation, SyncRepositoryError> {
    match value {
        "favorite_upsert" => Ok(SyncOperation::FavoriteUpsert),
        "favorite_delete" => Ok(SyncOperation::FavoriteDelete),
        "query_stats_upsert" => Ok(SyncOperation::QueryStatsUpsert),
        _ => Err(SyncRepositoryError::InvalidEvent),
    }
}
