use std::{
    fs,
    num::TryFromIntError,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use lvos_core::{ContentKey, LanguageCode, SOFTWARE_VERSION, TextKind, UnixTimestamp};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, backup::Backup, params};
use uuid::Uuid;

use crate::{
    Favorite, HistoryEntry, OutboxEvent, OutboxOperation, ProfileMetadata, QueryStats,
    StorageError, StoredContent, TranslationSnapshot,
    model::{FavoritePayload, QueryStatsPayload},
};

pub const SCHEMA_VERSION: u32 = 2;

const MIGRATION_1: &str = r"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at INTEGER NOT NULL
);
CREATE TABLE profile_meta (
    singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
    profile_id TEXT NOT NULL,
    user_id TEXT NULL,
    username TEXT NULL,
    device_id TEXT NOT NULL,
    platform TEXT NOT NULL CHECK(platform IN ('windows', 'macos')),
    server_origin TEXT NULL,
    last_server_revision INTEGER NOT NULL DEFAULT 0 CHECK(last_server_revision >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE history_entries (
    content_key TEXT PRIMARY KEY,
    key_version INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('word', 'text')),
    source_lang TEXT NOT NULL,
    target_lang TEXT NOT NULL,
    source_text TEXT NOT NULL,
    canonical_text TEXT NOT NULL,
    translation TEXT NOT NULL,
    provider TEXT NOT NULL,
    last_queried_at INTEGER NOT NULL,
    translation_updated_at INTEGER NOT NULL
);
CREATE INDEX idx_history_last_queried ON history_entries(last_queried_at DESC);
CREATE TABLE query_stats (
    content_key TEXT PRIMARY KEY,
    device_query_count INTEGER NOT NULL DEFAULT 1 CHECK(device_query_count >= 0),
    first_queried_at INTEGER NOT NULL,
    last_queried_at INTEGER NOT NULL,
    last_synced_device_query_count INTEGER NOT NULL DEFAULT 0 CHECK(last_synced_device_query_count >= 0),
    server_total_query_count INTEGER NOT NULL DEFAULT 0 CHECK(server_total_query_count >= 0),
    server_first_queried_at INTEGER NULL,
    server_last_queried_at INTEGER NULL,
    server_snapshot_at INTEGER NULL
);
CREATE TABLE favorites (
    content_key TEXT PRIMARY KEY,
    key_version INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('word', 'text')),
    source_lang TEXT NOT NULL,
    target_lang TEXT NOT NULL,
    source_text TEXT NOT NULL,
    canonical_text TEXT NOT NULL,
    translation TEXT NOT NULL,
    provider TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER NULL,
    entity_revision INTEGER NOT NULL DEFAULT 0 CHECK(entity_revision >= 0)
);
CREATE INDEX idx_favorites_created ON favorites(created_at DESC);
CREATE INDEX idx_favorites_active ON favorites(deleted_at);
CREATE TABLE sync_outbox (
    event_id TEXT PRIMARY KEY,
    content_key TEXT NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('favorite_upsert', 'favorite_delete', 'query_stats_upsert')),
    payload_json TEXT NULL,
    coalesce_key TEXT NULL,
    base_entity_revision INTEGER NULL CHECK(base_entity_revision IS NULL OR base_entity_revision >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    next_retry_at INTEGER NULL,
    last_error TEXT NULL
);
CREATE INDEX idx_outbox_retry ON sync_outbox(next_retry_at);
CREATE UNIQUE INDEX idx_outbox_coalesce ON sync_outbox(coalesce_key) WHERE coalesce_key IS NOT NULL;
";

const MIGRATION_2: &str = r"
ALTER TABLE profile_meta ADD COLUMN last_successful_sync_at INTEGER NULL;
ALTER TABLE profile_meta ADD COLUMN last_sync_error TEXT NULL;
ALTER TABLE profile_meta ADD COLUMN sse_connected INTEGER NOT NULL DEFAULT 0 CHECK(sse_connected IN (0, 1));
ALTER TABLE sync_outbox ADD COLUMN conflict_replay_count INTEGER NOT NULL DEFAULT 0 CHECK(conflict_replay_count >= 0);
";

#[derive(Clone, Debug)]
pub struct ProfilePaths {
    database: PathBuf,
    backups: PathBuf,
}

impl ProfilePaths {
    #[must_use]
    pub fn new(application_data_root: &Path, profile_id: Uuid) -> Self {
        Self {
            database: application_data_root.join(format!("profile-{profile_id}.sqlite3")),
            backups: application_data_root.join("backups"),
        }
    }

