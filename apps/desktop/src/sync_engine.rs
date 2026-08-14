use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lvos_core::UnixTimestamp;
use lvos_storage::{AcknowledgedEvent, ConflictResolution, OutboxEvent};
use lvos_sync::{PushAck, PushEvent, PushRequest};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AuthenticatedSession, BackgroundProfileServices, DatabaseWorker, DatabaseWorkerError,
    RevisionStreamEvent, SessionError, SyncTransport, TransportError,
};

const DEFAULT_PUSH_BATCH: u32 = 100;
const DEFAULT_PULL_PAGE: u32 = 200;
const MAX_RETRY_SECONDS: u64 = 300;

/// Performs durable push and pull cycles for exactly one active Profile.
pub struct SyncEngine<T> {
    database: Arc<DatabaseWorker>,
    session: Arc<AuthenticatedSession<T>>,
    push_batch: u32,
    pull_page: u32,
}

impl<T> fmt::Debug for SyncEngine<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncEngine")
            .field("database", &self.database)
            .field("session", &self.session)
            .field("push_batch", &self.push_batch)
            .field("pull_page", &self.pull_page)
            .finish()
    }
}

impl<T: SyncTransport + 'static> SyncEngine<T> {
    #[must_use]
    pub fn new(database: Arc<DatabaseWorker>, session: Arc<AuthenticatedSession<T>>) -> Self {
        Self {
            database,
            session,
            push_batch: DEFAULT_PUSH_BATCH,
            pull_page: DEFAULT_PULL_PAGE,
        }
    }

    #[must_use]
    pub fn with_batch_limits(mut self, push_batch: u32, pull_page: u32) -> Self {
        self.push_batch = push_batch;
        self.pull_page = pull_page;
        self
    }

    /// Executes one bounded push followed by complete paginated catch-up.
    ///
    /// # Errors
    /// Returns durable storage, protocol, session, or clock failures. Retryable network failures
    /// are persisted and returned as [`SyncRunOutcome::RetryAt`].
    pub async fn synchronize_once(&self) -> Result<SyncRunOutcome, SyncEngineError> {
        let now = current_timestamp()?;
        let limit = self.push_batch;
        let events = self
            .database
            .execute(move |database| database.ready_outbox_events(now, limit))
            .await?;
        if !events.is_empty() {
            match self.push_events(&events, now).await? {
                PushDisposition::Continue => {}
                PushDisposition::ReplayPrepared => return Ok(SyncRunOutcome::WorkRemaining),
                PushDisposition::ManualConflict => return Ok(SyncRunOutcome::ManualConflict),
                PushDisposition::RetryAt(timestamp) => {
                    return Ok(SyncRunOutcome::RetryAt(timestamp));
                }
                PushDisposition::Halted(reason) => return Ok(SyncRunOutcome::Halted(reason)),
            }
        }
        self.pull_all(now).await?;
        let batch_was_full = events.len() == usize::try_from(self.push_batch).unwrap_or(usize::MAX);
        Ok(if batch_was_full {
            SyncRunOutcome::WorkRemaining
        } else {
            SyncRunOutcome::Idle
        })
    }

    async fn push_events(
        &self,
        events: &[OutboxEvent],
        now: UnixTimestamp,
    ) -> Result<PushDisposition, SyncEngineError> {
        let owned = events.to_vec();
        let push_events = self
            .database
            .execute(move |database| {
                owned
                    .iter()
                    .map(|event| database.push_event(event))
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        let request = PushRequest {
            events: push_events.clone(),
        };
        match self.session.push(&request).await {
            Ok(response) => {
                let acknowledged =
                    match_acknowledgements(events, &push_events, response.acknowledgements)?;
                self.database
                    .execute(move |database| database.acknowledge_events(&acknowledged, now))
                    .await?;
                Ok(PushDisposition::Continue)
            }
            Err(SessionError::Transport(TransportError::Conflict(conflict))) => {
                let event = conflict
                    .event_id
                    .as_deref()
                    .and_then(|id| events.iter().find(|event| event.event_id.to_string() == id))
                    .or_else(|| (events.len() == 1).then(|| &events[0]))
                    .ok_or(SyncEngineError::InvalidResponse)?
                    .clone();
                let current = conflict.current.clone();
                let resolution = self
                    .database
                    .execute(move |database| {
                        database.resolve_conflict(&event, current.as_ref(), now)
                    })
                    .await?;
                Ok(match resolution {
                    ConflictResolution::Converged => PushDisposition::Continue,
                    ConflictResolution::ReplayPrepared => PushDisposition::ReplayPrepared,
                    ConflictResolution::ManualRetryRequired => PushDisposition::ManualConflict,
                })
            }
            Err(error) if is_offline(&error) => {
                let retry_at = retry_timestamp(events, now)?;
                let retry_events = events.to_vec();
                self.database
                    .execute(move |database| {
                        database.mark_retry(&retry_events, retry_at, "server_unavailable", now)
                    })
                    .await?;
                Ok(PushDisposition::RetryAt(retry_at))
            }
            Err(error) if is_session_halted(&error) => {
                let message = error.to_string();
                self.record_error(message.clone(), now).await?;
                Ok(PushDisposition::Halted(error.to_string()))
            }
            Err(error) => Err(SyncEngineError::Session(error)),
        }
    }

    async fn pull_all(&self, now: UnixTimestamp) -> Result<(), SyncEngineError> {
        loop {
            let diagnostics = self
                .database
                .execute(|database| database.sync_diagnostics())
                .await?;
            let response = match self
                .session
                .changes(diagnostics.last_server_revision, self.pull_page)
                .await
            {
                Ok(response) => response,
                Err(error) if is_offline(&error) => {
                    self.record_error("server_unavailable".to_owned(), now)
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    self.record_error(error.to_string(), now).await?;
                    return Err(SyncEngineError::Session(error));
                }
            };
            let changes = response.changes;
            let next_revision = response.next_revision;
            self.database
                .execute(move |database| database.apply_remote_page(&changes, next_revision, now))
                .await?;
            if !response.has_more {
                return Ok(());
            }
            if response.next_revision >= response.latest_revision {
                return Err(SyncEngineError::InvalidResponse);
            }
        }
    }

    async fn set_sse_connected(&self, connected: bool) -> Result<(), SyncEngineError> {
        let now = current_timestamp()?;
        self.database
            .execute(move |database| database.set_sse_connected(connected, now))
            .await?;
        Ok(())
    }

    async fn record_error(&self, error: String, now: UnixTimestamp) -> Result<(), SyncEngineError> {
        self.database
            .execute(move |database| database.set_sync_error(&error, now))
            .await?;
        Ok(())
    }
}

fn match_acknowledgements(
    events: &[OutboxEvent],
    sent: &[PushEvent],
    acknowledgements: Vec<PushAck>,
) -> Result<Vec<AcknowledgedEvent>, SyncEngineError> {
    if events.len() != sent.len() || events.len() != acknowledgements.len() {
        return Err(SyncEngineError::InvalidResponse);
    }
    let acknowledgements: HashMap<_, _> = acknowledgements
        .into_iter()
        .map(|ack| (ack.event_id.clone(), ack))
        .collect();
    if acknowledgements.len() != events.len() {
        return Err(SyncEngineError::InvalidResponse);
    }
    events
        .iter()
        .zip(sent)
        .map(|(event, sent)| {
            let acknowledgement = acknowledgements
                .get(&event.event_id.to_string())
                .ok_or(SyncEngineError::InvalidResponse)?
                .clone();
            Ok(AcknowledgedEvent {
                event: event.clone(),
                acknowledgement,
                sent_query_count: sent.query_stats.as_ref().map(|stats| stats.query_count),
            })
        })
        .collect()
}

fn retry_timestamp(
    events: &[OutboxEvent],
    now: UnixTimestamp,
) -> Result<UnixTimestamp, SyncEngineError> {
    let attempt = events
        .iter()
        .map(|event| event.attempt_count)
        .max()
        .unwrap_or(0)
        .min(8);
    let base = 2_u64.saturating_pow(attempt.saturating_add(1));
    let jitter = events
        .first()
        .map_or(0, |event| u64::from(event.event_id.as_bytes()[15] % 3));
    let delay = base.saturating_add(jitter).min(MAX_RETRY_SECONDS);
    let seconds = now
        .as_seconds()
        .checked_add(i64::try_from(delay).map_err(|_| SyncEngineError::Clock)?)
        .ok_or(SyncEngineError::Clock)?;
    Ok(UnixTimestamp::from_seconds(seconds))
}

fn current_timestamp() -> Result<UnixTimestamp, SyncEngineError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SyncEngineError::Clock)?
        .as_secs();
    Ok(UnixTimestamp::from_seconds(
        i64::try_from(seconds).map_err(|_| SyncEngineError::Clock)?,
    ))
}

