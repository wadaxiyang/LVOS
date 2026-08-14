use std::{fs, num::NonZeroUsize};

use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{
    AcknowledgedEvent, HistoryEntry, InstallationStore, OutboxOperation, Platform, ProfileDatabase,
    ProfileMetadata, ProfilePaths, StoredContent, TranslationSnapshot,
};
use lvos_sync::{AckStatus, AggregateQueryStats, PushAck};
use rusqlite::Connection;
use tempfile::tempdir;
use uuid::Uuid;

fn profile_metadata(profile_id: Uuid, device_id: Uuid) -> ProfileMetadata {
    ProfileMetadata {
        profile_id,
        user_id: None,
        username: None,
        device_id,
        platform: "macos".to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: UnixTimestamp::from_seconds(1_780_000_000),
        updated_at: UnixTimestamp::from_seconds(1_780_000_000),
    }
}

fn history(source: &str, translation: &str, timestamp: i64) -> HistoryEntry {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(
            NonZeroUsize::new(1_000).unwrap_or_else(|| unreachable!("nonzero fixture")),
        ),
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
            translation: translation.to_owned(),
            provider: "fixture".to_owned(),
            updated_at: UnixTimestamp::from_seconds(timestamp),
        },
        last_queried_at: UnixTimestamp::from_seconds(timestamp),
    }
}

#[test]
fn installation_identity_is_created_once_and_reloaded() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let store = InstallationStore::new(directory.path());
    let first = store
        .load_or_create(Platform::Macos, "Developer Mac")
        .unwrap_or_else(|error| unreachable!("installation: {error}"));
    let second = store
        .load_or_create(Platform::Windows, "ignored after creation")
        .unwrap_or_else(|error| unreachable!("installation: {error}"));
    assert_eq!(first, second);
    assert_eq!(
        store.path().file_name().and_then(|name| name.to_str()),
        Some("installation.json")
    );
}

#[test]
fn replacement_device_preserves_effective_stats_without_double_counting_old_snapshot() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let original_device = Uuid::new_v4();
    let metadata = profile_metadata(Uuid::new_v4(), original_device);
    let mut database = ProfileDatabase::open(
        ProfilePaths::new(directory.path(), metadata.profile_id),
        &metadata,
    )
    .unwrap_or_else(|error| unreachable!("open: {error}"));
    let entry = history("Device replacement", "设备替换", 100);
    let key = entry.content.content_key;
    database
        .record_successful_query(&entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .favorite(key, UnixTimestamp::from_seconds(101))
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    let event = database
        .outbox_events()
        .unwrap_or_default()
        .into_iter()
        .next()
        .unwrap_or_else(|| unreachable!("Favorite event"));
    database
        .acknowledge_events(
            &[AcknowledgedEvent {
                event: event.clone(),
                acknowledgement: PushAck {
                    event_id: event.event_id.to_string(),
                    status: AckStatus::Applied,
                    entity_revision: Some(1),
                    user_revision: 1,
                    aggregate_query_stats: Some(AggregateQueryStats {
                        query_count: 1,
                        first_queried_at: 100,
                        last_queried_at: 100,
                    }),
                },
                sent_query_count: Some(1),
            }],
            UnixTimestamp::from_seconds(102),
        )
        .unwrap_or_else(|error| unreachable!("acknowledge: {error}"));
    let replacement = Uuid::new_v4();
    database
        .replace_device_identity(
            original_device,
            replacement,
            UnixTimestamp::from_seconds(103),
        )
        .unwrap_or_else(|error| unreachable!("replace: {error}"));
    let stats = database
        .query_stats(key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("QueryStats"));
    assert_eq!(stats.device_query_count, 0);
    assert_eq!(stats.last_synced_device_query_count, 0);
    assert_eq!(stats.effective_total(), 1);
    database
        .unfavorite(key, UnixTimestamp::from_seconds(104))
        .unwrap_or_else(|error| unreachable!("unfavorite: {error}"));
    let event = database
        .outbox_events()
        .unwrap_or_default()
        .into_iter()
        .next()
        .unwrap_or_else(|| unreachable!("delete event"));
    let push = database
        .push_event(&event)
        .unwrap_or_else(|error| unreachable!("push event: {error}"));
    assert!(push.query_stats.is_none());
}