    #[must_use]
    pub fn database(&self) -> &Path {
        &self.database
    }

    #[must_use]
    pub fn backups(&self) -> &Path {
        &self.backups
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupArtifact {
    pub path: PathBuf,
    pub source_schema_version: u32,
    pub app_version: &'static str,
}

#[derive(Debug)]
pub struct ProfileDatabase {
    pub(crate) connection: Connection,
    paths: ProfilePaths,
    pre_migration_backup: Option<BackupArtifact>,
}

impl ProfileDatabase {
    /// Replaces the installation Device identity in one existing Profile without changing user
    /// data, sync cursor, or Outbox event IDs.
    ///
    /// # Errors
    /// Returns an error if the expected identity does not match or persistence fails.
    pub fn replace_device_identity_at(
        path: &Path,
        expected_current: Uuid,
        replacement: Uuid,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        let mut connection = Connection::open(path)?;
        replace_device_identity(&mut connection, expected_current, replacement, now)
    }

    /// Reads Profile metadata without opening the database for runtime use or running migrations.
    ///
    /// # Errors
    /// Returns an error for an unreadable, unmigrated, or malformed Profile.
    pub fn inspect_metadata(path: &Path) -> Result<ProfileMetadata, StorageError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        read_profile_metadata(&connection)
    }

    /// Opens one Desktop Profile and applies migrations after a consistent backup.
    ///
    /// # Errors
    /// Returns an error when opening, backup, migration, or Profile identity validation fails.
    pub fn open(paths: ProfilePaths, metadata: &ProfileMetadata) -> Result<Self, StorageError> {
        validate_profile_metadata(metadata)?;
        if paths.database.exists() {
            let parent = paths
                .database
                .parent()
                .ok_or(StorageError::InvalidData("profile path has no parent"))?;
            fs::create_dir_all(parent)?;
        } else if let Some(parent) = paths.database.parent() {
            fs::create_dir_all(parent)?;
        }
        let existed = paths.database.exists() && fs::metadata(&paths.database)?.len() > 0;
        let mut connection = Connection::open(&paths.database)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(Duration::from_secs(5))?;

        let current = current_schema_version(&connection)?;
        if current > SCHEMA_VERSION {
            return Err(StorageError::InvalidData(
                "profile schema is newer than this application",
            ));
        }
        let pre_migration_backup = if current < SCHEMA_VERSION && existed {
            Some(
                create_backup(&connection, &paths, current)
                    .map_err(|error| StorageError::Backup(Box::new(error)))?,
            )
        } else {
            None
        };
        apply_migrations(&mut connection, current)?;
        upsert_or_validate_profile(&mut connection, metadata)?;
        Ok(Self {
            connection,
            paths,
            pre_migration_backup,
        })
    }

    #[must_use]
    pub fn paths(&self) -> &ProfilePaths {
        &self.paths
    }

    #[must_use]
    pub fn pre_migration_backup(&self) -> Option<&BackupArtifact> {
        self.pre_migration_backup.as_ref()
    }

    /// Reads Profile metadata.
    ///
    /// # Errors
    /// Returns an error for missing or malformed persisted data.
    pub fn metadata(&self) -> Result<ProfileMetadata, StorageError> {
        read_profile_metadata(&self.connection)
    }

    /// Replaces the installation Device identity in the active Profile while retaining Outbox.
    ///
    /// # Errors
    /// Returns an error if the expected identity does not match or persistence fails.
    pub fn replace_device_identity(
        &mut self,
        expected_current: Uuid,
        replacement: Uuid,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        replace_device_identity(&mut self.connection, expected_current, replacement, now)
    }

    /// Returns whether the active Profile has no Server User binding.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn is_unbound(&self) -> Result<bool, StorageError> {
        self.connection
            .query_row(
                "SELECT user_id IS NULL FROM profile_meta WHERE singleton_id=1",
                [],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }

    /// Updates the descriptive account fields of an already bound Profile.
    ///
    /// # Errors
    /// Returns an error if the Profile is unbound, belongs to another User, or persistence fails.
    pub fn update_account_identity(
        &mut self,
        user_id: Uuid,
        username: &str,
        server_origin: &str,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        let existing = self.metadata()?;
        if existing.user_id != Some(user_id) {
            return Err(StorageError::InvalidData(
                "Profile User identity does not match",
            ));
        }
        self.connection.execute(
            "UPDATE profile_meta SET username=?1,server_origin=?2,updated_at=?3 WHERE singleton_id=1",
            params![username,server_origin,now.as_seconds()],
        )?;
        Ok(())
    }

    /// Records a successful lookup and coalesces `QueryStats` only after the Content entered the Favorite domain.
    ///
    /// # Errors
    /// Returns an error and rolls back both History and `QueryStats` changes on failure.
    pub fn record_successful_query(
        &mut self,
        entry: &HistoryEntry,
    ) -> Result<QueryStats, StorageError> {
        let transaction = self.connection.transaction()?;
        upsert_history(&transaction, entry)?;
        transaction.execute(
            "INSERT INTO query_stats (content_key, device_query_count, first_queried_at, last_queried_at) VALUES (?1, 1, ?2, ?2)
             ON CONFLICT(content_key) DO UPDATE SET device_query_count = device_query_count + 1, last_queried_at = excluded.last_queried_at",
            params![entry.content.content_key.to_string(), entry.last_queried_at.as_seconds()],
        )?;
        let stats = query_stats_tx(&transaction, entry.content.content_key)?
            .ok_or(StorageError::InvalidData("query stats upsert disappeared"))?;
        let in_sync_domain: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM favorites WHERE content_key = ?1)",
            [entry.content.content_key.to_string()],
            |row| row.get(0),
        )?;
        if in_sync_domain {
            upsert_query_stats_event(
                &transaction,
                entry.content.content_key,
                stats,
                entry.last_queried_at,
            )?;
        }
        transaction.commit()?;
        Ok(stats)
    }

    /// Returns a History entry by stable Content identity.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn history(&self, key: ContentKey) -> Result<Option<HistoryEntry>, StorageError> {
        self.connection
            .query_row(
                &format!("{HISTORY_SELECT} WHERE content_key=?1"),
                [key.to_string()],
                read_history,
            )
            .optional()
            .map_err(StorageError::from)?
            .map(parse_history)
            .transpose()
    }

    /// Searches source and translated text, newest lookup first.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn search_history(
        &self,
        term: &str,
        limit: u32,
    ) -> Result<Vec<HistoryEntry>, StorageError> {
        let mut statement = self.connection.prepare(&format!("{HISTORY_SELECT} WHERE source_text LIKE '%' || ?1 || '%' OR translation LIKE '%' || ?1 || '%' ORDER BY last_queried_at DESC LIMIT ?2"))?;
        let rows = statement.query_map(params![term, limit], read_history)?;
        rows.map(|row| row.map_err(StorageError::from).and_then(parse_history))
            .collect()
    }

    /// Adds or reactivates a Favorite and its Outbox intent atomically.
    ///
    /// # Errors
    /// Returns an error and rolls back when History/QueryStats is missing or persistence fails.
    pub fn favorite(
        &mut self,
        key: ContentKey,
        now: UnixTimestamp,
    ) -> Result<Favorite, StorageError> {
        let transaction = self.connection.transaction()?;
        let history = transaction
            .query_row(
                &format!("{HISTORY_SELECT} WHERE content_key=?1"),
                [key.to_string()],
                read_history,
            )
            .optional()?
            .map(parse_history)
            .transpose()?
            .ok_or(StorageError::MissingHistory)?;
        let stats = query_stats_tx(&transaction, key)?.ok_or(StorageError::InvalidData(
            "favorite query stats are missing",
        ))?;
        let existing = favorite_tx(&transaction, key)?;
        if existing
            .as_ref()
            .is_some_and(|favorite| favorite.deleted_at.is_none())
        {
            transaction.commit()?;
            return existing.ok_or(StorageError::InvalidData("favorite disappeared"));
        }
        let entity_revision = existing
            .as_ref()
            .map_or(0, |favorite| favorite.entity_revision);
        transaction.execute(
            "INSERT INTO favorites (content_key, key_version, kind, source_lang, target_lang, source_text, canonical_text, translation, provider, created_at, updated_at, deleted_at, entity_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, NULL, ?11)
             ON CONFLICT(content_key) DO UPDATE SET key_version=excluded.key_version, kind=excluded.kind, source_lang=excluded.source_lang, target_lang=excluded.target_lang, source_text=excluded.source_text, canonical_text=excluded.canonical_text, translation=excluded.translation, provider=excluded.provider, created_at=excluded.created_at, updated_at=excluded.updated_at, deleted_at=NULL",
            params![key.to_string(), history.content.key_version, history.content.kind.protocol_name(), history.content.source_lang.as_str(), history.translation.target_lang.as_str(), history.content.source_text, history.content.canonical_text, history.translation.translation, history.translation.provider, now.as_seconds(), to_i64(entity_revision)?],
        )?;
        upsert_favorite_event(
            &transaction,
            key,
            "active",
            entity_revision,
            stats,
            now,
            OutboxOperation::FavoriteUpsert,
        )?;
        transaction.commit()?;
        self.favorite_by_key(key)?
            .ok_or(StorageError::InvalidData("favorite upsert disappeared"))
    }

    /// Tombstones a Favorite and folds pending Outbox state according to its Server revision.
    ///
    /// # Errors
    /// Returns an error and rolls back both Favorite and Outbox changes on failure.
    pub fn unfavorite(
        &mut self,
        key: ContentKey,
        now: UnixTimestamp,
    ) -> Result<Favorite, StorageError> {
        let transaction = self.connection.transaction()?;
        let favorite = favorite_tx(&transaction, key)?
            .ok_or(StorageError::InvalidData("favorite does not exist"))?;
        if favorite.deleted_at.is_some() {
            transaction.commit()?;
            return Ok(favorite);
        }
        let stats = query_stats_tx(&transaction, key)?.ok_or(StorageError::InvalidData(
            "favorite query stats are missing",
        ))?;
        transaction.execute(
            "UPDATE favorites SET updated_at=?2, deleted_at=?2 WHERE content_key=?1",
            params![key.to_string(), now.as_seconds()],
        )?;
        transaction.execute(
            "DELETE FROM sync_outbox WHERE coalesce_key=?1",
            [query_stats_coalesce_key(
                key,
                &profile_device_id(&transaction)?,
            )],
        )?;
        if favorite.entity_revision == 0 {
            transaction.execute(
                "DELETE FROM sync_outbox WHERE coalesce_key=?1",
                [favorite_coalesce_key(key)],
            )?;
        } else {
            upsert_favorite_event(
                &transaction,
                key,
                "deleted",
                favorite.entity_revision,
                stats,
                now,
                OutboxOperation::FavoriteDelete,
            )?;
        }
        transaction.commit()?;
        self.favorite_by_key(key)?
            .ok_or(StorageError::InvalidData("favorite tombstone disappeared"))
    }

    /// Reads one Favorite including tombstones.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn favorite_by_key(&self, key: ContentKey) -> Result<Option<Favorite>, StorageError> {
        favorite_connection(&self.connection, key)
    }

    /// Searches active Favorites by source and translated text, newest update first.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn search_favorites(&self, term: &str, limit: u32) -> Result<Vec<Favorite>, StorageError> {
        let mut statement = self.connection.prepare(&format!(
            "{FAVORITE_SELECT_FIELDS} WHERE deleted_at IS NULL AND (source_text LIKE '%' || ?1 || '%' OR translation LIKE '%' || ?1 || '%') ORDER BY updated_at DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![term, limit], read_favorite)?;
        rows.map(|row| row.map_err(StorageError::from).and_then(parse_favorite))
            .collect()
    }

    /// Reads the current Device contribution and cached Server aggregate.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn query_stats(&self, key: ContentKey) -> Result<Option<QueryStats>, StorageError> {
        self.connection
            .query_row(
                "SELECT device_query_count, first_queried_at, last_queried_at, last_synced_device_query_count, server_total_query_count, server_first_queried_at, server_last_queried_at, server_snapshot_at FROM query_stats WHERE content_key=?1",
                [key.to_string()],
                read_query_stats,
            )
            .optional()
            .map_err(StorageError::from)?
            .map(parse_query_stats)
            .transpose()
    }

    /// Records a Server-acknowledged Favorite entity revision.
    ///
    /// # Errors
    /// Returns an error when the Favorite is absent, the revision is invalid, or persistence fails.
    pub fn acknowledge_favorite(
        &mut self,
        key: ContentKey,
        entity_revision: u64,
    ) -> Result<(), StorageError> {
        if entity_revision == 0 {
            return Err(StorageError::InvalidData(
                "acknowledged entity revision is zero",
            ));
        }
        let changed = self.connection.execute(
            "UPDATE favorites SET entity_revision=?2 WHERE content_key=?1",
            params![key.to_string(), to_i64(entity_revision)?],
        )?;
        if changed == 0 {
            return Err(StorageError::InvalidData("favorite does not exist"));
        }
        self.connection.execute(
            "DELETE FROM sync_outbox WHERE coalesce_key=?1",
            [favorite_coalesce_key(key)],
        )?;
        Ok(())
    }

    /// Binds an unbound Profile to its first stable Server User identity.
    ///
    /// # Errors
    /// Returns an error if the Profile is already bound to a different User or persistence fails.
    pub fn bind_user(
        &mut self,
        user_id: Uuid,
        username: &str,
        server_origin: &str,
        now: UnixTimestamp,
    ) -> Result<(), StorageError> {
        if user_id.is_nil() {
            return Err(StorageError::InvalidIdentifier("user"));
        }
        let existing: Option<String> = self.connection.query_row(
            "SELECT user_id FROM profile_meta WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )?;
        if existing
            .as_deref()
            .is_some_and(|value| value != user_id.to_string())
        {
            return Err(StorageError::InvalidData(
                "Profile is already bound to another User",
            ));
        }
        self.connection.execute(
            "UPDATE profile_meta SET user_id=?1,username=?2,server_origin=?3,updated_at=?4 WHERE singleton_id=1",
            params![user_id.to_string(),username,server_origin,now.as_seconds()],
        )?;
        Ok(())
    }

    /// Clears History and removes `QueryStats` only for Content that never entered the Favorite domain.
    ///
    /// # Errors
    /// Returns an error and rolls back the clear operation on failure.
    pub fn clear_history(&mut self) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM history_entries", [])?;
        transaction.execute(
            "DELETE FROM query_stats WHERE content_key NOT IN (SELECT content_key FROM favorites)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Lists Outbox events in creation order.
    ///
    /// # Errors
    /// Returns an error for malformed persisted data or `SQLite` failure.
    pub fn outbox_events(&self) -> Result<Vec<OutboxEvent>, StorageError> {
        let mut statement = self.connection.prepare("SELECT event_id, content_key, operation, payload_json, coalesce_key, base_entity_revision, created_at, updated_at, attempt_count, next_retry_at, last_error, conflict_replay_count FROM sync_outbox ORDER BY created_at, event_id")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get(10)?,
                row.get::<_, i64>(11)?,
            ))
        })?;
        rows.map(|row| {
            let row = row?;
            Ok(OutboxEvent {
                event_id: parse_uuid(&row.0, "event")?,
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
        })
        .collect()
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        SCHEMA_VERSION
    }
}

