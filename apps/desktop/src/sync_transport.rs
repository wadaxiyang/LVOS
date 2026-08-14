use std::{fmt, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use lvos_sync::{ChangesResponse, FavoriteRecord, PushRequest, PushResponse, RevisionNotice};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_ERROR_BODY_BYTES: u64 = 65_536;
const MAX_JSON_BODY_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_SSE_BUFFER_BYTES: usize = 65_536;

#[derive(Clone, Eq, PartialEq)]
pub struct LoginIdentity {
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub platform: String,
    pub access_token: String,
    pub access_expires_at: i64,
    pub refresh_token: String,
    pub latest_revision: u64,
}

impl fmt::Debug for LoginIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginIdentity")
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .field("device_id", &self.device_id)
            .field("platform", &self.platform)
            .field("access_token", &"[REDACTED]")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_token", &"[REDACTED]")
            .field("latest_revision", &self.latest_revision)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RefreshedTokens {
    pub access_token: String,
    pub access_expires_at: i64,
    pub refresh_token: String,
}

impl fmt::Debug for RefreshedTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshedTokens")
            .field("access_token", &"[REDACTED]")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct LoginCredentials {
    pub username: String,
    pub password: String,
    pub device_id: String,
    pub platform: String,
    pub device_name: Option<String>,
}

impl fmt::Debug for LoginCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("device_id", &self.device_id)
            .field("platform", &self.platform)
            .field("device_name", &self.device_name)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RemoteDevice {
    pub device_id: String,
    pub platform: String,
    pub device_name: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct FavoriteConflict {
    pub event_id: Option<String>,
    pub current: Option<FavoriteRecord>,
    pub latest_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionStreamEvent {
    Connected,
    Revision(u64),
}

#[derive(Clone, Debug)]
pub enum TransportError {
    Offline,
    AccessExpired,
    SessionInvalid,
    DeviceRevoked,
    Conflict(Box<FavoriteConflict>),
    Rejected,
    InvalidResponse,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Offline => "the sync server is unavailable",
            Self::AccessExpired => "the access token expired",
            Self::SessionInvalid => "the sync session is invalid",
            Self::DeviceRevoked => "the current device is revoked",
            Self::Conflict(_) => "the Favorite revision conflicts with the server",
            Self::Rejected => "the sync server rejected the request",
            Self::InvalidResponse => "the sync server returned an invalid response",
        })
    }
}

impl std::error::Error for TransportError {}

#[async_trait]
pub trait SyncTransport: Send + Sync {
    async fn login(
        &self,
        server_origin: &str,
        credentials: &LoginCredentials,
    ) -> Result<LoginIdentity, TransportError>;

    async fn refresh(
        &self,
        server_origin: &str,
        refresh_token: &str,
    ) -> Result<RefreshedTokens, TransportError>;

    async fn logout(&self, server_origin: &str, access_token: &str) -> Result<(), TransportError>;

    async fn push(
        &self,
        server_origin: &str,
        access_token: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError>;

    async fn changes(
        &self,
        server_origin: &str,
        access_token: &str,
        since: u64,
        limit: u32,
    ) -> Result<ChangesResponse, TransportError>;

    async fn devices(
        &self,
        server_origin: &str,
        access_token: &str,
    ) -> Result<Vec<RemoteDevice>, TransportError>;

    async fn revoke_device(
        &self,
        server_origin: &str,
        access_token: &str,
        device_id: &str,
    ) -> Result<(), TransportError>;

    async fn revision_stream(
        &self,
        server_origin: &str,
        access_token: &str,
        events: mpsc::Sender<RevisionStreamEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), TransportError>;
}

#[derive(Clone, Debug)]
pub struct HttpSyncTransport {
    client: reqwest::Client,
}

impl HttpSyncTransport {
    /// Builds the bounded HTTPS transport used by Desktop synchronization.
    ///
    /// # Errors
    /// Returns an error when the HTTP client cannot be initialized.
    pub fn new() -> Result<Self, TransportError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(format!("LVOS/{}", lvos_core::SOFTWARE_VERSION))
            .build()
            .map_err(|_| TransportError::InvalidResponse)?;
        Ok(Self { client })
    }

