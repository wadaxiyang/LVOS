use std::{
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use lvos_ipc::{
    AgentToUi, Envelope, HandshakeAccepted, Hello, IpcEndpoint, LocalListener, ProcessRole,
    ProtocolError, SensitiveString, UiSnapshot, UiToAgent, read_frame, write_frame,
};
use tokio::{
    process::{Child, Command},
    sync::{Mutex, Notify, mpsc},
};
use uuid::Uuid;

const UI_START_TIMEOUT: Duration = Duration::from_secs(8);
const UI_EXIT_GRACE: Duration = Duration::from_secs(2);
const UI_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type SnapshotFuture = Pin<Box<dyn Future<Output = UiSnapshot> + Send>>;
pub(crate) type SnapshotProvider = Arc<dyn Fn() -> SnapshotFuture + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UiProcessPhase {
    Stopped,
    Starting { generation: u64 },
    Ready { generation: u64 },
    Stopping { generation: u64 },
}

#[derive(Debug)]
struct ProcessState {
    phase: UiProcessPhase,
    process_id: Option<u32>,
    sender: Option<mpsc::UnboundedSender<AgentToUi>>,
    kill_sender: Option<mpsc::Sender<()>>,
    idle_exit_request: Option<Uuid>,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self {
            phase: UiProcessPhase::Stopped,
            process_id: None,
            sender: None,
            kill_sender: None,
            idle_exit_request: None,
        }
    }
}

struct Inner {
    session_id: Uuid,
    authentication: SensitiveString,
    endpoint: IpcEndpoint,
    ui_executable: PathBuf,
    next_generation: AtomicU64,
    state: Mutex<ProcessState>,
    state_changed: Notify,
    snapshot_provider: SnapshotProvider,
    inbound: mpsc::UnboundedSender<UiToAgent>,
    shutting_down: AtomicBool,
}

impl fmt::Debug for Inner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiProcessClientInner")
            .field("session_id", &self.session_id)
            .field("endpoint", &self.endpoint)
            .field("ui_executable", &self.ui_executable)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UiProcessClient {
    inner: Arc<Inner>,
}

impl UiProcessClient {
    /// Starts the authenticated local IPC listener without launching the GUI.
    ///
    /// # Errors
    /// Returns an I/O error when the endpoint cannot be bound.
    pub(crate) fn start(
        data_root: &Path,
        snapshot_provider: SnapshotProvider,
        inbound: mpsc::UnboundedSender<UiToAgent>,
    ) -> Result<Self, UiProcessError> {
        let session_id = Uuid::new_v4();
        let endpoint = IpcEndpoint::for_agent_session(data_root, session_id);
        let listener = LocalListener::bind(endpoint.clone()).map_err(UiProcessError::Io)?;
        let client = Self {
            inner: Arc::new(Inner {
                session_id,
                authentication: SensitiveString::new(format!(
                    "{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )),
                endpoint,
                ui_executable: ui_executable()?,
                next_generation: AtomicU64::new(1),
                state: Mutex::new(ProcessState::default()),
                state_changed: Notify::new(),
                snapshot_provider,
                inbound,
                shutting_down: AtomicBool::new(false),
            }),
        };
        let accept_client = client.clone();
        tokio::spawn(async move { accept_client.accept_connections(listener).await });
        Ok(client)
    }

    /// Sends a GUI command, starting and handshaking a GUI process if necessary.
    ///
    /// # Errors
    /// Returns an error if startup, handshake, or command delivery fails.
    pub(crate) async fn send(&self, command: AgentToUi) -> Result<(), UiProcessError> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(UiProcessError::ShuttingDown);
        }
        loop {
            self.ensure_ready().await?;
            let disconnected = {
                let state = self.inner.state.lock().await;
                if !matches!(state.phase, UiProcessPhase::Ready { .. }) {
                    false
                } else if let Some(sender) = state.sender.as_ref() {
                    if sender.send(command.clone()).is_ok() {
                        return Ok(());
                    }
                    true
                } else {
                    true
                }
            };
            if disconnected {
                self.mark_disconnected(None).await;
            }
        }
    }

    /// Delivers an update only to the currently ready GUI. It never starts a process.
    pub(crate) async fn send_if_ready(&self, command: AgentToUi) -> bool {
        let state = self.inner.state.lock().await;
        matches!(state.phase, UiProcessPhase::Ready { .. })
            && state
                .sender
                .as_ref()
                .is_some_and(|sender| sender.send(command).is_ok())
    }