const HISTORY_SELECT: &str = "SELECT content_key, key_version, kind, source_lang, target_lang, source_text, canonical_text, translation, provider, last_queried_at, translation_updated_at FROM history_entries";
type HistoryRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
);

fn read_history(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRow> {
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
    ))
}

fn parse_history(row: HistoryRow) -> Result<HistoryEntry, StorageError> {
    Ok(HistoryEntry {
        content: StoredContent {
            content_key: parse_key(&row.0)?,
            key_version: u32::try_from(row.1).map_err(map_int("key version"))?,
            kind: parse_kind(&row.2)?,
            source_lang: parse_language(&row.3)?,
            source_text: row.5,
            canonical_text: row.6,
        },
        translation: TranslationSnapshot {
            target_lang: parse_language(&row.4)?,
            translation: row.7,
            provider: row.8,
            updated_at: UnixTimestamp::from_seconds(row.10),
        },
        last_queried_at: UnixTimestamp::from_seconds(row.9),
    })
}

fn upsert_history(transaction: &Transaction<'_>, entry: &HistoryEntry) -> Result<(), StorageError> {
    transaction.execute(
        "INSERT INTO history_entries (content_key,key_version,kind,source_lang,target_lang,source_text,canonical_text,translation,provider,last_queried_at,translation_updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(content_key) DO UPDATE SET key_version=excluded.key_version,kind=excluded.kind,source_lang=excluded.source_lang,target_lang=excluded.target_lang,source_text=excluded.source_text,canonical_text=excluded.canonical_text,translation=excluded.translation,provider=excluded.provider,last_queried_at=excluded.last_queried_at,translation_updated_at=excluded.translation_updated_at",
        params![entry.content.content_key.to_string(),entry.content.key_version,entry.content.kind.protocol_name(),entry.content.source_lang.as_str(),entry.translation.target_lang.as_str(),entry.content.source_text,entry.content.canonical_text,entry.translation.translation,entry.translation.provider,entry.last_queried_at.as_seconds(),entry.translation.updated_at.as_seconds()],
    )?;
    Ok(())
}

