use std::num::NonZeroUsize;

use lvos_core::{
    LanguageCode, MAX_PORTABLE_JSON_BYTES, UnixTimestamp, ValidationPolicy, prepare_content,
};
use lvos_storage::{
    HistoryEntry, OutboxOperation, PortableDataError, PortableDataExport, ProfileDatabase,
    ProfileMetadata, ProfilePaths, StoredContent, TranslationSnapshot,
};
use tempfile::tempdir;
use uuid::Uuid;

fn metadata(profile_id: Uuid) -> ProfileMetadata {
    ProfileMetadata {
        profile_id,
        user_id: Some(Uuid::new_v4()),
        username: Some("portable fixture".to_owned()),
        device_id: Uuid::new_v4(),
        platform: "macos".to_owned(),
        server_origin: Some("https://server.invalid".to_owned()),
        last_server_revision: 47,
        created_at: UnixTimestamp::from_seconds(100),
        updated_at: UnixTimestamp::from_seconds(100),
    }
}

fn open_database(root: &std::path::Path) -> ProfileDatabase {
    let profile_id = Uuid::new_v4();
    ProfileDatabase::open(ProfilePaths::new(root, profile_id), &metadata(profile_id))
        .unwrap_or_else(|error| unreachable!("fixture database: {error}"))
}

fn history(source: &str, translation: &str, timestamp: i64) -> HistoryEntry {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(
            NonZeroUsize::new(16_384).unwrap_or_else(|| unreachable!("fixture limit")),
        ),
    )
    .unwrap_or_else(|error| unreachable!("fixture content: {error}"));
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
fn portable_round_trip_excludes_identity_and_keeps_archives_out_of_device_stats() {
    let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let mut source = open_database(source_root.path());
    let active = history("Portable active", "活动", 200);
    let tombstone = history("Portable tombstone", "墓碑", 300);
    for entry in [&active, &tombstone] {
        source
            .record_successful_query(entry)
            .unwrap_or_else(|error| unreachable!("query: {error}"));
        source
            .favorite(
                entry.content.content_key,
                UnixTimestamp::from_seconds(entry.last_queried_at.as_seconds() + 1),
            )
            .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    }
    source
        .unfavorite(
            tombstone.content.content_key,
            UnixTimestamp::from_seconds(302),
        )
        .unwrap_or_else(|error| unreachable!("unfavorite: {error}"));

    let bytes = source
        .export_portable_json()
        .unwrap_or_else(|error| unreachable!("export: {error}"));
    let json = String::from_utf8(bytes.clone())
        .unwrap_or_else(|error| unreachable!("UTF-8 export: {error}"));
    for forbidden in [
        "device_id",
        "user_id",
        "server_origin",
        "last_server_revision",
        "event_id",
        "access_token",
        "refresh_token",
        "api_key",
    ] {
        assert!(!json.contains(forbidden), "export leaked {forbidden}");
    }

    let mut target = open_database(target_root.path());
    let plan = target
        .preview_portable_import(&bytes)
        .unwrap_or_else(|error| unreachable!("preview: {error}"));
    let preview = plan.preview();
    assert_eq!(preview.history_add, 2);
    assert_eq!(preview.favorite_add, 1);
    assert_eq!(preview.tombstone_archive, 1);
    assert_eq!(preview.query_stats_archive, 2);
    let result = target
        .apply_portable_import(plan, UnixTimestamp::from_seconds(400))
        .unwrap_or_else(|error| unreachable!("import: {error}"));
    assert_eq!(result, preview);

    assert!(
        target
            .favorite_by_key(active.content.content_key)
            .unwrap_or_default()
            .is_some_and(|favorite| favorite.deleted_at.is_none())
    );
    assert!(
        target
            .favorite_by_key(tombstone.content.content_key)
            .unwrap_or_default()
            .is_some_and(|favorite| favorite.deleted_at.is_some())
    );
    for key in [active.content.content_key, tombstone.content.content_key] {
        let stats = target
            .query_stats(key)
            .unwrap_or_default()
            .unwrap_or_else(|| unreachable!("imported QueryStats shell"));
        assert_eq!(stats.device_query_count, 0);
        assert_eq!(stats.effective_total(), 0);
    }
    let events = target.outbox_events().unwrap_or_default();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, OutboxOperation::FavoriteUpsert);
}