fn is_offline(error: &SessionError) -> bool {
    matches!(error, SessionError::Transport(TransportError::Offline))
}

fn is_session_halted(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::Transport(TransportError::SessionInvalid | TransportError::DeviceRevoked)
            | SessionError::NoPersistentSession
            | SessionError::InvalidCredential
    )
}

enum PushDisposition {
    Continue,
    ReplayPrepared,
    ManualConflict,
    RetryAt(UnixTimestamp),
    Halted(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncRunOutcome {
    Idle,
    WorkRemaining,
    RetryAt(UnixTimestamp),
    ManualConflict,
    Halted(String),
}

/// Event-driven worker. Wakes only for local mutations, revision notices, manual sync, or a
/// persisted retry deadline.
pub struct SyncWorker<T> {
    engine: Arc<SyncEngine<T>>,
    wake_receiver: mpsc::Receiver<()>,
    wake_sender: mpsc::Sender<()>,
}

impl<T> fmt::Debug for SyncWorker<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncWorker")
            .field("engine", &self.engine)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct SyncWorkerHandle {
    wake_sender: mpsc::Sender<()>,
}

impl SyncWorkerHandle {
    pub fn wake(&self) {
        let _ = self.wake_sender.try_send(());
    }
}

/// Profile-scoped session registry used by [`crate::ProfileLifecycle`].
///
/// Registering a session does not start work. The lifecycle starts it only after the matching
/// Profile database is active, and cancellation removes the manual-sync handle before a switch.
pub struct SyncProfileServices<T> {
    database: Arc<DatabaseWorker>,
    sessions: StdMutex<HashMap<Uuid, Arc<AuthenticatedSession<T>>>>,
    handles: Arc<StdMutex<HashMap<Uuid, SyncWorkerHandle>>>,
}

impl<T> fmt::Debug for SyncProfileServices<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let session_count = self.sessions.lock().map_or(0, |sessions| sessions.len());
        let active_count = self.handles.lock().map_or(0, |handles| handles.len());
        formatter
            .debug_struct("SyncProfileServices")
            .field("database", &self.database)
            .field("session_count", &session_count)
            .field("active_count", &active_count)
            .finish()
    }
}