fn query_stats_tx(
    transaction: &Transaction<'_>,
    key: ContentKey,
) -> Result<Option<QueryStats>, StorageError> {
    transaction.query_row("SELECT device_query_count, first_queried_at, last_queried_at, last_synced_device_query_count, server_total_query_count, server_first_queried_at, server_last_queried_at, server_snapshot_at FROM query_stats WHERE content_key=?1", [key.to_string()], read_query_stats).optional().map_err(StorageError::from)?.map(parse_query_stats).transpose()
}

type StatsRow = (
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);
fn read_query_stats(row: &rusqlite::Row<'_>) -> rusqlite::Result<StatsRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}
fn parse_query_stats(row: StatsRow) -> Result<QueryStats, StorageError> {
    Ok(QueryStats {
        device_query_count: to_u64(row.0)?,
        first_queried_at: UnixTimestamp::from_seconds(row.1),
        last_queried_at: UnixTimestamp::from_seconds(row.2),
        last_synced_device_query_count: to_u64(row.3)?,
        server_total_query_count: to_u64(row.4)?,
        server_first_queried_at: row.5.map(UnixTimestamp::from_seconds),
        server_last_queried_at: row.6.map(UnixTimestamp::from_seconds),
        server_snapshot_at: row.7.map(UnixTimestamp::from_seconds),
    })
}

const FAVORITE_SELECT_FIELDS: &str = "SELECT content_key,key_version,kind,source_lang,target_lang,source_text,canonical_text,translation,provider,created_at,updated_at,deleted_at,entity_revision FROM favorites";
const FAVORITE_SELECT: &str = "SELECT content_key,key_version,kind,source_lang,target_lang,source_text,canonical_text,translation,provider,created_at,updated_at,deleted_at,entity_revision FROM favorites WHERE content_key=?1";
type FavoriteRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    i64,
);
fn read_favorite(row: &rusqlite::Row<'_>) -> rusqlite::Result<FavoriteRow> {
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
        row.get(12)?,
    ))
}
fn parse_favorite(row: FavoriteRow) -> Result<Favorite, StorageError> {
    Ok(Favorite {
        content: StoredContent {
            content_key: parse_key(&row.0)?,
            key_version: u32::try_from(row.1).map_err(map_int("key version"))?,
            kind: parse_kind(&row.2)?,
            source_lang: parse_language(&row.3)?,
            source_text: row.5,
            canonical_text: row.6,
        },
        translation: TranslationSnapshot {
            target_lang: parse_language(&row.4)?,
            translation: row.7,
            provider: row.8,
            updated_at: UnixTimestamp::from_seconds(row.10),
        },
        created_at: UnixTimestamp::from_seconds(row.9),
        updated_at: UnixTimestamp::from_seconds(row.10),
        deleted_at: row.11.map(UnixTimestamp::from_seconds),
        entity_revision: to_u64(row.12)?,
    })
}
fn favorite_tx(
    transaction: &Transaction<'_>,
    key: ContentKey,
) -> Result<Option<Favorite>, StorageError> {
    transaction
        .query_row(FAVORITE_SELECT, [key.to_string()], read_favorite)
        .optional()
        .map_err(StorageError::from)?
        .map(parse_favorite)
        .transpose()
}
fn favorite_connection(
    connection: &Connection,
    key: ContentKey,
) -> Result<Option<Favorite>, StorageError> {
    connection
        .query_row(FAVORITE_SELECT, [key.to_string()], read_favorite)
        .optional()
        .map_err(StorageError::from)?
        .map(parse_favorite)
        .transpose()
}

