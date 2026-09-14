use std::{error::Error, fmt, sync::mpsc as std_mpsc, time::Duration};

use lvos_ipc::{
    AgentToUi, Envelope, Hello, IpcEndpoint, ProcessRole, ProtocolError, SensitiveString,
    UiOperation, UiReady, UiToAgent, connect, read_frame, write_frame,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const SNAPSHOT_APPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(crate) struct UiRequestSender {
    sender: mpsc::UnboundedSender<UiToAgent>,
}

impl UiRequestSender {
    pub(crate) fn request(&self, operation: UiOperation) -> Result<Uuid, UiIpcError> {
        let request_id = Uuid::new_v4();
        self.sender
            .send(UiToAgent::Request {
                request_id,
                operation,
            })
            .map_err(|_| UiIpcError::Disconnected)?;
        Ok(request_id)
    }

    pub(crate) fn event(&self, event: UiToAgent) -> Result<(), UiIpcError> {
        self.sender
            .send(event)
            .map_err(|_| UiIpcError::Disconnected)
    }
}

pub(crate) struct IncomingMessage {
    pub(crate) sequence: u64,
    pub(crate) payload: AgentToUi,
    pub(crate) applied: Option<oneshot::Sender<()>>,
}

impl fmt::Debug for IncomingMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IncomingMessage")
            .field("sequence", &self.sequence)
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct UiConnectionConfig {
    endpoint: IpcEndpoint,
    session_id: Uuid,
    authentication: SensitiveString,
    generation: u64,
}

impl UiConnectionConfig {
    fn from_environment() -> Result<Self, UiIpcError> {
        let endpoint = required_env("LVOS_IPC_ENDPOINT")?;
        let session_id = required_env("LVOS_IPC_SESSION")?
            .parse()
            .map_err(|_| UiIpcError::InvalidConfiguration("LVOS_IPC_SESSION"))?;
        let authentication = SensitiveString::new(required_env("LVOS_IPC_AUTH")?);
        let generation = required_env("LVOS_UI_GENERATION")?
            .parse()
            .map_err(|_| UiIpcError::InvalidConfiguration("LVOS_UI_GENERATION"))?;
        Ok(Self {
            endpoint: IpcEndpoint::from_argument(&endpoint),
            session_id,
            authentication,
            generation,
        })
    }
}

#[derive(Debug)]
pub(crate) struct UiIpcClient {
    requests: UiRequestSender,
    generation: u64,
}

impl UiIpcClient {
    /// Starts the GUI-side transport on a dedicated runtime thread.
    ///
    /// # Errors
    /// Returns an error when required authenticated launch configuration is absent or invalid.
    pub(crate) fn start(incoming: std_mpsc::Sender<IncomingMessage>) -> Result<Self, UiIpcError> {
        let config = UiConnectionConfig::from_environment()?;
        let generation = config.generation;
        let (sender, receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("lvos-ui-ipc".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "failed to initialize GUI IPC runtime");
                        request_event_loop_quit();
                        return;
                    }
                };
                if let Err(error) = runtime.block_on(run_connection(config, incoming, receiver)) {
                    tracing::warn!(%error, "GUI IPC connection ended");
                }
                request_event_loop_quit();
            })
            .map_err(UiIpcError::Io)?;
        Ok(Self {
            requests: UiRequestSender { sender },
            generation,
        })
    }

    #[must_use]
    pub(crate) fn requests(&self) -> UiRequestSender {
        self.requests.clone()
    }

    pub(crate) fn announce_exit(&self) {
        let _ = self.requests.event(UiToAgent::Exiting {
            process_generation: self.generation,
        });
    }
}

