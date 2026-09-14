//! Versioned, authenticated local IPC shared by the LVOS Agent and GUI process.
//!
//! Frames use a bounded length-prefixed JSON encoding. The transport is a local-only Windows
//! named pipe or a user-owned Unix domain socket on macOS. A per-Agent secret in the first frame
//! authenticates a GUI child before any state is disclosed.

use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SensitiveString(String);

impl SensitiveString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveString([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub protocol_version: u16,
    pub session_id: Uuid,
    pub sequence: u64,
    pub message_id: Uuid,
    pub payload: T,
}

impl<T> Envelope<T> {
    #[must_use]
    pub fn new(session_id: Uuid, sequence: u64, payload: T) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            session_id,
            sequence,
            message_id: Uuid::new_v4(),
            payload,
        }
    }

    /// Verifies the frozen protocol and expected Agent session.
    ///
    /// # Errors
    /// Returns a protocol error for version or session mismatches.
    pub fn validate(&self, expected_session: Uuid) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::VersionMismatch {
                expected: PROTOCOL_VERSION,
                received: self.protocol_version,
            });
        }
        if self.session_id != expected_session {
            return Err(ProtocolError::SessionMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRole {
    Agent,
    Ui,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub role: ProcessRole,
    pub process_id: u32,
    pub process_generation: u64,
    pub authentication: SensitiveString,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandshakeAccepted {
    pub process_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiReady {
    pub process_generation: u64,
    pub applied_sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupErrorKind {
    ProviderConfigurationRequired,
    ProviderUnauthorized,
    TranslationUnavailable,
    UnsupportedInput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum LookupUiState {
    Loading {
        query_id: u64,
        source: String,
    },
    Ready {
        query_id: u64,
        content_key: String,
        source: String,
        translation: String,
        favorite: bool,
        effective_query_count: u64,
    },
    Error {
        query_id: u64,
        source: String,
        kind: LookupErrorKind,
    },
}

impl LookupUiState {
    #[must_use]
    pub const fn query_id(&self) -> u64 {
        match self {
            Self::Loading { query_id, .. }
            | Self::Ready { query_id, .. }
            | Self::Error { query_id, .. } => *query_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRecordSnapshot {
    pub key: String,
    pub source: String,
    pub translation: String,
    pub count: u64,
    pub favorite: bool,
    pub metadata: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSnapshot {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub last_seen: String,
    pub current: bool,
    pub revoked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPreferenceSnapshot {
    pub popup_idle_timeout_secs: u32,
    pub launch_minimized: bool,
    pub global_hotkey: String,
    pub start_at_login: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainUiSnapshot {
    pub revision: u64,
    pub history: Vec<UiRecordSnapshot>,
    pub favorites: Vec<UiRecordSnapshot>,
    pub devices: Vec<DeviceSnapshot>,
    pub tokenhub_model: String,
    pub tokenhub_configured: bool,
    pub proxy_kind: u8,
    pub proxy_address: String,
    pub provider_proxy_enabled: bool,
    pub update_proxy_enabled: bool,
    pub server_url: String,
    pub username: String,
    pub current_device: String,
    pub sync_status: String,
    pub update_status: String,
    pub preferences: UiPreferenceSnapshot,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainUiPatch {
    pub base_revision: u64,
    pub revision: u64,
    pub history: Option<Vec<UiRecordSnapshot>>,
    pub favorites: Option<Vec<UiRecordSnapshot>>,
    pub devices: Option<Vec<DeviceSnapshot>>,
    pub tokenhub_model: Option<String>,
    pub tokenhub_configured: Option<bool>,
    pub proxy_kind: Option<u8>,
    pub proxy_address: Option<String>,
    pub provider_proxy_enabled: Option<bool>,
    pub update_proxy_enabled: Option<bool>,
    pub server_url: Option<String>,
    pub username: Option<String>,
    pub current_device: Option<String>,
    pub sync_status: Option<String>,
    pub update_status: Option<String>,
    pub preferences: Option<UiPreferenceSnapshot>,
    pub feedback: Option<UiFeedback>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackLevel {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFeedback {
    pub message: String,
    pub level: FeedbackLevel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSnapshot {
    pub revision: u64,
    pub main: MainUiSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiOperationName {
    HistorySearch,
    FavoritesSearch,
    SetFavorite,
    ClearHistory,
    SaveProviderSettings,
    SaveNetworkSettings,
    TestProvider,
    Login,
    Logout,
    ManualSync,
    TestConnection,
    RevokeDevice,
    RegenerateDeviceIdentity,
    ExportData,
    PreviewImport,
    ApplyImport,
    CheckUpdate,
    UpdateGlobalHotkey,
    UpdateStartAtLogin,
    UpdateLaunchMinimized,
    UpdatePopupIdleTimeout,
    PopupFavoriteToggle,
    PopupRefresh,
    RequestSnapshot,
    OpenAccessibilitySettings,
    RequestAccessibilityPermission,
    RestartAgent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiOperation {
    HistorySearch {
        term: String,
    },
    FavoritesSearch {
        term: String,
    },
    SetFavorite {
        key: String,
        active: bool,
    },
    ClearHistory,
    SaveProviderSettings {
        tokenhub_model: String,
        tokenhub_key: SensitiveString,
        provider_proxy_enabled: bool,
    },
    SaveNetworkSettings {
        proxy_address: String,
        proxy_kind: u8,
        provider_proxy_enabled: bool,
        update_proxy_enabled: bool,
    },
    TestProvider {
        tokenhub_model: String,
        tokenhub_key: SensitiveString,
    },
    Login {
        server: String,
        username: String,
        password: SensitiveString,
    },
    Logout,
    ManualSync,
    TestConnection {
        server: String,
    },
    RevokeDevice {
        device_id: String,
    },
    RegenerateDeviceIdentity,
    ExportData {
        path: PathBuf,
    },
    PreviewImport {
        path: PathBuf,
    },
    ApplyImport {
        token: Uuid,
    },
    CheckUpdate,
    UpdateGlobalHotkey {
        shortcut: String,
    },
    UpdateStartAtLogin {
        enabled: bool,
    },
    UpdateLaunchMinimized {
        enabled: bool,
    },
    UpdatePopupIdleTimeout {
        seconds: u32,
    },
    PopupFavoriteToggle {
        active: bool,
    },
    PopupRefresh {
        display_session_id: Uuid,
    },
    RequestSnapshot,
    OpenAccessibilitySettings,
    RequestAccessibilityPermission,
    RestartAgent,
}

impl UiOperation {
    #[must_use]
    pub const fn name(&self) -> UiOperationName {
        match self {
            Self::HistorySearch { .. } => UiOperationName::HistorySearch,
            Self::FavoritesSearch { .. } => UiOperationName::FavoritesSearch,
            Self::SetFavorite { .. } => UiOperationName::SetFavorite,
            Self::ClearHistory => UiOperationName::ClearHistory,
            Self::SaveProviderSettings { .. } => UiOperationName::SaveProviderSettings,
            Self::SaveNetworkSettings { .. } => UiOperationName::SaveNetworkSettings,
            Self::TestProvider { .. } => UiOperationName::TestProvider,
            Self::Login { .. } => UiOperationName::Login,
            Self::Logout => UiOperationName::Logout,
            Self::ManualSync => UiOperationName::ManualSync,
            Self::TestConnection { .. } => UiOperationName::TestConnection,
            Self::RevokeDevice { .. } => UiOperationName::RevokeDevice,
            Self::RegenerateDeviceIdentity => UiOperationName::RegenerateDeviceIdentity,
            Self::ExportData { .. } => UiOperationName::ExportData,
            Self::PreviewImport { .. } => UiOperationName::PreviewImport,
            Self::ApplyImport { .. } => UiOperationName::ApplyImport,
            Self::CheckUpdate => UiOperationName::CheckUpdate,
            Self::UpdateGlobalHotkey { .. } => UiOperationName::UpdateGlobalHotkey,
            Self::UpdateStartAtLogin { .. } => UiOperationName::UpdateStartAtLogin,
            Self::UpdateLaunchMinimized { .. } => UiOperationName::UpdateLaunchMinimized,
            Self::UpdatePopupIdleTimeout { .. } => UiOperationName::UpdatePopupIdleTimeout,
            Self::PopupFavoriteToggle { .. } => UiOperationName::PopupFavoriteToggle,
            Self::PopupRefresh { .. } => UiOperationName::PopupRefresh,
            Self::RequestSnapshot => UiOperationName::RequestSnapshot,
            Self::OpenAccessibilitySettings => UiOperationName::OpenAccessibilitySettings,
            Self::RequestAccessibilityPermission => UiOperationName::RequestAccessibilityPermission,
            Self::RestartAgent => UiOperationName::RestartAgent,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "output", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationOutput {
    None,
    ImportPreview {
        token: Uuid,
        history_add: u64,
        history_update: u64,
        favorite_add: u64,
        favorite_reactivate: u64,
        query_stats_archive: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationResult {
    pub response_to: Uuid,
    pub operation: UiOperationName,
    pub success: bool,
    pub feedback: UiFeedback,
    pub output: OperationOutput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentToUi {
    HandshakeAccepted(HandshakeAccepted),
    Snapshot(UiSnapshot),
    Patch(MainUiPatch),
    BeginLookup {
        display_session_id: Uuid,
        state: LookupUiState,
    },
    UpdateLookup {
        display_session_id: Uuid,
        state: LookupUiState,
    },
    HideLookup {
        display_session_id: Uuid,
    },
    OpenMainWindow,
    ShowPermission {
        status: String,
    },
    OperationResult(OperationResult),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiToAgent {
    Hello(Hello),
    Ready(UiReady),
    Request {
        request_id: Uuid,
        operation: UiOperation,
    },
    LookupDismissed {
        display_session_id: Uuid,
    },
    UiBlockingChanged {
        blocked: bool,
    },
    Exiting {
        process_generation: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    VersionMismatch { expected: u16, received: u16 },
    SessionMismatch,
    SequenceRegression { last: u64, received: u64 },
    FrameTooLarge(usize),
    UnexpectedMessage,
    AuthenticationFailed,
    Serialization(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionMismatch { expected, received } => write!(
                formatter,
                "IPC protocol version mismatch: expected {expected}, received {received}"
            ),
            Self::SessionMismatch => formatter.write_str("IPC Agent session mismatch"),
            Self::SequenceRegression { last, received } => write!(
                formatter,
                "IPC message sequence did not increase: last {last}, received {received}"
            ),
            Self::FrameTooLarge(size) => write!(formatter, "IPC frame exceeds limit: {size} bytes"),
            Self::UnexpectedMessage => formatter.write_str("unexpected IPC message"),
            Self::AuthenticationFailed => formatter.write_str("IPC authentication failed"),
            Self::Serialization(error) => write!(formatter, "IPC serialization failed: {error}"),
        }
    }
}

impl Error for ProtocolError {}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Protocol(ProtocolError),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "IPC transport failed: {error}"),
            Self::Protocol(error) => error.fmt(formatter),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Writes one bounded frame.
///
/// # Errors
/// Returns a protocol error when serialization exceeds the bound, or a transport error.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)
        .map_err(|error| FrameError::Protocol(ProtocolError::Serialization(error.to_string())))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Protocol(ProtocolError::FrameTooLarge(
            bytes.len(),
        )));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| FrameError::Protocol(ProtocolError::FrameTooLarge(bytes.len())))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads and decodes one bounded frame.
///
/// # Errors
/// Returns a protocol error for an oversized or malformed frame, or a transport error.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Protocol(ProtocolError::FrameTooLarge(length)));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| FrameError::Protocol(ProtocolError::Serialization(error.to_string())))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcEndpoint {
    #[cfg(target_os = "windows")]
    name: String,
    #[cfg(not(target_os = "windows"))]
    path: PathBuf,
}

impl IpcEndpoint {
    #[must_use]
    pub fn for_agent_session(data_root: &Path, session_id: Uuid) -> Self {
        #[cfg(target_os = "windows")]
        {
            let _ = data_root;
            Self {
                name: format!(r"\\.\pipe\site.niuniu770.lvos.{session_id}"),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self {
                path: data_root.join(format!("agent-{session_id}.sock")),
            }
        }
    }

    #[must_use]
    pub fn argument(&self) -> String {
        #[cfg(target_os = "windows")]
        {
            self.name.clone()
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.path.to_string_lossy().into_owned()
        }
    }

    #[must_use]
    pub fn from_argument(value: &str) -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                name: value.to_owned(),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self {
                path: PathBuf::from(value),
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub type ClientStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(target_os = "windows")]
pub type ServerStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(not(target_os = "windows"))]
pub type ClientStream = tokio::net::UnixStream;
#[cfg(not(target_os = "windows"))]
pub type ServerStream = tokio::net::UnixStream;

#[derive(Debug)]
pub struct LocalListener {
    endpoint: IpcEndpoint,
    #[cfg(not(target_os = "windows"))]
    listener: tokio::net::UnixListener,
    #[cfg(target_os = "windows")]
    first_instance: bool,
}

impl LocalListener {
    /// Binds a local endpoint for one Agent session.
    ///
    /// # Errors
    /// Returns an I/O error when the endpoint cannot be created or secured.
    pub fn bind(endpoint: IpcEndpoint) -> io::Result<Self> {
        #[cfg(target_os = "windows")]
        {
            Ok(Self {
                endpoint,
                first_instance: true,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(parent) = endpoint.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::remove_file(&endpoint.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let listener = tokio::net::UnixListener::bind(&endpoint.path)?;
            std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { endpoint, listener })
        }
    }

    /// Accepts one local GUI connection.
    ///
    /// # Errors
    /// Returns an I/O error when connection setup fails.
    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        #[cfg(target_os = "windows")]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let mut options = ServerOptions::new();
            options.reject_remote_clients(true);
            if self.first_instance {
                options.first_pipe_instance(true);
                self.first_instance = false;
            }
            let server = options.create(&self.endpoint.name)?;
            server.connect().await?;
            Ok(server)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let (stream, _) = self.listener.accept().await?;
            Ok(stream)
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for LocalListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.endpoint.path);
    }
}

/// Connects to an Agent endpoint with a bounded retry window.
///
/// # Errors
/// Returns the last I/O error when the timeout expires.
pub async fn connect(endpoint: &IpcEndpoint, timeout: Duration) -> io::Result<ClientStream> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        #[cfg(target_os = "windows")]
        let result = tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.name);
        #[cfg(not(target_os = "windows"))]
        let result = tokio::net::UnixStream::connect(&endpoint.path).await;
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_fields_are_redacted_from_debug_output() {
        let operation = UiOperation::Login {
            server: "https://example.invalid".to_owned(),
            username: "user".to_owned(),
            password: SensitiveString::new("never-log-this"),
        };
        let debug = format!("{operation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("never-log-this"));
    }

    #[test]
    fn envelopes_reject_other_versions_and_sessions() {
        let session = Uuid::new_v4();
        let mut envelope = Envelope::new(session, 1, AgentToUi::Shutdown);
        assert!(envelope.validate(session).is_ok());
        envelope.protocol_version = PROTOCOL_VERSION.saturating_add(1);
        assert!(matches!(
            envelope.validate(session),
            Err(ProtocolError::VersionMismatch { .. })
        ));
        envelope.protocol_version = PROTOCOL_VERSION;
        assert_eq!(
            envelope.validate(Uuid::new_v4()),
            Err(ProtocolError::SessionMismatch)
        );
    }

    #[tokio::test]
    async fn framed_messages_round_trip_and_enforce_the_size_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut sender, mut receiver) = tokio::io::duplex(MAX_FRAME_BYTES + 16);
        let session = Uuid::new_v4();
        let message = Envelope::new(session, 1, AgentToUi::OpenMainWindow);
        write_frame(&mut sender, &message).await?;
        let decoded: Envelope<AgentToUi> = read_frame(&mut receiver).await?;
        assert_eq!(decoded, message);

        let oversized = "x".repeat(MAX_FRAME_BYTES);
        let result = write_frame(&mut sender, &oversized).await;
        assert!(matches!(
            result,
            Err(FrameError::Protocol(ProtocolError::FrameTooLarge(_)))
        ));
        Ok(())
    }
}
