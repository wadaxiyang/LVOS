use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use lvos::{
    DesktopApplication, LookupCardState, LookupMode, NetworkPreferences, ProviderPreferences,
    ProxyKind,
};
use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{HistoryEntry, Platform, StoredContent, TranslationSnapshot};
use tempfile::tempdir;

#[derive(Debug, Default)]
struct MemoryCredentials(Mutex<HashMap<CredentialScope, Vec<u8>>>);

impl CredentialStore for MemoryCredentials {
    fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| AuthError::CredentialStore)?
            .get(scope)
            .cloned())
    }

    fn contains(&self, scope: &CredentialScope) -> Result<bool, AuthError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| AuthError::CredentialStore)?
            .contains_key(scope))
    }

    fn set(&self, scope: &CredentialScope, secret: &[u8]) -> Result<(), AuthError> {
        self.0
            .lock()
            .map_err(|_| AuthError::CredentialStore)?
            .insert(scope.clone(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError> {
        self.0
            .lock()
            .map_err(|_| AuthError::CredentialStore)?
            .remove(scope);
        Ok(())
    }
}

fn language(value: &str) -> LanguageCode {
    LanguageCode::parse(value).unwrap_or_else(|error| unreachable!("fixture: {error}"))
}

fn assert_rejected_provider_settings_are_atomic(application: &DesktopApplication) {
    assert!(
        application
            .save_provider_settings(
                ProviderPreferences {
                    tokenhub_model: "invalid model".to_owned(),
                },
                "must-not-be-partially-stored",
            )
            .is_err()
    );
    assert!(
        !application.provider_configuration().unwrap_or_default(),
        "rejected TokenHub settings must not partially mutate credentials"
    );
}

#[test]
fn legacy_provider_preferences_default_the_new_tokenhub_model() {
    let preferences: ProviderPreferences =
        serde_json::from_str(r#"{"primary":"retired-provider","fallback":null}"#)
            .unwrap_or_else(|error| unreachable!("legacy settings: {error}"));
    assert_eq!(
        preferences.tokenhub_model,
        lvos_translation::DEFAULT_TOKENHUB_MODEL
    );
    assert_eq!(
        serde_json::to_value(preferences)
            .unwrap_or_else(|error| unreachable!("normalized settings: {error}")),
        serde_json::json!({"tokenhub_model": "hy-mt2-lite"})
    );
}

#[tokio::test]
async fn production_composition_keeps_cache_available_without_provider_and_secrets_out_of_files() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let credentials = Arc::new(MemoryCredentials::default());
    let store: Arc<dyn CredentialStore> = credentials.clone();
    let application = DesktopApplication::open(
        directory.path().to_path_buf(),
        Platform::Macos,
        "integration-mac",
        store,
    )
    .await
    .unwrap_or_else(|error| unreachable!("application: {error}"));
    assert!(!application.provider_configuration().unwrap_or_default());

    let prepared = prepare_content(
        "cached integration",
        language("en"),
        ValidationPolicy::new(NonZeroUsize::new(2_000).unwrap_or_else(|| unreachable!("fixture"))),
    )
    .unwrap_or_else(|error| unreachable!("content: {error}"));
    let entry = HistoryEntry {
        content: StoredContent {
            content_key: prepared.content_key(),
            key_version: prepared.key_version(),
            kind: prepared.kind(),
            source_lang: prepared.source_lang().clone(),
            source_text: prepared.source_text().to_owned(),
            canonical_text: prepared.canonical_text().to_owned(),
        },
        translation: TranslationSnapshot {
            target_lang: language("zh-CN"),
            translation: "缓存集成".to_owned(),
            provider: "fixture".to_owned(),
            updated_at: UnixTimestamp::from_seconds(10),
        },
        last_queried_at: UnixTimestamp::from_seconds(10),
    };
    application
        .database()
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|error| unreachable!("seed: {error}"));

    let cached = application
        .lookup("cached integration".to_owned(), LookupMode::UseCache)
        .await;
    assert!(
        matches!(cached, LookupCardState::Ready { translation, .. } if translation == "缓存集成")
    );
    let missing = application
        .lookup("uncached integration".to_owned(), LookupMode::UseCache)
        .await;
    assert!(matches!(
        missing,
        LookupCardState::Error {
            kind: lvos_translation::LookupCardErrorKind::ProviderConfigurationRequired,
            ..
        }
    ));

    assert_rejected_provider_settings_are_atomic(&application);

    let profile = application.profile();
    let retired_scope = CredentialScope {
        server_origin: "lvos://translation-provider".to_owned(),
        user_id: profile.profile_id.to_string(),
        device_id: profile.device_id.to_string(),
        key: CredentialKey::RetiredTranslationApiKey,
    };
    credentials
        .set(&retired_scope, b"retired-secret")
        .unwrap_or_else(|error| unreachable!("retired fixture: {error}"));

    application
        .save_provider_settings(
            ProviderPreferences {
                tokenhub_model: "organization/custom-translation-v1".to_owned(),
            },
            "tokenhub-integration-secret",
        )
        .unwrap_or_else(|error| unreachable!("settings: {error}"));
    assert!(application.provider_configuration().unwrap_or_default());
    assert!(
        !credentials
            .contains(&retired_scope)
            .unwrap_or_else(|error| unreachable!("retired cleanup: {error}"))
    );

    assert_provider_settings_are_secret_free_and_include_model(directory.path());
}

#[tokio::test]
async fn network_proxy_preferences_persist_without_secrets() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let credentials = Arc::new(MemoryCredentials::default());
    let store: Arc<dyn CredentialStore> = credentials.clone();
    let application = DesktopApplication::open(
        directory.path().to_path_buf(),
        Platform::Macos,
        "integration-mac",
        Arc::clone(&store) as Arc<dyn CredentialStore>,
    )
    .await
    .unwrap_or_else(|error| unreachable!("application: {error}"));
    application
        .save_network_preferences(NetworkPreferences {
            proxy_kind: ProxyKind::Socks5,
            proxy_address: "127.0.0.1:1080".to_owned(),
            provider_proxy_enabled: true,
            update_proxy_enabled: true,
        })
        .unwrap_or_else(|error| unreachable!("network settings: {error}"));
    drop(application);

    let reopened = DesktopApplication::open(
        directory.path().to_path_buf(),
        Platform::Macos,
        "integration-mac",
        store,
    )
    .await
    .unwrap_or_else(|error| unreachable!("reopen: {error}"));
    assert_eq!(
        reopened.network_preferences(),
        NetworkPreferences {
            proxy_kind: ProxyKind::Socks5,
            proxy_address: "127.0.0.1:1080".to_owned(),
            provider_proxy_enabled: true,
            update_proxy_enabled: true,
        }
    );
    let saved = std::fs::read_to_string(directory.path().join("network-settings.json"))
        .unwrap_or_else(|error| unreachable!("network settings file: {error}"));
    assert!(saved.contains("127.0.0.1:1080"));
}

fn assert_provider_settings_are_secret_free_and_include_model(root: &std::path::Path) {
    let mut model_persisted = false;
    for entry in std::fs::read_dir(root).unwrap_or_else(|error| unreachable!("directory: {error}"))
    {
        let path = entry
            .unwrap_or_else(|error| unreachable!("entry: {error}"))
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let bytes =
                std::fs::read(path).unwrap_or_else(|error| unreachable!("settings read: {error}"));
            assert!(
                !bytes
                    .windows(b"integration-secret".len())
                    .any(|window| window == b"integration-secret")
            );
            model_persisted |= bytes
                .windows(b"organization/custom-translation-v1".len())
                .any(|window| window == b"organization/custom-translation-v1");
        }
    }
    assert!(
        model_persisted,
        "the non-secret Profile model setting must persist"
    );
}
