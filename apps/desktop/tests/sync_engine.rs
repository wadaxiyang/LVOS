use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use lvos::{
    AuthenticatedSession, DatabaseWorker, FavoriteConflict, LoginCredentials, LoginIdentity,
    ProfileLifecycle, RefreshedTokens, RemoteDevice, RevisionStreamEvent, SyncEngine,
    SyncProfileServices, SyncRunOutcome, SyncTransport, SyncWorker, TransportError,
};
use lvos_auth::{AuthError, CredentialScope, CredentialStore};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{HistoryEntry, ProfileMetadata, StoredContent, TranslationSnapshot};
use lvos_sync::{
    AckStatus, ChangesResponse, FavoriteRecord, FavoriteSnapshot, PushAck, PushRequest,
    PushResponse, SyncChange, SyncOperation,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

#[derive(Clone, Debug)]
enum PushBehavior {
    Acknowledge,
    OfflineAfterApply,
    Conflict(Box<FavoriteRecord>),
    Revoked,
}

#[derive(Debug, Default)]
struct FakeTransport {
    push_behaviors: Mutex<VecDeque<PushBehavior>>,
    change_behaviors: Mutex<VecDeque<Result<ChangesResponse, TransportError>>>,
    pushed_event_ids: Mutex<Vec<Vec<String>>>,
    requested_cursors: Mutex<Vec<u64>>,
    disconnect_first_stream: AtomicBool,
    stream_attempts: AtomicUsize,
}

impl FakeTransport {
    fn push_behavior(&self, behavior: PushBehavior) {
        self.push_behaviors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(behavior);
    }

    fn changes(&self, response: Result<ChangesResponse, TransportError>) {
        self.change_behaviors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(response);
    }
}

#[async_trait]
impl SyncTransport for FakeTransport {
    async fn login(
        &self,
        _server_origin: &str,
        credentials: &LoginCredentials,
    ) -> Result<LoginIdentity, TransportError> {
        Ok(LoginIdentity {
            user_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            username: credentials.username.clone(),
            device_id: credentials.device_id.clone(),
            platform: credentials.platform.clone(),
            access_token: "access".to_owned(),
            access_expires_at: i64::MAX,
            refresh_token: "refresh".to_owned(),
            latest_revision: 0,
        })
    }

    async fn refresh(
        &self,
        _server_origin: &str,
        _refresh_token: &str,
    ) -> Result<RefreshedTokens, TransportError> {
        unreachable!("fixture access token does not expire")
    }

    async fn logout(
        &self,
        _server_origin: &str,
        _access_token: &str,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    async fn push(
        &self,
        _server_origin: &str,
        _access_token: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let event_ids = request
            .events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<Vec<_>>();
        self.pushed_event_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event_ids);
        match self
            .push_behaviors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or(PushBehavior::Acknowledge)
        {
            PushBehavior::Acknowledge => Ok(PushResponse {
                acknowledgements: request
                    .events
                    .iter()
                    .map(|event| PushAck {
                        event_id: event.event_id.clone(),
                        status: AckStatus::Applied,
                        entity_revision: Some(1),
                        user_revision: 1,
                        aggregate_query_stats: None,
                    })
                    .collect(),
                latest_revision: 1,
            }),
            PushBehavior::OfflineAfterApply => Err(TransportError::Offline),
            PushBehavior::Conflict(current) => {
                Err(TransportError::Conflict(Box::new(FavoriteConflict {
                    event_id: request.events.first().map(|event| event.event_id.clone()),
                    current: Some(*current),
                    latest_revision: 1,
                })))
            }
            PushBehavior::Revoked => Err(TransportError::DeviceRevoked),
        }
    }

    async fn changes(
        &self,
        _server_origin: &str,
        _access_token: &str,
        since: u64,
        _limit: u32,
    ) -> Result<ChangesResponse, TransportError> {
        self.requested_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(since);
        self.change_behaviors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Ok(ChangesResponse {
                    changes: Vec::new(),
                    next_revision: since,
                    latest_revision: since,
                    has_more: false,
                })
            })
    }

    async fn devices(
        &self,
        _server_origin: &str,
        _access_token: &str,
    ) -> Result<Vec<RemoteDevice>, TransportError> {
        Ok(Vec::new())
    }

    async fn revoke_device(
        &self,
        _server_origin: &str,
        _access_token: &str,
        _device_id: &str,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    async fn revision_stream(
        &self,
        _server_origin: &str,
        _access_token: &str,
        events: mpsc::Sender<RevisionStreamEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), TransportError> {
        self.stream_attempts.fetch_add(1, Ordering::SeqCst);
        if events.send(RevisionStreamEvent::Connected).await.is_err() {
            return Ok(());
        }
        if self.disconnect_first_stream.swap(false, Ordering::SeqCst) {
            let _ = events.send(RevisionStreamEvent::Revision(1)).await;
            return Err(TransportError::Offline);
        }
        cancellation.cancelled().await;
        Ok(())
    }
}