fn upsert_query_stats_event(
    transaction: &Transaction<'_>,
    key: ContentKey,
    stats: QueryStats,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    let device_id = profile_device_id(transaction)?;
    let key_hex = key.to_string();
    let payload = serde_json::to_string(&QueryStatsPayload {
        content_key: &key_hex,
        device_query_count: stats.device_query_count,
        first_queried_at: stats.first_queried_at.as_seconds(),
        last_queried_at: stats.last_queried_at.as_seconds(),
    })?;
    upsert_outbox(
        transaction,
        &PendingOutbox {
            event_id: Uuid::now_v7(),
            key,
            operation: OutboxOperation::QueryStatsUpsert,
            payload: &payload,
            coalesce_key: &query_stats_coalesce_key(key, &device_id),
            revision: None,
            now,
        },
    )
}

fn upsert_favorite_event(
    transaction: &Transaction<'_>,
    key: ContentKey,
    desired_state: &str,
    revision: u64,
    stats: QueryStats,
    now: UnixTimestamp,
    operation: OutboxOperation,
) -> Result<(), StorageError> {
    let key_hex = key.to_string();
    let payload = serde_json::to_string(&FavoritePayload {
        content_key: &key_hex,
        desired_state,
        base_entity_revision: revision,
        query_stats: QueryStatsPayload {
            content_key: &key_hex,
            device_query_count: stats.device_query_count,
            first_queried_at: stats.first_queried_at.as_seconds(),
            last_queried_at: stats.last_queried_at.as_seconds(),
        },
    })?;
    upsert_outbox(
        transaction,
        &PendingOutbox {
            event_id: Uuid::now_v7(),
            key,
            operation,
            payload: &payload,
            coalesce_key: &favorite_coalesce_key(key),
            revision: Some(revision),
            now,
        },
    )
}