impl<T> SyncProfileServices<T> {
    #[must_use]
    pub fn new(database: Arc<DatabaseWorker>) -> Self {
        Self {
            database,
            sessions: StdMutex::new(HashMap::new()),
            handles: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub fn register_session(&self, profile_id: Uuid, session: Arc<AuthenticatedSession<T>>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(profile_id, session);
        }
    }

    pub fn remove_session(&self, profile_id: Uuid) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&profile_id);
        }
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove(&profile_id);
        }
    }

    #[must_use]
    pub fn session(&self, profile_id: Uuid) -> Option<Arc<AuthenticatedSession<T>>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&profile_id).cloned())
    }

    /// Wakes the active Profile for a user-requested sync.
    ///
    /// Returns `false` when that Profile has no running authenticated worker.
    pub fn manual_sync(&self, profile_id: Uuid) -> bool {
        self.handles
            .lock()
            .ok()
            .and_then(|handles| handles.get(&profile_id).cloned())
            .is_some_and(|handle| {
                handle.wake();
                true
            })
    }

    #[must_use]
    pub fn worker_handle(&self, profile_id: Uuid) -> Option<SyncWorkerHandle> {
        self.handles
            .lock()
            .ok()
            .and_then(|handles| handles.get(&profile_id).cloned())
    }
}

impl<T: SyncTransport + 'static> BackgroundProfileServices for SyncProfileServices<T> {
    fn start(&self, profile_id: Uuid, cancellation: CancellationToken) -> Vec<JoinHandle<()>> {
        let Some(session) = self.session(profile_id) else {
            return Vec::new();
        };
        let engine = Arc::new(SyncEngine::new(Arc::clone(&self.database), session));
        let (worker, handle) = SyncWorker::new(engine);
        if let Ok(mut handles) = self.handles.lock() {
            handles.insert(profile_id, handle);
        }
        let mut tasks = worker.start(&cancellation);
        let handles = Arc::clone(&self.handles);
        let cleanup_cancellation = cancellation.clone();
        tasks.push(tokio::spawn(async move {
            cleanup_cancellation.cancelled().await;
            if let Ok(mut handles) = handles.lock() {
                handles.remove(&profile_id);
            }
        }));
        tasks
    }
}