    fn endpoint(server_origin: &str, path: &str) -> Result<Url, TransportError> {
        let origin = Url::parse(server_origin).map_err(|_| TransportError::InvalidResponse)?;
        if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
            return Err(TransportError::InvalidResponse);
        }
        if !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(TransportError::InvalidResponse);
        }
        origin
            .join(path)
            .map_err(|_| TransportError::InvalidResponse)
    }

    async fn json_response<T: DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, TransportError> {
        if response.status().is_success() {
            let body = bounded_response_body(response, MAX_JSON_BODY_BYTES).await?;
            return serde_json::from_slice(&body).map_err(|_| TransportError::InvalidResponse);
        }
        Err(response_error(response).await)
    }
}

#[async_trait]
impl SyncTransport for HttpSyncTransport {
    async fn login(
        &self,
        server_origin: &str,
        credentials: &LoginCredentials,
    ) -> Result<LoginIdentity, TransportError> {
        let response = self
            .client
            .post(Self::endpoint(server_origin, "/api/v1/auth/login")?)
            .json(&LoginRequest::from(credentials))
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        let tokens: TokenResponse = Self::json_response(response).await?;
        Ok(tokens.into_login_identity())
    }

    async fn refresh(
        &self,
        server_origin: &str,
        refresh_token: &str,
    ) -> Result<RefreshedTokens, TransportError> {
        let response = self
            .client
            .post(Self::endpoint(server_origin, "/api/v1/auth/refresh")?)
            .json(&RefreshRequest { refresh_token })
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        let tokens: TokenResponse = Self::json_response(response).await?;
        Ok(RefreshedTokens {
            access_token: tokens.access_token,
            access_expires_at: tokens.access_expires_at,
            refresh_token: tokens.refresh_token,
        })
    }

