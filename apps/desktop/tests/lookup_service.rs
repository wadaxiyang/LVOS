use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lvos::{DatabaseWorker, LookupMode, LookupService};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::ProfileMetadata;
use lvos_translation::{
    ProviderId, ProviderRegistry, RouterSettings, TranslationError, TranslationProvider,
    TranslationRequest, TranslationResult, TranslationRouter,
};
use tempfile::tempdir;
use uuid::Uuid;

fn language(value: &str) -> LanguageCode {
    LanguageCode::parse(value).unwrap_or_else(|error| unreachable!("fixture: {error}"))
}

fn content() -> lvos_core::PreparedContent {
    prepare_content(
        "cache invariant",
        language("en"),
        ValidationPolicy::new(NonZeroUsize::new(2_000).unwrap_or_else(|| unreachable!("fixture"))),
    )
    .unwrap_or_else(|error| unreachable!("fixture: {error}"))
}

fn metadata() -> ProfileMetadata {
    ProfileMetadata {
        profile_id: Uuid::new_v4(),
        user_id: None,
        username: None,
        device_id: Uuid::new_v4(),
        platform: "macos".to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: UnixTimestamp::from_seconds(1),
        updated_at: UnixTimestamp::from_seconds(1),
    }
}

#[derive(Debug)]
struct CountingProvider {
    calls: Mutex<u64>,
}

#[async_trait]
impl TranslationProvider for CountingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("counting")
    }

    async fn translate(
        &self,
        _request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError> {
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *calls += 1;
        Ok(TranslationResult {
            text: format!("translation-{calls}"),
            provider: self.id(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_and_refresh_preserve_query_and_favorite_invariants() {
    let root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let worker = Arc::new(
        DatabaseWorker::start(root.path().to_path_buf())
            .unwrap_or_else(|error| unreachable!("worker: {error}")),
    );
    worker
        .switch_profile(metadata())
        .await
        .unwrap_or_else(|error| unreachable!("profile: {error}"));

    let provider = Arc::new(CountingProvider {
        calls: Mutex::new(0),
    });
    let mut registry = ProviderRegistry::default();
    registry.register(provider.clone());
    let router = TranslationRouter::new(
        &registry,
        &RouterSettings {
            primary: ProviderId::new("counting"),
            fallback: None,
        },
    )
    .unwrap_or_else(|error| unreachable!("router: {error}"));
    let service = LookupService::new(worker.clone(), router);

    let first = service
        .lookup(
            content(),
            language("zh-CN"),
            LookupMode::UseCache,
            UnixTimestamp::from_seconds(10),
        )
        .await
        .unwrap_or_else(|error| unreachable!("lookup: {error}"));
    assert!(!first.cache_hit);
    assert_eq!(first.history.translation.translation, "translation-1");

    let second = service
        .lookup(
            content(),
            language("zh-CN"),
            LookupMode::UseCache,
            UnixTimestamp::from_seconds(20),
        )
        .await
        .unwrap_or_else(|error| unreachable!("lookup: {error}"));
    assert!(second.cache_hit);
    assert_eq!(second.history.translation.translation, "translation-1");
    assert_eq!(second.query_stats.device_query_count, 2);
    assert_eq!(
        *provider
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        1
    );

    let key = content().content_key();
    let favorite = worker
        .execute(move |database| database.favorite(key, UnixTimestamp::from_seconds(21)))
        .await
        .unwrap_or_else(|error| unreachable!("favorite: {error}"));
    assert_eq!(favorite.translation.translation, "translation-1");

    let refreshed = service
        .lookup(
            content(),
            language("zh-CN"),
            LookupMode::Refresh,
            UnixTimestamp::from_seconds(30),
        )
        .await
        .unwrap_or_else(|error| unreachable!("refresh: {error}"));
    assert!(!refreshed.cache_hit);
    assert_eq!(refreshed.history.translation.translation, "translation-2");
    assert_eq!(refreshed.query_stats.device_query_count, 3);

    let key = content().content_key();
    let persisted_favorite = worker
        .execute(move |database| database.favorite_by_key(key))
        .await
        .unwrap_or_else(|error| unreachable!("favorite: {error}"))
        .unwrap_or_else(|| unreachable!("favorite disappeared"));
    assert_eq!(persisted_favorite.translation.translation, "translation-1");
    assert_eq!(
        *provider
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        2
    );
}