    /// Atomically prevents new presentation commands and approves an idle GUI exit.
    pub(crate) async fn approve_idle_exit(&self, request_id: Uuid, generation: u64) {
        let mut state = self.inner.state.lock().await;
        if state.phase != (UiProcessPhase::Ready { generation }) {
            return;
        }
        let Some(sender) = state.sender.as_ref() else {
            return;
        };
        if sender
            .send(AgentToUi::IdleExitApproved {
                request_id,
                process_generation: generation,
            })
            .is_ok()
        {
            state.phase = UiProcessPhase::Stopping { generation };
            state.idle_exit_request = Some(request_id);
            drop(state);
            self.inner.state_changed.notify_waiters();
        }
    }

    /// Returns a still-connected GUI to Ready when queued activity invalidated its idle claim.
    pub(crate) async fn cancel_idle_exit(&self, request_id: Uuid, generation: u64) {
        let mut state = self.inner.state.lock().await;
        if state.phase == (UiProcessPhase::Stopping { generation })
            && state.idle_exit_request == Some(request_id)
            && state.sender.is_some()
            && !self.inner.shutting_down.load(Ordering::Acquire)
        {
            state.phase = UiProcessPhase::Ready { generation };
            state.idle_exit_request = None;
            drop(state);
            self.inner.state_changed.notify_waiters();
        }
    }