struct Fixture {
    _directory: TempDir,
    root: std::path::PathBuf,
    metadata: ProfileMetadata,
    worker: Arc<DatabaseWorker>,
    session: Arc<AuthenticatedSession<FakeTransport>>,
    transport: Arc<FakeTransport>,
}

impl Fixture {
    async fn new() -> Self {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
        let root = directory.path().to_path_buf();
        let metadata = ProfileMetadata {
            profile_id: Uuid::new_v4(),
            user_id: None,
            username: None,
            device_id: Uuid::new_v4(),
            platform: "macos".to_owned(),
            server_origin: None,
            last_server_revision: 0,
            created_at: UnixTimestamp::from_seconds(1_780_000_000),
            updated_at: UnixTimestamp::from_seconds(1_780_000_000),
        };
        let worker = Arc::new(
            DatabaseWorker::start(root.clone())
                .unwrap_or_else(|error| unreachable!("worker fixture: {error}")),
        );
        worker
            .switch_profile(metadata.clone())
            .await
            .unwrap_or_else(|error| unreachable!("profile fixture: {error}"));
        let transport = Arc::new(FakeTransport::default());
        let credential_store: Arc<dyn CredentialStore> = Arc::new(MemoryCredentials::default());
        let (session, _) = AuthenticatedSession::login(
            Arc::clone(&transport),
            credential_store,
            "https://sync.example".to_owned(),
            &LoginCredentials {
                username: "alice".to_owned(),
                password: "password".to_owned(),
                device_id: metadata.device_id.to_string(),
                platform: metadata.platform.clone(),
                device_name: None,
            },
        )
        .await
        .unwrap_or_else(|error| unreachable!("session fixture: {error}"));
        Self {
            _directory: directory,
            root,
            metadata,
            worker,
            session: Arc::new(session),
            transport,
        }
    }

    async fn seed_favorite(&self) -> FavoriteRecord {
        let record = remote_record("Durable intent", "original", 1, None);
        let content_key = record.content_key.clone();
        let history = history("Durable intent", "original");
        self.worker
            .execute(move |database| {
                database.record_successful_query(&history)?;
                database.favorite(
                    content_key.parse().map_err(|_| {
                        lvos_storage::StorageError::InvalidData("fixture content key")
                    })?,
                    UnixTimestamp::from_seconds(1_780_000_002),
                )?;
                Ok(())
            })
            .await
            .unwrap_or_else(|error| unreachable!("seed fixture: {error}"));
        record
    }

    fn engine(&self) -> SyncEngine<FakeTransport> {
        SyncEngine::new(Arc::clone(&self.worker), Arc::clone(&self.session))
    }
}

fn history(source: &str, translation: &str) -> HistoryEntry {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN)),
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

fn remote_record(
    source: &str,
    translation: &str,
    entity_revision: u64,
    deleted_at: Option<i64>,
) -> FavoriteRecord {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN)),
    )
    .unwrap_or_else(|error| unreachable!("fixture: {error}"));
    FavoriteRecord {
        content_key: prepared.content_key().to_string(),
        key_version: prepared.key_version(),
        favorite: FavoriteSnapshot {
            kind: prepared.kind().protocol_name().to_owned(),
            source_lang: prepared.source_lang().to_string(),
            target_lang: "zh-CN".to_owned(),
            source_text: prepared.source_text().to_owned(),
            canonical_text: prepared.canonical_text().to_owned(),
            translation: translation.to_owned(),
            provider: "fixture".to_owned(),
            favorited_at: 1_780_000_002,
            updated_at: 1_780_000_002,
        },
        deleted_at,
        entity_revision,
        server_received_at: 1_780_000_003,
        aggregate_query_stats: None,
    }
}