impl<T: SyncTransport + 'static> SyncWorker<T> {
    #[must_use]
    pub fn new(engine: Arc<SyncEngine<T>>) -> (Self, SyncWorkerHandle) {
        let (wake_sender, wake_receiver) = mpsc::channel(1);
        (
            Self {
                engine,
                wake_receiver,
                wake_sender: wake_sender.clone(),
            },
            SyncWorkerHandle { wake_sender },
        )
    }

    /// Starts the sync cycle and SSE supervisor as Profile-scoped tasks.
    #[must_use]
    pub fn start(mut self, cancellation: &CancellationToken) -> Vec<JoinHandle<()>> {
        let cycle_cancel = cancellation.child_token();
        let stream_cancel = cancellation.child_token();
        let stream_engine = Arc::clone(&self.engine);
        let stream_wake = self.wake_sender.clone();
        vec![
            tokio::spawn(async move { self.run_cycles(cycle_cancel).await }),
            tokio::spawn(async move {
                run_revision_supervisor(stream_engine, stream_wake, stream_cancel).await;
            }),
        ]
    }

    async fn run_cycles(&mut self, cancellation: CancellationToken) {
        let mut retry_at = None;
        loop {
            if retry_at.is_none() {
                match self.engine.synchronize_once().await {
                    Ok(SyncRunOutcome::WorkRemaining) => continue,
                    Ok(SyncRunOutcome::RetryAt(timestamp)) => retry_at = Some(timestamp),
                    Ok(SyncRunOutcome::Halted(_)) | Err(_) => return,
                    Ok(SyncRunOutcome::Idle | SyncRunOutcome::ManualConflict) => {}
                }
            }
            let retry_delay = retry_at.map_or(Duration::from_secs(31_536_000), |timestamp| {
                delay_until(timestamp)
            });
            tokio::select! {
                () = cancellation.cancelled() => return,
                wake = self.wake_receiver.recv() => {
                    if wake.is_none() { return; }
                    retry_at = None;
                }
                () = tokio::time::sleep(retry_delay), if retry_at.is_some() => retry_at = None,
            }
        }
    }
}

async fn run_revision_supervisor<T: SyncTransport + 'static>(
    engine: Arc<SyncEngine<T>>,
    wake: mpsc::Sender<()>,
    cancellation: CancellationToken,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if cancellation.is_cancelled() {
            let _ = engine.set_sse_connected(false).await;
            return;
        }
        let (event_sender, mut event_receiver) = mpsc::channel(8);
        let stream_cancel = cancellation.child_token();
        let session = Arc::clone(&engine.session);
        let stream_token = stream_cancel.clone();
        let mut stream =
            tokio::spawn(async move { session.revision_stream(event_sender, stream_token).await });
        let _ = engine.set_sse_connected(false).await;
        let stream_result = loop {
            tokio::select! {
                () = cancellation.cancelled() => {
                    stream_cancel.cancel();
                    let _ = stream.await;
                    let _ = engine.set_sse_connected(false).await;
                    return;
                }
                event = event_receiver.recv() => {
                    match event {
                        Some(RevisionStreamEvent::Connected) => {
                            let _ = engine.set_sse_connected(true).await;
                            backoff = Duration::from_secs(1);
                            let _ = wake.try_send(());
                        }
                        Some(RevisionStreamEvent::Revision(_)) => {
                            let _ = wake.try_send(());
                        }
                        None => {}
                    }
                }
                result = &mut stream => break result,
            }
        };
        let _ = engine.set_sse_connected(false).await;
        if matches!(
            stream_result,
            Ok(Err(SessionError::Transport(
                TransportError::DeviceRevoked | TransportError::SessionInvalid
            )))
        ) {
            return;
        }
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(60));
    }
}

fn delay_until(timestamp: UnixTimestamp) -> Duration {
    let now = current_timestamp().map_or(timestamp.as_seconds(), UnixTimestamp::as_seconds);
    Duration::from_secs(u64::try_from(timestamp.as_seconds().saturating_sub(now)).unwrap_or(0))
}

#[derive(Debug)]
pub enum SyncEngineError {
    Database(DatabaseWorkerError),
    Session(SessionError),
    InvalidResponse,
    Clock,
}

impl fmt::Display for SyncEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "sync storage failed: {error}"),
            Self::Session(error) => write!(formatter, "sync session failed: {error}"),
            Self::InvalidResponse => formatter.write_str("sync response was inconsistent"),
            Self::Clock => formatter.write_str("system clock cannot represent sync time"),
        }
    }
}

impl Error for SyncEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::InvalidResponse | Self::Clock => None,
        }
    }
}

impl From<DatabaseWorkerError> for SyncEngineError {
    fn from(value: DatabaseWorkerError) -> Self {
        Self::Database(value)
    }
}