    /// Requests orderly GUI shutdown and prevents later respawn from this Agent instance.
    pub(crate) async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Release);
        let (sender, kill_sender, generation) = {
            let mut state = self.inner.state.lock().await;
            let generation = phase_generation(state.phase);
            if let Some(generation) = generation {
                state.phase = UiProcessPhase::Stopping { generation };
            }
            state.idle_exit_request = None;
            (state.sender.take(), state.kill_sender.clone(), generation)
        };
        if let Some(sender) = sender {
            let _ = sender.send(AgentToUi::Shutdown);
        } else if let Some(kill_sender) = kill_sender {
            let _ = kill_sender.send(()).await;
        }
        if generation.is_some() {
            self.inner.state_changed.notify_waiters();
            if tokio::time::timeout(UI_SHUTDOWN_TIMEOUT, self.wait_until_stopped())
                .await
                .is_err()
            {
                tracing::warn!(event = "ui_process_shutdown_timeout");
                if let Some(kill_sender) = {
                    let state = self.inner.state.lock().await;
                    state.kill_sender.clone()
                } {
                    let _ = kill_sender.send(()).await;
                    let _ = tokio::time::timeout(UI_EXIT_GRACE, self.wait_until_stopped()).await;
                }
            }
        }
    }

    async fn wait_until_stopped(&self) {
        loop {
            let notified = self.inner.state_changed.notified();
            if self.inner.state.lock().await.phase == UiProcessPhase::Stopped {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn phase(&self) -> UiProcessPhase {
        self.inner.state.lock().await.phase
    }

    pub(crate) async fn process_id(&self) -> Option<u32> {
        self.inner.state.lock().await.process_id
    }

    pub(crate) async fn wait_for_stopped(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.wait_until_stopped())
            .await
            .is_ok()
    }

    async fn ensure_ready(&self) -> Result<(), UiProcessError> {
        loop {
            let notified = self.inner.state_changed.notified();
            let should_spawn = {
                let mut state = self.inner.state.lock().await;
                match state.phase {
                    UiProcessPhase::Ready { .. } => return Ok(()),
                    UiProcessPhase::Stopped => {
                        let generation = self.inner.next_generation.fetch_add(1, Ordering::AcqRel);
                        state.phase = UiProcessPhase::Starting { generation };
                        Some(generation)
                    }
                    UiProcessPhase::Starting { .. } | UiProcessPhase::Stopping { .. } => None,
                }
            };
            if let Some(generation) = should_spawn
                && let Err(error) = self.spawn_ui(generation).await
            {
                self.mark_stopped(generation).await;
                return Err(error);
            }
            tokio::time::timeout(UI_START_TIMEOUT, notified)
                .await
                .map_err(|_| UiProcessError::StartupTimeout)?;
        }
    }

    async fn spawn_ui(&self, generation: u64) -> Result<(), UiProcessError> {
        let mut command = Command::new(&self.inner.ui_executable);
        configure_ui_renderer_environment(&mut command);
        command
            .env("LVOS_IPC_ENDPOINT", self.inner.endpoint.argument())
            .env("LVOS_IPC_SESSION", self.inner.session_id.to_string())
            .env("LVOS_IPC_AUTH", self.inner.authentication.expose())
            .env("LVOS_UI_GENERATION", generation.to_string())
            .kill_on_drop(false);
        let child = command.spawn().map_err(UiProcessError::Io)?;
        let (kill_sender, kill_receiver) = mpsc::channel(1);
        {
            let mut state = self.inner.state.lock().await;
            if state.phase != (UiProcessPhase::Starting { generation }) {
                return Err(UiProcessError::StaleGeneration);
            }
            state.kill_sender = Some(kill_sender);
        }
        let client = self.clone();
        tokio::spawn(async move {
            client
                .wait_for_child(generation, child, kill_receiver)
                .await;
        });
        tracing::info!(event = "ui_process_spawned", generation);
        Ok(())
    }

    async fn wait_for_child(
        &self,
        generation: u64,
        mut child: Child,
        mut kill_receiver: mpsc::Receiver<()>,
    ) {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill_receiver.recv() => {
                if let Ok(status) = tokio::time::timeout(UI_EXIT_GRACE, child.wait()).await {
                    status
                } else {
                    let _ = child.kill().await;
                    child.wait().await
                }
            }
        };
        match status {
            Ok(status) if status.success() => {
                tracing::info!(event = "ui_process_exited", generation);
            }
            Ok(status) => tracing::warn!(
                event = "ui_process_exited",
                generation,
                ?status,
                reason = "unexpected_status"
            ),
            Err(error) => tracing::warn!(
                event = "ui_process_exited",
                generation,
                %error,
                reason = "wait_failed"
            ),
        }
        self.mark_stopped(generation).await;
    }

    async fn accept_connections(&self, mut listener: LocalListener) {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let client = self.clone();
                    tokio::spawn(async move {
                        if let Err(error) = client.serve_connection(stream).await {
                            tracing::warn!(%error, "rejected or lost lvos-ui IPC connection");
                        }
                    });
                }
                Err(error) => {
                    tracing::error!(%error, "lvos-ui IPC listener stopped");
                    break;
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn serve_connection(
        &self,
        mut stream: lvos_ipc::ServerStream,
    ) -> Result<(), UiProcessError> {
        let hello: Envelope<UiToAgent> = read_frame(&mut stream).await?;
        hello.validate(self.inner.session_id)?;
        let UiToAgent::Hello(Hello {
            role: ProcessRole::Ui,
            process_id,
            process_generation,
            authentication,
            ..
        }) = hello.payload
        else {
            return Err(UiProcessError::Protocol(ProtocolError::UnexpectedMessage));
        };
        if authentication != self.inner.authentication {
            return Err(UiProcessError::Protocol(
                ProtocolError::AuthenticationFailed,
            ));
        }
        {
            let state = self.inner.state.lock().await;
            if state.phase
                != (UiProcessPhase::Starting {
                    generation: process_generation,
                })
            {
                return Err(UiProcessError::StaleGeneration);
            }
        }

        write_frame(
            &mut stream,
            &Envelope::new(
                self.inner.session_id,
                1,
                AgentToUi::HandshakeAccepted(HandshakeAccepted { process_generation }),
            ),
        )
        .await?;
        let snapshot = (self.inner.snapshot_provider)().await;
        write_frame(
            &mut stream,
            &Envelope::new(self.inner.session_id, 2, AgentToUi::Snapshot(snapshot)),
        )
        .await?;

        let ready_envelope: Envelope<UiToAgent> = read_frame(&mut stream).await?;
        ready_envelope.validate(self.inner.session_id)?;
        let ready_sequence = ready_envelope.sequence;
        let UiToAgent::Ready(ready) = ready_envelope.payload else {
            return Err(UiProcessError::Protocol(ProtocolError::UnexpectedMessage));
        };
        if ready.process_generation != process_generation || ready.applied_sequence != 2 {
            return Err(UiProcessError::StaleGeneration);
        }

        let (outbound, mut outbound_rx) = mpsc::unbounded_channel();
        {
            let mut state = self.inner.state.lock().await;
            if state.phase
                != (UiProcessPhase::Starting {
                    generation: process_generation,
                })
            {
                return Err(UiProcessError::StaleGeneration);
            }
            state.phase = UiProcessPhase::Ready {
                generation: process_generation,
            };
            state.process_id = Some(process_id);
            state.sender = Some(outbound);
            state.idle_exit_request = None;
        }
        self.inner.state_changed.notify_waiters();
        tracing::info!(event = "ui_process_ready", generation = process_generation);

        let (mut reader, mut writer) = tokio::io::split(stream);
        let session_id = self.inner.session_id;
        let writer_task = async move {
            let mut sequence = 3_u64;
            while let Some(message) = outbound_rx.recv().await {
                write_frame(&mut writer, &Envelope::new(session_id, sequence, message)).await?;
                sequence = sequence.wrapping_add(1);
            }
            Ok::<(), lvos_ipc::FrameError>(())
        };
        let inbound = self.inner.inbound.clone();
        let reader_task = async move {
            let mut last_sequence = ready_sequence;
            loop {
                let message: Envelope<UiToAgent> = read_frame(&mut reader).await?;
                message
                    .validate(session_id)
                    .map_err(lvos_ipc::FrameError::Protocol)?;
                if message.sequence <= last_sequence {
                    return Err(lvos_ipc::FrameError::Protocol(
                        ProtocolError::SequenceRegression {
                            last: last_sequence,
                            received: message.sequence,
                        },
                    ));
                }
                last_sequence = message.sequence;
                if inbound.send(message.payload).is_err() {
                    return Ok(());
                }
            }
        };
        let connection_result = tokio::select! {
            result = writer_task => result,
            result = reader_task => result,
        };
        self.mark_disconnected(Some(process_generation)).await;
        connection_result.map_err(UiProcessError::Frame)
    }

    async fn mark_disconnected(&self, generation: Option<u64>) {
        let kill_sender = {
            let mut state = self.inner.state.lock().await;
            if generation
                .is_some_and(|generation| phase_generation(state.phase) != Some(generation))
            {
                return;
            }
            let Some(current) = phase_generation(state.phase) else {
                return;
            };
            state.phase = UiProcessPhase::Stopping {
                generation: current,
            };
            state.sender = None;
            state.process_id = None;
            state.idle_exit_request = None;
            state.kill_sender.clone()
        };
        if let Some(kill_sender) = kill_sender {
            let _ = kill_sender.send(()).await;
        }
        self.inner.state_changed.notify_waiters();
    }

    async fn mark_stopped(&self, generation: u64) {
        let mut state = self.inner.state.lock().await;
        if phase_generation(state.phase) == Some(generation) {
            state.phase = UiProcessPhase::Stopped;
            state.process_id = None;
            state.sender = None;
            state.kill_sender = None;
            state.idle_exit_request = None;
            drop(state);
            self.inner.state_changed.notify_waiters();
        }
    }
}

fn configure_ui_renderer_environment(command: &mut Command) {
    command.env("SLINT_DESTROY_WINDOW_ON_HIDE", "1");
}

fn phase_generation(phase: UiProcessPhase) -> Option<u64> {
    match phase {
        UiProcessPhase::Stopped => None,
        UiProcessPhase::Starting { generation }
        | UiProcessPhase::Ready { generation }
        | UiProcessPhase::Stopping { generation } => Some(generation),
    }
}

fn ui_executable() -> Result<PathBuf, UiProcessError> {
    let executable = std::env::current_exe().map_err(UiProcessError::Io)?;
    let parent = executable
        .parent()
        .ok_or(UiProcessError::MissingExecutable)?;
    #[cfg(target_os = "windows")]
    let name = "lvos-ui.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "lvos-ui";
    Ok(parent.join(name))
}

#[derive(Debug)]
pub(crate) enum UiProcessError {
    Io(std::io::Error),
    Frame(lvos_ipc::FrameError),
    Protocol(ProtocolError),
    StartupTimeout,
    StaleGeneration,
    MissingExecutable,
    ShuttingDown,
}

impl fmt::Display for UiProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "GUI process I/O failed: {error}"),
            Self::Frame(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::StartupTimeout => formatter.write_str("GUI process startup timed out"),
            Self::StaleGeneration => formatter.write_str("GUI process generation is stale"),
            Self::MissingExecutable => {
                formatter.write_str("lvos-ui executable path is unavailable")
            }
            Self::ShuttingDown => formatter.write_str("Agent is shutting down"),
        }
    }
}