struct PendingOutbox<'a> {
    event_id: Uuid,
    key: ContentKey,
    operation: OutboxOperation,
    payload: &'a str,
    coalesce_key: &'a str,
    revision: Option<u64>,
    now: UnixTimestamp,
}

fn upsert_outbox(
    transaction: &Transaction<'_>,
    event: &PendingOutbox<'_>,
) -> Result<(), StorageError> {
    transaction.execute(
        "INSERT INTO sync_outbox (event_id,content_key,operation,payload_json,coalesce_key,base_entity_revision,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)
         ON CONFLICT(coalesce_key) WHERE coalesce_key IS NOT NULL DO UPDATE SET event_id=excluded.event_id,operation=excluded.operation,payload_json=excluded.payload_json,base_entity_revision=excluded.base_entity_revision,updated_at=excluded.updated_at,attempt_count=0,next_retry_at=NULL,last_error=NULL",
        params![event.event_id.to_string(),event.key.to_string(),event.operation.as_str(),event.payload,event.coalesce_key,event.revision.map(to_i64).transpose()?,event.now.as_seconds()],
    )?;
    Ok(())
}

fn current_schema_version(connection: &Connection) -> Result<u32, StorageError> {
    let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')", [], |row| row.get(0))?;
    if !exists {
        return Ok(0);
    }
    let version: Option<i64> =
        connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    version.map_or(Ok(0), |value| {
        u32::try_from(value).map_err(map_int("schema version"))
    })
}