#[test]
fn history_query_stats_and_outbox_are_deduplicated() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let metadata = profile_metadata(profile_id, Uuid::new_v4());
    let mut database =
        ProfileDatabase::open(ProfilePaths::new(directory.path(), profile_id), &metadata)
            .unwrap_or_else(|error| unreachable!("open: {error}"));
    let mut entry = history("Invariant", "不变的", 100);
    let key = entry.content.content_key;
    for timestamp in 100..110 {
        entry.last_queried_at = UnixTimestamp::from_seconds(timestamp);
        let stats = database
            .record_successful_query(&entry)
            .unwrap_or_else(|error| unreachable!("query: {error}"));
        assert_eq!(
            stats.device_query_count,
            u64::try_from(timestamp - 99).unwrap_or_default()
        );
    }
    assert_eq!(
        database
            .query_stats(key)
            .unwrap_or_default()
            .map(|stats| stats.device_query_count),
        Some(10)
    );
    assert_eq!(
        database
            .search_history("不变", 20)
            .unwrap_or_default()
            .len(),
        1
    );
    assert!(database.outbox_events().unwrap_or_default().is_empty());

    database
        .favorite(key, UnixTimestamp::from_seconds(200))
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    assert_eq!(
        database
            .search_favorites("不变", 20)
            .unwrap_or_default()
            .len(),
        1
    );
    for timestamp in 201..210 {
        database
            .favorite(key, UnixTimestamp::from_seconds(timestamp))
            .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    }
    for timestamp in 201..205 {
        entry.last_queried_at = UnixTimestamp::from_seconds(timestamp);
        database
            .record_successful_query(&entry)
            .unwrap_or_else(|error| unreachable!("query: {error}"));
    }
    let events = database.outbox_events().unwrap_or_default();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.operation == OutboxOperation::QueryStatsUpsert)
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|event| event.payload_json.contains("\"device_query_count\":14"))
    );
}

#[test]
fn pending_favorite_cancel_folds_events_but_preserves_tombstone_and_stats() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let metadata = profile_metadata(profile_id, Uuid::new_v4());
    let mut database =
        ProfileDatabase::open(ProfilePaths::new(directory.path(), profile_id), &metadata)
            .unwrap_or_else(|error| unreachable!("open: {error}"));
    let entry = history("Invariant", "不变的", 100);
    let key = entry.content.content_key;
    database
        .record_successful_query(&entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .favorite(key, UnixTimestamp::from_seconds(200))
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    database
        .unfavorite(key, UnixTimestamp::from_seconds(300))
        .unwrap_or_else(|error| unreachable!("unfavorite: {error}"));
    assert!(database.outbox_events().unwrap_or_default().is_empty());
    assert!(
        database
            .favorite_by_key(key)
            .unwrap_or_default()
            .and_then(|favorite| favorite.deleted_at)
            .is_some()
    );
    assert!(database.query_stats(key).unwrap_or_default().is_some());
}

#[test]
fn synced_unfavorite_folds_stats_into_one_delete_event() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let metadata = profile_metadata(profile_id, Uuid::new_v4());
    let mut database =
        ProfileDatabase::open(ProfilePaths::new(directory.path(), profile_id), &metadata)
            .unwrap_or_else(|error| unreachable!("open: {error}"));
    let mut entry = history("Invariant", "不变的", 100);
    let key = entry.content.content_key;
    database
        .record_successful_query(&entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .favorite(key, UnixTimestamp::from_seconds(200))
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    database
        .acknowledge_favorite(key, 8)
        .unwrap_or_else(|error| unreachable!("ack: {error}"));
    entry.last_queried_at = UnixTimestamp::from_seconds(250);
    database
        .record_successful_query(&entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .unfavorite(key, UnixTimestamp::from_seconds(300))
        .unwrap_or_else(|error| unreachable!("unfavorite: {error}"));
    let events = database.outbox_events().unwrap_or_default();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, OutboxOperation::FavoriteDelete);
    assert_eq!(events[0].base_entity_revision, Some(8));
    assert!(events[0].payload_json.contains("\"device_query_count\":2"));
}