#[test]
fn import_uses_newer_history_but_preserves_an_active_local_favorite_snapshot() {
    let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let mut source = open_database(source_root.path());
    let imported = history("Merge identity", "导入的新历史", 300);
    let imported_equal = history("Equal timestamp", "导入但不应覆盖", 250);
    source
        .record_successful_query(&imported)
        .unwrap_or_else(|error| unreachable!("source query: {error}"));
    source
        .record_successful_query(&imported_equal)
        .unwrap_or_else(|error| unreachable!("source equal query: {error}"));
    source
        .favorite(
            imported.content.content_key,
            UnixTimestamp::from_seconds(301),
        )
        .unwrap_or_else(|error| unreachable!("source favorite: {error}"));
    let bytes = source
        .export_portable_json()
        .unwrap_or_else(|error| unreachable!("export: {error}"));

    let mut target = open_database(target_root.path());
    let local = history("Merge identity", "本地收藏快照", 200);
    let local_equal = history("Equal timestamp", "本地同时间胜出", 250);
    target
        .record_successful_query(&local)
        .unwrap_or_else(|error| unreachable!("target query: {error}"));
    target
        .record_successful_query(&local_equal)
        .unwrap_or_else(|error| unreachable!("target equal query: {error}"));
    target
        .favorite(local.content.content_key, UnixTimestamp::from_seconds(201))
        .unwrap_or_else(|error| unreachable!("target favorite: {error}"));
    let prior_outbox = target.outbox_events().unwrap_or_default().len();

    let plan = target
        .preview_portable_import(&bytes)
        .unwrap_or_else(|error| unreachable!("preview: {error}"));
    assert_eq!(plan.preview().history_update, 1);
    assert_eq!(plan.preview().history_skip, 1);
    assert_eq!(plan.preview().favorite_skip, 1);
    target
        .apply_portable_import(plan, UnixTimestamp::from_seconds(400))
        .unwrap_or_else(|error| unreachable!("apply: {error}"));

    let merged_history = target
        .history(local.content.content_key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("merged history"));
    assert_eq!(merged_history.translation.translation, "导入的新历史");
    let equal_history = target
        .history(local_equal.content.content_key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("equal history"));
    assert_eq!(equal_history.translation.translation, "本地同时间胜出");
    let favorite = target
        .favorite_by_key(local.content.content_key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("local favorite"));
    assert_eq!(favorite.translation.translation, "本地收藏快照");
    assert_eq!(
        target.outbox_events().unwrap_or_default().len(),
        prior_outbox
    );
}

#[test]
fn malformed_oversized_and_identity_inconsistent_documents_are_rejected_before_mutation() {
    let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let mut source = open_database(source_root.path());
    source
        .record_successful_query(&history("Validation", "验证", 200))
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    let bytes = source
        .export_portable_json()
        .unwrap_or_else(|error| unreachable!("export: {error}"));
    let target = open_database(target_root.path());

    assert!(matches!(
        target.preview_portable_import(b"{"),
        Err(PortableDataError::Json(_))
    ));
    assert!(matches!(
        target.preview_portable_import(&vec![b' '; MAX_PORTABLE_JSON_BYTES + 1]),
        Err(PortableDataError::TooLarge)
    ));

    let mut export: PortableDataExport = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| unreachable!("fixture JSON: {error}"));
    export.history[0].content.canonical_text.push('x');
    let invalid = serde_json::to_vec(&export)
        .unwrap_or_else(|error| unreachable!("invalid fixture JSON: {error}"));
    assert!(matches!(
        target.preview_portable_import(&invalid),
        Err(PortableDataError::Invalid("content identity mismatch"))
    ));

    let mut duplicate: PortableDataExport = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| unreachable!("fixture JSON: {error}"));
    duplicate.history.push(duplicate.history[0].clone());
    let duplicate = serde_json::to_vec(&duplicate)
        .unwrap_or_else(|error| unreachable!("duplicate fixture JSON: {error}"));
    assert!(matches!(
        target.preview_portable_import(&duplicate),
        Err(PortableDataError::Invalid("duplicate History content key"))
    ));
    assert!(target.search_history("", 20).unwrap_or_default().is_empty());
    assert!(target.outbox_events().unwrap_or_default().is_empty());
}