fn apply_migrations(connection: &mut Connection, current: u32) -> Result<(), StorageError> {
    if current < 1 {
        let transaction = connection.transaction()?;
        transaction
            .execute_batch(MIGRATION_1)
            .map_err(|source| StorageError::Migration { version: 1, source })?;
        transaction.execute("INSERT INTO schema_migrations (version,name,applied_at) VALUES (1,'desktop_initial',strftime('%s','now'))", []).map_err(|source| StorageError::Migration { version: 1, source })?;
        transaction
            .commit()
            .map_err(|source| StorageError::Migration { version: 1, source })?;
    }
    if current < 2 {
        let transaction = connection.transaction()?;
        transaction
            .execute_batch(MIGRATION_2)
            .map_err(|source| StorageError::Migration { version: 2, source })?;
        transaction.execute("INSERT INTO schema_migrations (version,name,applied_at) VALUES (2,'desktop_sync_runtime',strftime('%s','now'))", []).map_err(|source| StorageError::Migration { version: 2, source })?;
        transaction
            .commit()
            .map_err(|source| StorageError::Migration { version: 2, source })?;
    }
    Ok(())
}

fn create_backup(
    source: &Connection,
    paths: &ProfilePaths,
    source_version: u32,
) -> Result<BackupArtifact, StorageError> {
    fs::create_dir_all(&paths.backups)?;
    let stem = paths
        .database
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or(StorageError::InvalidData("profile filename is not UTF-8"))?;
    let backup_id = Uuid::new_v4();
    let path = paths.backups.join(format!(
        "{stem}.pre-v{source_version}.app-{SOFTWARE_VERSION}.{backup_id}.sqlite3"
    ));
    let mut destination = Connection::open(&path)?;
    let backup = Backup::new(source, &mut destination)?;
    backup.run_to_completion(128, Duration::from_millis(1), None)?;
    drop(backup);
    destination.close().map_err(|(_, error)| error)?;
    Ok(BackupArtifact {
        path,
        source_schema_version: source_version,
        app_version: SOFTWARE_VERSION,
    })
}

fn upsert_or_validate_profile(
    connection: &mut Connection,
    metadata: &ProfileMetadata,
) -> Result<(), StorageError> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT profile_id,device_id FROM profile_meta WHERE singleton_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((profile_id, device_id)) = existing {
        if profile_id != metadata.profile_id.to_string()
            || device_id != metadata.device_id.to_string()
        {
            return Err(StorageError::InvalidData(
                "Profile identity does not match its database",
            ));
        }
        return Ok(());
    }
    connection.execute("INSERT INTO profile_meta (singleton_id,profile_id,user_id,username,device_id,platform,server_origin,last_server_revision,created_at,updated_at) VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![metadata.profile_id.to_string(),metadata.user_id.map(|value| value.to_string()),metadata.username,metadata.device_id.to_string(),metadata.platform,metadata.server_origin,to_i64(metadata.last_server_revision)?,metadata.created_at.as_seconds(),metadata.updated_at.as_seconds()])?;
    Ok(())
}

