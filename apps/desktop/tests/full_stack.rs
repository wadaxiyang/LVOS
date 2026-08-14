use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use lvos::{DesktopApplication, HttpSyncTransport, TransportError};
use lvos_auth::{AuthError, CredentialScope, CredentialStore};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_server::{AppEnvironment, ServerConfig};
use lvos_storage::{HistoryEntry, Platform, StoredContent, TranslationSnapshot};
use tempfile::tempdir;

async fn compatibility_fixture(
    api_version: &'static str,
    minimum_desktop_version: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        "/api/v1/health",
        axum::routing::get(move || async move {
            axum::Json(serde_json::json!({
                "status": "ok",
                "server_api_version": api_version,
                "server_version": "0.1.0",
                "minimum_desktop_version": minimum_desktop_version,
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap_or_else(|error| unreachable!("fixture listener: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| unreachable!("fixture address: {error}"));
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .unwrap_or_else(|error| unreachable!("fixture server: {error}"));
    });
    (format!("http://{address}"), task)
}

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

fn config(root: &std::path::Path) -> ServerConfig {
    ServerConfig {
        environment: AppEnvironment::Development,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        database_url: format!("sqlite://{}", root.join("server.sqlite3").display()),
        public_server_url: "http://127.0.0.1".to_owned(),
        bootstrap_default_user: true,
        default_username: "integration".to_owned(),
        default_password: Some("integration-password".to_owned()),
        access_token_ttl_seconds: 3_600,
        refresh_idle_ttl_seconds: 90 * 24 * 60 * 60,
        login_rate_limit_enabled: true,
        login_rate_limit_max_failures: 5,
        login_rate_limit_window_seconds: 60,
        max_request_body_bytes: 1_048_576,
        max_sync_body_bytes: 1_048_576,
        max_sync_events_per_batch: 500,
        sync_changes_default_limit: 100,
        sync_changes_max_limit: 500,
        backup_enabled: false,
        backup_dir: root.join("backups"),
        backup_retention_count: 3,
        backup_interval_seconds: 86_400,
    }
}

fn history(source: &str) -> HistoryEntry {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(NonZeroUsize::new(2_000).unwrap_or_else(|| unreachable!("fixture"))),
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
            translation: "全栈".to_owned(),
            provider: "fixture".to_owned(),
            updated_at: UnixTimestamp::from_seconds(1_780_000_000),
        },
        last_queried_at: UnixTimestamp::from_seconds(1_780_000_000),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn real_http_server_and_two_desktop_databases_converge() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let server_config = config(directory.path());
    let repository = lvos_server::ServerRepository::open(
        &server_config.database_url,
        &server_config.backup_dir,
        server_config.backup_retention_count,
        1_780_000_000,
    )
    .unwrap_or_else(|error| unreachable!("repository: {error}"));
    let app = lvos_server::build_app(server_config, repository)
        .await
        .unwrap_or_else(|error| unreachable!("server: {error}"));
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap_or_else(|error| unreachable!("listener: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| unreachable!("address: {error}"));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .unwrap_or_else(|error| unreachable!("serve: {error}"));
    });
    let origin = format!("http://{address}");
    let transport = Arc::new(
        HttpSyncTransport::new().unwrap_or_else(|error| unreachable!("transport: {error}")),
    );
    let compatibility = transport
        .compatibility(&origin)
        .await
        .unwrap_or_else(|error| unreachable!("compatibility: {error}"));
    assert_eq!(compatibility.server_api_version, lvos_core::API_VERSION);

    let first_credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentials::default());
    let first_application = DesktopApplication::open(
        directory.path().join("first"),
        Platform::Macos,
        "first",
        first_credentials,
    )
    .await
    .unwrap_or_else(|error| unreachable!("first application: {error}"));
    first_application
        .login(
            origin.clone(),
            "integration".to_owned(),
            "integration-password".to_owned(),
        )
        .await
        .unwrap_or_else(|error| unreachable!("first login: {error}"));
    let user_id = first_application
        .profile()
        .user_id
        .unwrap_or_else(|| unreachable!("bound profile has no user"));
    let entry = history("full stack");
    let key = entry.content.content_key;
    first_application
        .database()
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|error| unreachable!("first mutation: {error}"));
    first_application
        .set_favorite(key.to_string(), true)
        .await
        .unwrap_or_else(|error| unreachable!("first favorite: {error}"));
    assert!(first_application.manual_sync().await);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let diagnostics = first_application
                .sync_diagnostics()
                .await
                .unwrap_or_else(|error| unreachable!("diagnostics: {error}"));
            if diagnostics.pending_outbox == 0 && diagnostics.last_server_revision > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| unreachable!("production sync did not become idle"));

    let second_credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentials::default());
    let second_application = DesktopApplication::open(
        directory.path().join("second"),
        Platform::Macos,
        "second",
        second_credentials,
    )
    .await
    .unwrap_or_else(|error| unreachable!("second application: {error}"));
    second_application
        .login(
            origin,
            "integration".to_owned(),
            "integration-password".to_owned(),
        )
        .await
        .unwrap_or_else(|error| unreachable!("second login: {error}"));
    assert_eq!(second_application.profile().user_id, Some(user_id));
    assert!(second_application.manual_sync().await);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if second_application
                .database()
                .execute(move |database| database.favorite_by_key(key))
                .await
                .ok()
                .flatten()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| unreachable!("second production Desktop did not converge"));
    let favorite = second_application
        .database()
        .execute(move |database| database.favorite_by_key(key))
        .await
        .unwrap_or_else(|error| unreachable!("second read: {error}"))
        .unwrap_or_else(|| unreachable!("favorite did not converge"));
    assert!(favorite.deleted_at.is_none());
    assert_eq!(favorite.translation.translation, "全栈");

    server.abort();
}

#[tokio::test]
async fn compatibility_boundary_rejects_wrong_api_and_too_old_desktop() {
    let transport =
        HttpSyncTransport::new().unwrap_or_else(|error| unreachable!("transport: {error}"));
    let (origin, task) = compatibility_fixture("v2", "0.1.0").await;
    assert!(matches!(
        transport.compatibility(&origin).await,
        Err(TransportError::IncompatibleServer)
    ));
    task.abort();

    let (origin, task) = compatibility_fixture("v1", "999.0.0").await;
    assert!(matches!(
        transport.compatibility(&origin).await,
        Err(TransportError::IncompatibleServer)
    ));
    task.abort();
}