#[test]
fn clear_history_preserves_only_content_that_entered_favorite_domain() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let metadata = profile_metadata(profile_id, Uuid::new_v4());
    let mut database =
        ProfileDatabase::open(ProfilePaths::new(directory.path(), profile_id), &metadata)
            .unwrap_or_else(|error| unreachable!("open: {error}"));
    let favorite_entry = history("Invariant", "不变的", 100);
    let ordinary_entry = history("Representation", "表示", 101);
    database
        .record_successful_query(&favorite_entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .record_successful_query(&ordinary_entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database
        .favorite(
            favorite_entry.content.content_key,
            UnixTimestamp::from_seconds(200),
        )
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    database
        .unfavorite(
            favorite_entry.content.content_key,
            UnixTimestamp::from_seconds(300),
        )
        .unwrap_or_else(|error| unreachable!("unfavorite: {error}"));
    database
        .clear_history()
        .unwrap_or_else(|error| unreachable!("clear: {error}"));
    assert!(
        database
            .history(favorite_entry.content.content_key)
            .unwrap_or_default()
            .is_none()
    );
    assert!(
        database
            .query_stats(favorite_entry.content.content_key)
            .unwrap_or_default()
            .is_some()
    );
    assert!(
        database
            .query_stats(ordinary_entry.content.content_key)
            .unwrap_or_default()
            .is_none()
    );
    assert!(
        database
            .favorite_by_key(favorite_entry.content.content_key)
            .unwrap_or_default()
            .is_some()
    );
}

#[test]
fn profiles_are_isolated_and_first_login_binding_preserves_data() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let device_id = Uuid::new_v4();
    let profile_a = Uuid::new_v4();
    let profile_b = Uuid::new_v4();
    let mut database_a = ProfileDatabase::open(
        ProfilePaths::new(directory.path(), profile_a),
        &profile_metadata(profile_a, device_id),
    )
    .unwrap_or_else(|error| unreachable!("open A: {error}"));
    let database_b = ProfileDatabase::open(
        ProfilePaths::new(directory.path(), profile_b),
        &profile_metadata(profile_b, device_id),
    )
    .unwrap_or_else(|error| unreachable!("open B: {error}"));
    let entry = history("Invariant", "不变的", 100);
    database_a
        .record_successful_query(&entry)
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    database_a
        .bind_user(
            Uuid::new_v4(),
            "alice",
            "https://example.invalid",
            UnixTimestamp::from_seconds(200),
        )
        .unwrap_or_else(|error| unreachable!("bind: {error}"));
    assert!(
        database_a
            .history(entry.content.content_key)
            .unwrap_or_default()
            .is_some()
    );
    assert!(
        database_b
            .history(entry.content.content_key)
            .unwrap_or_default()
            .is_none()
    );
    assert_ne!(database_a.paths().database(), database_b.paths().database());
    assert_eq!(
        database_a
            .metadata()
            .unwrap_or_else(|error| unreachable!("metadata: {error}"))
            .device_id,
        device_id
    );
}

#[test]
fn legacy_database_is_backed_up_consistently_before_migration() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let paths = ProfilePaths::new(directory.path(), profile_id);
    let legacy =
        Connection::open(paths.database()).unwrap_or_else(|error| unreachable!("legacy: {error}"));
    legacy.execute_batch("CREATE TABLE legacy_marker(value TEXT NOT NULL); INSERT INTO legacy_marker VALUES ('preserved');").unwrap_or_else(|error| unreachable!("legacy: {error}"));
    drop(legacy);
    let database = ProfileDatabase::open(paths, &profile_metadata(profile_id, Uuid::new_v4()))
        .unwrap_or_else(|error| unreachable!("migration: {error}"));
    let artifact = database
        .pre_migration_backup()
        .unwrap_or_else(|| unreachable!("backup expected"));
    assert!(artifact.path.exists());
    assert_eq!(artifact.source_schema_version, 0);
    assert_eq!(artifact.app_version, "0.1.2");
    let backup =
        Connection::open(&artifact.path).unwrap_or_else(|error| unreachable!("backup: {error}"));
    let value: String = backup
        .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
        .unwrap_or_else(|error| unreachable!("backup query: {error}"));
    assert_eq!(value, "preserved");
    assert!(fs::metadata(database.paths().database()).is_ok());
}

#[test]
fn failed_migration_refuses_open_and_rolls_back_partial_schema() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let profile_id = Uuid::new_v4();
    let paths = ProfilePaths::new(directory.path(), profile_id);
    let legacy =
        Connection::open(paths.database()).unwrap_or_else(|error| unreachable!("legacy: {error}"));
    legacy
        .execute_batch(
            "CREATE TABLE history_entries(conflicting_column TEXT); CREATE TABLE legacy_marker(value TEXT); INSERT INTO legacy_marker VALUES ('recoverable');",
        )
        .unwrap_or_else(|error| unreachable!("legacy: {error}"));
    drop(legacy);

    let result =
        ProfileDatabase::open(paths.clone(), &profile_metadata(profile_id, Uuid::new_v4()));
    assert!(result.is_err(), "conflicting schema must reject startup");

    let source =
        Connection::open(paths.database()).unwrap_or_else(|error| unreachable!("source: {error}"));
    let migrations_exist: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    let query_stats_exist: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='query_stats')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    assert!(!migrations_exist);
    assert!(!query_stats_exist);

    let backup_path = fs::read_dir(paths.backups())
        .unwrap_or_else(|error| unreachable!("backup directory: {error}"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(&format!(
                        "profile-{profile_id}.pre-v0.app-{}.",
                        env!("CARGO_PKG_VERSION")
                    )) && name.ends_with(".sqlite3")
                })
        })
        .unwrap_or_else(|| unreachable!("migration backup expected"));
    let backup =
        Connection::open(backup_path).unwrap_or_else(|error| unreachable!("backup: {error}"));
    let marker: String = backup
        .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
        .unwrap_or_else(|error| unreachable!("backup marker: {error}"));
    assert_eq!(marker, "recoverable");
}