fn replace_device_identity(
    connection: &mut Connection,
    expected_current: Uuid,
    replacement: Uuid,
    now: UnixTimestamp,
) -> Result<(), StorageError> {
    if expected_current.is_nil() || replacement.is_nil() || expected_current == replacement {
        return Err(StorageError::InvalidIdentifier("device"));
    }
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE profile_meta SET device_id=?1,updated_at=?2
         WHERE singleton_id=1 AND device_id=?3",
        params![
            replacement.to_string(),
            now.as_seconds(),
            expected_current.to_string()
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::InvalidData(
            "Profile Device identity does not match",
        ));
    }
    transaction.execute(
        "UPDATE sync_outbox SET coalesce_key='query_stats:' || ?1 || ':' || content_key,
         updated_at=?2 WHERE operation='query_stats_upsert'",
        params![replacement.to_string(), now.as_seconds()],
    )?;
    transaction.execute(
        "UPDATE query_stats
         SET device_query_count=device_query_count-last_synced_device_query_count,
             last_synced_device_query_count=0",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}

fn read_profile_metadata(connection: &Connection) -> Result<ProfileMetadata, StorageError> {
    connection.query_row(
        "SELECT profile_id, user_id, username, device_id, platform, server_origin, last_server_revision, created_at, updated_at FROM profile_meta WHERE singleton_id = 1",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get(2)?, row.get::<_, String>(3)?, row.get(4)?, row.get(5)?, row.get::<_, i64>(6)?, row.get(7)?, row.get(8)?)),
    ).map_err(StorageError::from).and_then(|row| Ok(ProfileMetadata {
        profile_id: parse_uuid(&row.0, "profile")?,
        user_id: row.1.as_deref().map(|value| parse_uuid(value, "user")).transpose()?,
        username: row.2,
        device_id: parse_uuid(&row.3, "device")?,
        platform: row.4,
        server_origin: row.5,
        last_server_revision: to_u64(row.6)?,
        created_at: UnixTimestamp::from_seconds(row.7),
        updated_at: UnixTimestamp::from_seconds(row.8),
    }))
}

fn validate_profile_metadata(metadata: &ProfileMetadata) -> Result<(), StorageError> {
    if metadata.profile_id.is_nil() {
        return Err(StorageError::InvalidIdentifier("profile"));
    }
    if metadata.device_id.is_nil() {
        return Err(StorageError::InvalidIdentifier("device"));
    }
    if !matches!(metadata.platform.as_str(), "windows" | "macos") {
        return Err(StorageError::InvalidData("unsupported platform"));
    }
    Ok(())
}

fn profile_device_id(transaction: &Transaction<'_>) -> Result<String, StorageError> {
    transaction
        .query_row(
            "SELECT device_id FROM profile_meta WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}
fn favorite_coalesce_key(key: ContentKey) -> String {
    format!("favorite:{key}")
}
fn query_stats_coalesce_key(key: ContentKey, device_id: &str) -> String {
    format!("query_stats:{device_id}:{key}")
}
fn parse_key(value: &str) -> Result<ContentKey, StorageError> {
    ContentKey::from_str(value).map_err(|_| StorageError::InvalidData("content key"))
}
fn parse_language(value: &str) -> Result<LanguageCode, StorageError> {
    LanguageCode::from_str(value).map_err(|_| StorageError::InvalidData("language code"))
}
fn parse_uuid(value: &str, kind: &'static str) -> Result<Uuid, StorageError> {
    Uuid::parse_str(value).map_err(|_| StorageError::InvalidIdentifier(kind))
}
fn parse_kind(value: &str) -> Result<TextKind, StorageError> {
    match value {
        "word" => Ok(TextKind::Word),
        "text" => Ok(TextKind::Text),
        _ => Err(StorageError::InvalidData("text kind")),
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
fn to_u64(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(map_int("negative integer"))
}
fn to_i64(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::InvalidData("integer exceeds SQLite range"))
}
fn map_int(message: &'static str) -> impl FnOnce(TryFromIntError) -> StorageError {
    move |_| StorageError::InvalidData(message)
}