#[test]
fn imported_tombstone_never_cancels_an_active_local_favorite() {
    let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let mut source = open_database(source_root.path());
    let imported = history("Local favorite survives", "导入墓碑", 300);
    let key = imported.content.content_key;
    source
        .record_successful_query(&imported)
        .unwrap_or_else(|error| unreachable!("source query: {error}"));
    source
        .favorite(key, UnixTimestamp::from_seconds(301))
        .unwrap_or_else(|error| unreachable!("source favorite: {error}"));
    source
        .unfavorite(key, UnixTimestamp::from_seconds(302))
        .unwrap_or_else(|error| unreachable!("source unfavorite: {error}"));
    let bytes = source
        .export_portable_json()
        .unwrap_or_else(|error| unreachable!("export: {error}"));

    let mut target = open_database(target_root.path());
    let local = history("Local favorite survives", "本地活动收藏", 200);
    target
        .record_successful_query(&local)
        .unwrap_or_else(|error| unreachable!("target query: {error}"));
    target
        .favorite(key, UnixTimestamp::from_seconds(201))
        .unwrap_or_else(|error| unreachable!("target favorite: {error}"));
    let plan = target
        .preview_portable_import(&bytes)
        .unwrap_or_else(|error| unreachable!("preview: {error}"));
    assert_eq!(plan.preview().favorite_skip, 1);
    let result = target
        .apply_portable_import(plan, UnixTimestamp::from_seconds(400))
        .unwrap_or_else(|error| unreachable!("apply: {error}"));
    assert_eq!(result.favorite_skip, 1);
    let favorite = target
        .favorite_by_key(key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("active local favorite"));
    assert!(favorite.deleted_at.is_none());
    assert_eq!(favorite.translation.translation, "本地活动收藏");
}

#[test]
fn apply_rechecks_a_preview_after_intervening_local_edits() {
    let source_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let target_root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let mut source = open_database(source_root.path());
    let imported = history("Stale preview", "预览时较新", 200);
    let key = imported.content.content_key;
    source
        .record_successful_query(&imported)
        .unwrap_or_else(|error| unreachable!("source query: {error}"));
    let bytes = source
        .export_portable_json()
        .unwrap_or_else(|error| unreachable!("export: {error}"));

    let mut target = open_database(target_root.path());
    let plan = target
        .preview_portable_import(&bytes)
        .unwrap_or_else(|error| unreachable!("preview: {error}"));
    assert_eq!(plan.preview().history_add, 1);
    let intervening = history("Stale preview", "应用前的本地更新", 300);
    target
        .record_successful_query(&intervening)
        .unwrap_or_else(|error| unreachable!("intervening query: {error}"));
    let result = target
        .apply_portable_import(plan, UnixTimestamp::from_seconds(400))
        .unwrap_or_else(|error| unreachable!("apply: {error}"));
    assert_eq!(result.history_add, 0);
    assert_eq!(result.history_skip, 1);
    let retained = target
        .history(key)
        .unwrap_or_default()
        .unwrap_or_else(|| unreachable!("retained history"));
    assert_eq!(retained.translation.translation, "应用前的本地更新");
}