#[allow(clippy::too_many_lines)]
async fn run_connection(
    config: UiConnectionConfig,
    incoming: std_mpsc::Sender<IncomingMessage>,
    mut outgoing: mpsc::UnboundedReceiver<UiToAgent>,
) -> Result<(), UiIpcError> {
    let mut stream = connect(&config.endpoint, CONNECT_TIMEOUT)
        .await
        .map_err(UiIpcError::Io)?;
    write_frame(
        &mut stream,
        &Envelope::new(
            config.session_id,
            1,
            UiToAgent::Hello(Hello {
                role: ProcessRole::Ui,
                process_id: std::process::id(),
                process_generation: config.generation,
                authentication: config.authentication,
            }),
        ),
    )
    .await?;

    let accepted: Envelope<AgentToUi> = read_frame(&mut stream).await?;
    accepted.validate(config.session_id)?;
    let AgentToUi::HandshakeAccepted(accepted_payload) = accepted.payload else {
        return Err(UiIpcError::Protocol(ProtocolError::UnexpectedMessage));
    };
    if accepted.sequence != 1 || accepted_payload.process_generation != config.generation {
        return Err(UiIpcError::Protocol(ProtocolError::SequenceRegression {
            last: 0,
            received: accepted.sequence,
        }));
    }

    let snapshot: Envelope<AgentToUi> = read_frame(&mut stream).await?;
    snapshot.validate(config.session_id)?;
    if snapshot.sequence != 2 || !matches!(&snapshot.payload, AgentToUi::Snapshot(_)) {
        return Err(UiIpcError::Protocol(ProtocolError::UnexpectedMessage));
    }
    let (applied, applied_rx) = oneshot::channel();
    incoming
        .send(IncomingMessage {
            sequence: snapshot.sequence,
            payload: snapshot.payload,
            applied: Some(applied),
        })
        .map_err(|_| UiIpcError::Disconnected)?;
    tokio::time::timeout(SNAPSHOT_APPLY_TIMEOUT, applied_rx)
        .await
        .map_err(|_| UiIpcError::SnapshotApplyTimeout)?
        .map_err(|_| UiIpcError::Disconnected)?;
    write_frame(
        &mut stream,
        &Envelope::new(
            config.session_id,
            2,
            UiToAgent::Ready(UiReady {
                process_generation: config.generation,
                applied_sequence: snapshot.sequence,
            }),
        ),
    )
    .await?;

    let (mut reader, mut writer) = tokio::io::split(stream);
    let session_id = config.session_id;
    let writer_task = async move {
        let mut sequence = 3_u64;
        while let Some(message) = outgoing.recv().await {
            write_frame(&mut writer, &Envelope::new(session_id, sequence, message)).await?;
            sequence = sequence.wrapping_add(1);
        }
        Ok::<(), lvos_ipc::FrameError>(())
    };
    let reader_task = async move {
        let mut last_sequence = 2_u64;
        loop {
            let message: Envelope<AgentToUi> = read_frame(&mut reader).await?;
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
            if incoming
                .send(IncomingMessage {
                    sequence: message.sequence,
                    payload: message.payload,
                    applied: None,
                })
                .is_err()
            {
                return Ok(());
            }
        }
    };
    tokio::select! {
        result = writer_task => result?,
        result = reader_task => result?,
    }
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, UiIpcError> {
    std::env::var(name).map_err(|_| UiIpcError::InvalidConfiguration(name))
}

fn request_event_loop_quit() {
    if let Err(error) = slint::invoke_from_event_loop(|| {
        let _ = slint::quit_event_loop();
    }) {
        tracing::debug!(%error, "GUI event loop already stopped");
    }
}

#[derive(Debug)]
pub(crate) enum UiIpcError {
    InvalidConfiguration(&'static str),
    Io(std::io::Error),
    Frame(lvos_ipc::FrameError),
    Protocol(ProtocolError),
    SnapshotApplyTimeout,
    Disconnected,
}

impl fmt::Display for UiIpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(name) => {
                write!(formatter, "missing or invalid Agent launch setting {name}")
            }
            Self::Io(error) => write!(formatter, "GUI IPC I/O failed: {error}"),
            Self::Frame(error) => error.fmt(formatter),
            Self::Protocol(error) => error.fmt(formatter),
            Self::SnapshotApplyTimeout => {
                formatter.write_str("GUI did not apply the Agent snapshot in time")
            }
            Self::Disconnected => formatter.write_str("Agent IPC disconnected"),
        }
    }
}

impl Error for UiIpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::InvalidConfiguration(_) | Self::SnapshotApplyTimeout | Self::Disconnected => None,
        }
    }
}

impl From<lvos_ipc::FrameError> for UiIpcError {
    fn from(value: lvos_ipc::FrameError) -> Self {
        Self::Frame(value)
    }
}

impl From<ProtocolError> for UiIpcError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}