impl Error for UiProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::StartupTimeout
            | Self::StaleGeneration
            | Self::MissingExecutable
            | Self::ShuttingDown => None,
        }
    }
}

impl From<lvos_ipc::FrameError> for UiProcessError {
    fn from(value: lvos_ipc::FrameError) -> Self {
        Self::Frame(value)
    }
}

impl From<ProtocolError> for UiProcessError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_child_destroys_native_windows_when_hidden() {
        let mut command = Command::new("lvos-ui");
        configure_ui_renderer_environment(&mut command);
        let configured = command.as_std().get_envs().any(|(name, value)| {
            name == "SLINT_DESTROY_WINDOW_ON_HIDE" && value.is_some_and(|value| value == "1")
        });
        assert!(configured);
    }

    #[test]
    fn lifecycle_generation_is_never_lost_when_a_ui_host_is_rebuilt() {
        let mut state = ProcessState {
            phase: UiProcessPhase::Starting { generation: 41 },
            process_id: None,
            sender: None,
            kill_sender: None,
            idle_exit_request: None,
        };
        state.phase = UiProcessPhase::Stopping { generation: 41 };
        assert_eq!(phase_generation(state.phase), Some(41));
        state.phase = UiProcessPhase::Stopped;
        state.phase = UiProcessPhase::Starting { generation: 42 };
        assert_eq!(phase_generation(state.phase), Some(42));
    }
}