#[tokio::test]
async fn ack_loss_survives_restart_and_reuses_the_same_event_id() {
    let mut fixture = Fixture::new().await;
    fixture.seed_favorite().await;
    fixture
        .transport
        .push_behavior(PushBehavior::OfflineAfterApply);
    assert!(matches!(
        fixture.engine().synchronize_once().await,
        Ok(SyncRunOutcome::RetryAt(_))
    ));
    let first_id = fixture
        .transport
        .pushed_event_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)[0][0]
        .clone();

    let worker = Arc::clone(&fixture.worker);
    drop(fixture.worker);
    drop(worker);
    fixture.worker = Arc::new(
        DatabaseWorker::start(fixture.root.clone())
            .unwrap_or_else(|error| unreachable!("restart fixture: {error}")),
    );
    fixture
        .worker
        .switch_profile(fixture.metadata.clone())
        .await
        .unwrap_or_else(|error| unreachable!("reopen fixture: {error}"));
    let retry_events = fixture
        .worker
        .execute(|database| database.outbox_events())
        .await
        .unwrap_or_else(|error| unreachable!("outbox fixture: {error}"));
    fixture
        .worker
        .execute(move |database| {
            database.mark_retry(
                &retry_events,
                UnixTimestamp::from_seconds(0),
                "manual_retry",
                UnixTimestamp::from_seconds(1),
            )
        })
        .await
        .unwrap_or_else(|error| unreachable!("retry fixture: {error}"));
    assert!(matches!(
        fixture.engine().synchronize_once().await,
        Ok(SyncRunOutcome::Idle)
    ));
    {
        let pushes = fixture
            .transport
            .pushed_event_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(pushes[1][0], first_id);
    }
    assert!(
        fixture
            .worker
            .execute(|database| database.outbox_events())
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

#[tokio::test]
async fn pagination_failure_keeps_committed_cursor_and_remote_apply_creates_no_outbox() {
    let fixture = Fixture::new().await;
    let first = remote_record("Remote only", "远端", 1, None);
    let second = remote_record("Remote only", "远端", 2, Some(1_780_000_010));
    fixture.transport.changes(Ok(ChangesResponse {
        changes: vec![SyncChange {
            revision: 1,
            operation: SyncOperation::FavoriteUpsert,
            favorite: first,
        }],
        next_revision: 1,
        latest_revision: 2,
        has_more: true,
    }));
    fixture.transport.changes(Err(TransportError::Offline));
    fixture
        .engine()
        .synchronize_once()
        .await
        .unwrap_or_else(|error| unreachable!("first pull: {error}"));
    let diagnostics = fixture
        .worker
        .execute(|database| database.sync_diagnostics())
        .await
        .unwrap_or_else(|error| unreachable!("diagnostics: {error}"));
    assert_eq!(diagnostics.last_server_revision, 1);
    fixture.transport.changes(Ok(ChangesResponse {
        changes: vec![SyncChange {
            revision: 2,
            operation: SyncOperation::FavoriteDelete,
            favorite: second,
        }],
        next_revision: 2,
        latest_revision: 2,
        has_more: false,
    }));
    fixture
        .engine()
        .synchronize_once()
        .await
        .unwrap_or_else(|error| unreachable!("second pull: {error}"));
    assert_eq!(
        *fixture
            .transport
            .requested_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![0, 1, 1]
    );
    assert!(
        fixture
            .worker
            .execute(|database| database.outbox_events())
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_empty_page_cannot_advance_the_durable_cursor() {
    let fixture = Fixture::new().await;
    fixture.transport.changes(Ok(ChangesResponse {
        changes: Vec::new(),
        next_revision: 9,
        latest_revision: 9,
        has_more: false,
    }));
    assert!(fixture.engine().synchronize_once().await.is_err());
    let diagnostics = fixture
        .worker
        .execute(|database| database.sync_diagnostics())
        .await
        .unwrap_or_else(|error| unreachable!("diagnostics: {error}"));
    assert_eq!(diagnostics.last_server_revision, 0);
}

#[tokio::test]
async fn favorite_conflict_replays_once_then_stays_visible_for_manual_retry() {
    let fixture = Fixture::new().await;
    fixture.seed_favorite().await;
    let conflicting = remote_record("Durable intent", "different remote value", 4, None);
    fixture
        .transport
        .push_behavior(PushBehavior::Conflict(Box::new(conflicting.clone())));
    fixture
        .transport
        .push_behavior(PushBehavior::Conflict(Box::new(FavoriteRecord {
            entity_revision: 5,
            ..conflicting
        })));
    assert_eq!(
        fixture
            .engine()
            .synchronize_once()
            .await
            .unwrap_or_else(|error| unreachable!("first conflict: {error}")),
        SyncRunOutcome::WorkRemaining
    );
    assert_eq!(
        fixture
            .engine()
            .synchronize_once()
            .await
            .unwrap_or_else(|error| unreachable!("second conflict: {error}")),
        SyncRunOutcome::ManualConflict
    );
    let events = fixture
        .worker
        .execute(|database| database.outbox_events())
        .await
        .unwrap_or_default();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].conflict_replay_count, 1);
    assert_eq!(events[0].last_error.as_deref(), Some("favorite_conflict"));
}

#[tokio::test]
async fn revoked_device_halts_sync_without_consuming_local_outbox() {
    let fixture = Fixture::new().await;
    fixture.seed_favorite().await;
    fixture.transport.push_behavior(PushBehavior::Revoked);
    assert!(matches!(
        fixture.engine().synchronize_once().await,
        Ok(SyncRunOutcome::Halted(_))
    ));
    assert_eq!(
        fixture
            .worker
            .execute(|database| database.outbox_events())
            .await
            .unwrap_or_default()
            .len(),
        1
    );
}

#[tokio::test]
async fn sse_disconnect_reconnects_and_cancellation_clears_connected_diagnostic() {
    let fixture = Fixture::new().await;
    fixture
        .transport
        .disconnect_first_stream
        .store(true, Ordering::SeqCst);
    let engine = Arc::new(fixture.engine());
    let (worker, _handle) = SyncWorker::new(engine);
    let cancellation = CancellationToken::new();
    let tasks = worker.start(&cancellation);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while fixture.transport.stream_attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| unreachable!("SSE did not reconnect"));
    cancellation.cancel();
    for task in tasks {
        task.await
            .unwrap_or_else(|error| unreachable!("worker task: {error}"));
    }
    let diagnostics = fixture
        .worker
        .execute(|database| database.sync_diagnostics())
        .await
        .unwrap_or_else(|error| unreachable!("diagnostics: {error}"));
    assert!(!diagnostics.sse_connected);
    assert!(
        !fixture
            .transport
            .requested_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[tokio::test]
async fn profile_switch_cancels_old_sync_worker_before_starting_the_new_one() {
    let fixture = Fixture::new().await;
    let services = Arc::new(SyncProfileServices::new(Arc::clone(&fixture.worker)));
    services.register_session(fixture.metadata.profile_id, Arc::clone(&fixture.session));
    let mut second = fixture.metadata.clone();
    second.profile_id = Uuid::new_v4();
    services.register_session(second.profile_id, Arc::clone(&fixture.session));
    let mut lifecycle = ProfileLifecycle::new(Arc::clone(&fixture.worker), Arc::clone(&services));
    lifecycle
        .switch_profile(fixture.metadata.clone(), Duration::from_secs(2))
        .await
        .unwrap_or_else(|error| unreachable!("first Profile: {error}"));
    assert!(services.manual_sync(fixture.metadata.profile_id));
    lifecycle
        .switch_profile(second.clone(), Duration::from_secs(2))
        .await
        .unwrap_or_else(|error| unreachable!("second Profile: {error}"));
    assert!(!services.manual_sync(fixture.metadata.profile_id));
    assert!(services.manual_sync(second.profile_id));
    lifecycle
        .shutdown(Duration::from_secs(2))
        .await
        .unwrap_or_else(|error| unreachable!("shutdown: {error}"));
    assert!(!services.manual_sync(second.profile_id));
}
