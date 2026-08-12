use std::{num::NonZeroUsize, sync::Arc};

use lvos::{DatabaseWorker, UiDataError, UiDataService};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{HistoryEntry, ProfileMetadata, StoredContent, TranslationSnapshot};
use tempfile::tempdir;
use uuid::Uuid;

fn metadata() -> ProfileMetadata {
    ProfileMetadata {
        profile_id: Uuid::new_v4(),
        user_id: None,
        username: None,
        device_id: Uuid::new_v4(),
        platform: "macos".to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: UnixTimestamp::from_seconds(1_780_000_000),
        updated_at: UnixTimestamp::from_seconds(1_780_000_000),
    }
}

fn history(source: &str, translation: &str) -> HistoryEntry {
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
            updated_at: UnixTimestamp::from_seconds(1_780_000_001),
        },
        last_queried_at: UnixTimestamp::from_seconds(1_780_000_001),
    }
}

#[tokio::test]
async fn ui_data_actions_run_through_profile_database_worker() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let worker = Arc::new(
        DatabaseWorker::start(directory.path().to_path_buf())
            .unwrap_or_else(|error| unreachable!("worker: {error}")),
    );
    worker
        .switch_profile(metadata())
        .await
        .unwrap_or_else(|error| unreachable!("profile: {error}"));
    let entry = history("Invariant", "不变的");
    let key = entry.content.content_key;
    worker
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|error| unreachable!("seed: {error}"));

    let service = UiDataService::new(worker);
    let records = service
        .history("不变".to_owned(), 20)
        .await
        .unwrap_or_else(|error| unreachable!("history: {error}"));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].count, 1);
    assert!(!records[0].favorite);

    assert!(
        service
            .set_favorite(
                key.to_string(),
                true,
                UnixTimestamp::from_seconds(1_780_000_002)
            )
            .await
            .unwrap_or_else(|error| unreachable!("favorite: {error}"))
    );
    let favorites = service
        .favorites(String::new(), 20)
        .await
        .unwrap_or_else(|error| unreachable!("favorites: {error}"));
    assert_eq!(favorites.len(), 1);
    assert!(favorites[0].favorite);

    service
        .clear_history()
        .await
        .unwrap_or_else(|error| unreachable!("clear: {error}"));
    assert!(
        service
            .history(String::new(), 20)
            .await
            .unwrap_or_default()
            .is_empty()
    );
    assert_eq!(
        service
            .favorites(String::new(), 20)
            .await
            .unwrap_or_default()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_ui_content_identity_is_rejected_before_database_mutation() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let worker = Arc::new(
        DatabaseWorker::start(directory.path().to_path_buf())
            .unwrap_or_else(|error| unreachable!("worker: {error}")),
    );
    let service = UiDataService::new(worker);
    assert!(matches!(
        service
            .set_favorite("not-a-key".to_owned(), true, UnixTimestamp::from_seconds(1))
            .await,
        Err(UiDataError::InvalidContentKey)
    ));
}