    async fn logout(&self, server_origin: &str, access_token: &str) -> Result<(), TransportError> {
        empty_response(
            self.client
                .post(Self::endpoint(server_origin, "/api/v1/auth/logout")?)
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|_| TransportError::Offline)?,
        )
        .await
    }

    async fn push(
        &self,
        server_origin: &str,
        access_token: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let response = self
            .client
            .post(Self::endpoint(server_origin, "/api/v1/sync/push")?)
            .bearer_auth(access_token)
            .json(request)
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        Self::json_response(response).await
    }

    async fn changes(
        &self,
        server_origin: &str,
        access_token: &str,
        since: u64,
        limit: u32,
    ) -> Result<ChangesResponse, TransportError> {
        let response = self
            .client
            .get(Self::endpoint(server_origin, "/api/v1/sync/changes")?)
            .bearer_auth(access_token)
            .query(&[("since", since), ("limit", u64::from(limit))])
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        Self::json_response(response).await
    }

    async fn devices(
        &self,
        server_origin: &str,
        access_token: &str,
    ) -> Result<Vec<RemoteDevice>, TransportError> {
        let response = self
            .client
            .get(Self::endpoint(server_origin, "/api/v1/devices")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        let response: DevicesResponse = Self::json_response(response).await?;
        Ok(response.devices)
    }

    async fn revoke_device(
        &self,
        server_origin: &str,
        access_token: &str,
        device_id: &str,
    ) -> Result<(), TransportError> {
        empty_response(
            self.client
                .post(Self::endpoint(
                    server_origin,
                    &format!("/api/v1/devices/{device_id}/revoke"),
                )?)
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|_| TransportError::Offline)?,
        )
        .await
    }

    async fn revision_stream(
        &self,
        server_origin: &str,
        access_token: &str,
        events: mpsc::Sender<RevisionStreamEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), TransportError> {
        let response = self
            .client
            .get(Self::endpoint(server_origin, "/api/v1/sync/stream")?)
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|_| TransportError::Offline)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err(TransportError::InvalidResponse);
        }
        if events.send(RevisionStreamEvent::Connected).await.is_err() {
            return Ok(());
        }
        let mut chunks = response.bytes_stream();
        let mut buffer = String::new();
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                chunk = chunks.next() => {
                    let Some(chunk) = chunk else { return Err(TransportError::Offline); };
                    let chunk = chunk.map_err(|_| TransportError::Offline)?;
                    let text = std::str::from_utf8(&chunk)
                        .map_err(|_| TransportError::InvalidResponse)?;
                    buffer.push_str(text);
                    if buffer.len() > MAX_SSE_BUFFER_BYTES {
                        return Err(TransportError::InvalidResponse);
                    }
                    while let Some(index) = sse_boundary(&buffer) {
                        let frame = buffer[..index].to_owned();
                        let drain_to = index + if buffer[index..].starts_with("\r\n\r\n") { 4 } else { 2 };
                        buffer.drain(..drain_to);
                        if let Some(revision) = parse_revision_frame(&frame)?
                            && events
                                .send(RevisionStreamEvent::Revision(revision))
                                .await
                                .is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

fn sse_boundary(buffer: &str) -> Option<usize> {
    match (buffer.find("\r\n\r\n"), buffer.find("\n\n")) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

fn parse_revision_frame(frame: &str) -> Result<Option<u64>, TransportError> {
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<RevisionNotice>(&data)
        .map(|notice| Some(notice.latest_revision))
        .map_err(|_| TransportError::InvalidResponse)
}

async fn empty_response(response: reqwest::Response) -> Result<(), TransportError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(response_error(response).await)
    }
}

async fn response_error(response: reqwest::Response) -> TransportError {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ERROR_BODY_BYTES)
    {
        return TransportError::InvalidResponse;
    }
    let body = match bounded_response_body(
        response,
        usize::try_from(MAX_ERROR_BODY_BYTES).unwrap_or(usize::MAX),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => return error,
    };
    let envelope = serde_json::from_slice::<ErrorEnvelope>(&body).ok();
    let code = envelope.as_ref().map(|value| value.error.code.as_str());
    match (status, code) {
        (StatusCode::UNAUTHORIZED, Some("access_token_expired")) => TransportError::AccessExpired,
        (StatusCode::UNAUTHORIZED, Some("device_revoked")) => TransportError::DeviceRevoked,
        (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN, _) => TransportError::SessionInvalid,
        (StatusCode::CONFLICT, Some("favorite_conflict")) => serde_json::from_slice::<
            ConflictEnvelope,
        >(&body)
        .map_or(TransportError::InvalidResponse, |value| {
            TransportError::Conflict(Box::new(FavoriteConflict {
                event_id: value.error.event_id,
                current: value.error.current,
                latest_revision: value.error.latest_revision,
            }))
        }),
        _ if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
            TransportError::Offline
        }
        _ => TransportError::Rejected,
    }
}

async fn bounded_response_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, TransportError> {
    if response
        .content_length()
        .is_some_and(|size| usize::try_from(size).map_or(true, |size| size > limit))
    {
        return Err(TransportError::InvalidResponse);
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| TransportError::Offline)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(TransportError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
    device_id: &'a str,
    platform: &'a str,
    device_name: Option<&'a str>,
}

impl<'a> From<&'a LoginCredentials> for LoginRequest<'a> {
    fn from(value: &'a LoginCredentials) -> Self {
        Self {
            username: &value.username,
            password: &value.password,
            device_id: &value.device_id,
            platform: &value.platform,
            device_name: value.device_name.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    access_expires_at: i64,
    refresh_token: String,
    user: UserResponse,
    device: DeviceResponse,
    latest_revision: u64,
}

impl TokenResponse {
    fn into_login_identity(self) -> LoginIdentity {
        LoginIdentity {
            user_id: self.user.user_id,
            username: self.user.username,
            device_id: self.device.device_id,
            platform: self.device.platform,
            access_token: self.access_token,
            access_expires_at: self.access_expires_at,
            refresh_token: self.refresh_token,
            latest_revision: self.latest_revision,
        }
    }
}

#[derive(Deserialize)]
struct UserResponse {
    user_id: String,
    username: String,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_id: String,
    platform: String,
}

#[derive(Deserialize)]
struct DevicesResponse {
    devices: Vec<RemoteDevice>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
}

#[derive(Deserialize)]
struct ConflictEnvelope {
    error: ConflictBody,
}

#[derive(Deserialize)]
struct ConflictBody {
    event_id: Option<String>,
    current: Option<FavoriteRecord>,
    latest_revision: u64,
}
