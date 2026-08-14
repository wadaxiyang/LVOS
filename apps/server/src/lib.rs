#![forbid(unsafe_code)]

mod backup;
mod config;
mod repository;
mod storage;
mod sync_api;
mod sync_repository;

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Extension, Json, Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
use uuid::Uuid;

pub use backup::BackupService;
pub use config::{AppEnvironment, ConfigError, ServerConfig};
pub use repository::{DeviceRecord, RepositoryError, ServerRepository};
use repository::{Principal, SessionIdentity};

const MAX_USERNAME_BYTES: usize = 128;
const MAX_PASSWORD_BYTES: usize = 1_024;
const MAX_DEVICE_NAME_BYTES: usize = 128;
const MAX_TOKEN_BYTES: usize = 512;

#[derive(Clone)]
struct AppState {
    repository: ServerRepository,
    config: Arc<ServerConfig>,
    limiter: Arc<LoginRateLimiter>,
    dummy_password_hash: Arc<String>,
    revision_hub: sync_api::RevisionHub,
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("repository", &self.repository)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Initializes bootstrap identity and returns the complete Stage 08 Axum application.
///
/// # Errors
/// Returns a typed initialization error if password hashing or persistence fails.
pub async fn build_app(
    config: ServerConfig,
    repository: ServerRepository,
) -> Result<Router, ServerInitError> {
    config.validate()?;
    if config.bootstrap_default_user {
        let password = config
            .default_password
            .clone()
            .ok_or(ConfigError::MissingBootstrapPassword)?;
        bootstrap_user(
            repository.clone(),
            config.default_username.clone(),
            password,
        )
        .await?;
    }
    let dummy_password_hash = hash_password("lvos-dummy-password".to_owned()).await?;
    let max_body = config.max_request_body_bytes;
    let max_sync_body = config.max_sync_body_bytes;
    let state = AppState {
        repository,
        config: Arc::new(config),
        limiter: Arc::new(LoginRateLimiter::default()),
        dummy_password_hash: Arc::new(dummy_password_hash),
        revision_hub: sync_api::RevisionHub::default(),
    };

    let sync = Router::new()
        .route("/api/v1/sync/push", post(sync_api::push))
        .route("/api/v1/sync/changes", get(sync_api::changes))
        .route("/api/v1/sync/stream", get(sync_api::stream))
        .route("/api/v1/favorites", get(sync_api::favorites))
        .route(
            "/api/v1/favorites/{content_key}/state",
            patch(sync_api::set_favorite_state),
        )
        .layer(RequestBodyLimitLayer::new(max_sync_body));

    let protected = Router::new()
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(current_user))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{device_id}/revoke", post(revoke_device))
        .merge(sync)
        .route_layer(from_fn_with_state(state.clone(), require_access));

    Ok(Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .merge(protected)
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

/// Creates an Argon2id-backed User without replacing an existing username's password.
///
/// # Errors
/// Returns a typed initialization error if hashing or persistence fails.
pub async fn bootstrap_user(
    repository: ServerRepository,
    username: String,
    password: String,
) -> Result<String, ServerInitError> {
    let password_hash = hash_password(password).await?;
    let now = unix_timestamp().map_err(|_| ServerInitError::Clock)?;
    tokio::task::spawn_blocking(move || repository.bootstrap_user(&username, &password_hash, now))
        .await
        .map_err(|_| ServerInitError::Task)?
        .map_err(ServerInitError::Repository)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    validate_login_request(&request)?;
    let now = unix_timestamp()?;
    if state.config.login_rate_limit_enabled
        && state.limiter.is_blocked(
            &request.username,
            now,
            state.config.login_rate_limit_max_failures,
            state.config.login_rate_limit_window_seconds,
        )?
    {
        return Err(ApiError::rate_limited());
    }

    let repository = state.repository.clone();
    let username = request.username.clone();
    let user = blocking(move || repository.user_by_username(&username)).await?;
    let verification_hash = user
        .as_ref()
        .map_or_else(
            || state.dummy_password_hash.as_str(),
            |user| &user.password_hash,
        )
        .to_owned();
    let password_valid = verify_password(request.password.clone(), verification_hash).await?;
    let Some(user) = user.filter(|user| password_valid && !user.disabled) else {
        if state.config.login_rate_limit_enabled {
            state.limiter.record_failure(
                &request.username,
                now,
                state.config.login_rate_limit_window_seconds,
            )?;
        }
        return Err(ApiError::invalid_credentials());
    };
    state.limiter.clear(&request.username)?;

    let tokens = NewTokens::generate(now, state.config.access_token_ttl_seconds)?;
    let repository = state.repository.clone();
    let device_id = request.device_id.clone();
    let platform = request.platform.clone();
    let device_name = request.device_name.clone();
    let access_hash = tokens.access_hash.clone();
    let refresh_hash = tokens.refresh_hash.clone();
    let identity = blocking(move || {
        repository.create_session(
            &user,
            &device_id,
            &platform,
            device_name.as_deref(),
            &access_hash,
            &refresh_hash,
            tokens.access_expires_at,
            now,
        )
    })
    .await
    .map_err(|error| {
        if error.code == "device_revoked" {
            ApiError::device_revoked_for_login()
        } else {
            error
        }
    })?;
    Ok(Json(TokenResponse::new(tokens, identity)))
}

async fn refresh(
    State(state): State<AppState>,
    Json(request): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    if request.refresh_token.is_empty() || request.refresh_token.len() > MAX_TOKEN_BYTES {
        return Err(ApiError::invalid_session());
    }
    let now = unix_timestamp()?;
    let old_hash = token_hash(&request.refresh_token);
    let tokens = NewTokens::generate(now, state.config.access_token_ttl_seconds)?;
    let repository = state.repository.clone();
    let access_hash = tokens.access_hash.clone();
    let refresh_hash = tokens.refresh_hash.clone();
    let refresh_idle_ttl = state.config.refresh_idle_ttl_seconds;
    let identity = blocking(move || {
        repository.rotate_refresh(
            &old_hash,
            &access_hash,
            &refresh_hash,
            tokens.access_expires_at,
            refresh_idle_ttl,
            now,
        )
    })
    .await?;
    Ok(Json(TokenResponse::new(tokens, identity)))
}

async fn logout(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<StatusCode, ApiError> {
    let repository = state.repository.clone();
    let now = unix_timestamp()?;
    blocking(move || repository.revoke_session(&principal.session_id, now)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn current_user(Extension(principal): Extension<Principal>) -> Json<CurrentUserResponse> {
    Json(CurrentUserResponse {
        user: UserResponse {
            user_id: principal.user_id,
            username: principal.username,
        },
        device: DeviceIdentityResponse {
            device_id: principal.device_id,
            platform: principal.platform,
        },
        latest_revision: principal.latest_revision,
    })
}

async fn list_devices(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<DevicesResponse>, ApiError> {
    let repository = state.repository.clone();
    let devices = blocking(move || repository.devices_for_user(&principal.user_id)).await?;
    Ok(Json(DevicesResponse { devices }))
}

async fn revoke_device(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(device_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    validate_device_id(&device_id)?;
    let repository = state.repository.clone();
    let now = unix_timestamp()?;
    blocking(move || repository.revoke_device(&principal.user_id, &device_id, now)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn require_access(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token(request.headers().get(header::AUTHORIZATION))?;
    let access_hash = token_hash(token);
    let repository = state.repository.clone();
    let now = unix_timestamp()?;
    let principal = blocking(move || repository.authenticate_access(&access_hash, now)).await?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

fn bearer_token(header_value: Option<&HeaderValue>) -> Result<&str, ApiError> {
    let value = header_value
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::invalid_session)?;
    let token = value
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_BYTES)
        .ok_or_else(ApiError::invalid_session)?;
    Ok(token)
}

fn validate_login_request(request: &LoginRequest) -> Result<(), ApiError> {
    if request.username.is_empty()
        || request.username.len() > MAX_USERNAME_BYTES
        || request.password.is_empty()
        || request.password.len() > MAX_PASSWORD_BYTES
        || request
            .device_name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_DEVICE_NAME_BYTES)
    {
        return Err(ApiError::invalid_request("login fields are invalid"));
    }
    validate_device_id(&request.device_id)?;
    if !matches!(request.platform.as_str(), "windows" | "macos") {
        return Err(ApiError::invalid_request(
            "platform must be windows or macos",
        ));
    }
    Ok(())
}

fn validate_device_id(device_id: &str) -> Result<(), ApiError> {
    Uuid::parse_str(device_id)
        .map(|_| ())
        .map_err(|_| ApiError::invalid_request("device_id must be a UUID"))
}

async fn blocking<T, F>(operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RepositoryError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::from)
}

async fn hash_password(password: String) -> Result<String, ServerInitError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
            .map_err(|_| ServerInitError::PasswordHash)?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| ServerInitError::PasswordHash)
    })
    .await
    .map_err(|_| ServerInitError::Task)?
}

async fn verify_password(password: String, encoded: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&encoded).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .map_err(|_| ApiError::internal())
}

fn unix_timestamp() -> Result<i64, ApiError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApiError::internal())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| ApiError::internal())
}

struct NewTokens {
    access_token: String,
    refresh_token: String,
    access_hash: String,
    refresh_hash: String,
    access_expires_at: i64,
}

impl NewTokens {
    fn generate(now: i64, ttl_seconds: i64) -> Result<Self, ApiError> {
        let access_token = random_token("lvos_at");
        let refresh_token = random_token("lvos_rt");
        let access_expires_at = now
            .checked_add(ttl_seconds)
            .ok_or_else(ApiError::internal)?;
        Ok(Self {
            access_hash: token_hash(&access_token),
            refresh_hash: token_hash(&refresh_token),
            access_token,
            refresh_token,
            access_expires_at,
        })
    }
}

fn random_token(prefix: &str) -> String {
    format!(
        "{prefix}_{}_{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn token_hash(token: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Default)]
struct LoginRateLimiter {
    attempts: Mutex<HashMap<String, AttemptBucket>>,
}

struct AttemptBucket {
    failures: u32,
    window_started_at: i64,
}

impl LoginRateLimiter {
    fn is_blocked(
        &self,
        username: &str,
        now: i64,
        max_failures: u32,
        window_seconds: i64,
    ) -> Result<bool, ApiError> {
        let mut attempts = self.attempts.lock().map_err(|_| ApiError::internal())?;
        let Some(bucket) = attempts.get_mut(username) else {
            return Ok(false);
        };
        if now.saturating_sub(bucket.window_started_at) >= window_seconds {
            attempts.remove(username);
            return Ok(false);
        }
        Ok(bucket.failures >= max_failures)
    }

    fn record_failure(
        &self,
        username: &str,
        now: i64,
        window_seconds: i64,
    ) -> Result<(), ApiError> {
        let mut attempts = self.attempts.lock().map_err(|_| ApiError::internal())?;
        let bucket = attempts
            .entry(username.to_owned())
            .or_insert(AttemptBucket {
                failures: 0,
                window_started_at: now,
            });
        if now.saturating_sub(bucket.window_started_at) >= window_seconds {
            bucket.failures = 0;
            bucket.window_started_at = now;
        }
        bucket.failures = bucket.failures.saturating_add(1);
        Ok(())
    }

    fn clear(&self, username: &str) -> Result<(), ApiError> {
        self.attempts
            .lock()
            .map_err(|_| ApiError::internal())?
            .remove(username);
        Ok(())
    }
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    device_id: String,
    platform: String,
    device_name: Option<String>,
}

#[derive(Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    access_expires_at: i64,
    refresh_token: String,
    user: UserResponse,
    device: DeviceIdentityResponse,
    latest_revision: i64,
}

impl TokenResponse {
    fn new(tokens: NewTokens, identity: SessionIdentity) -> Self {
        Self {
            access_token: tokens.access_token,
            access_expires_at: tokens.access_expires_at,
            refresh_token: tokens.refresh_token,
            user: UserResponse {
                user_id: identity.user_id,
                username: identity.username,
            },
            device: DeviceIdentityResponse {
                device_id: identity.device_id,
                platform: identity.platform,
            },
            latest_revision: identity.latest_revision,
        }
    }
}

#[derive(Serialize)]
struct UserResponse {
    user_id: String,
    username: String,
}

#[derive(Serialize)]
struct DeviceIdentityResponse {
    device_id: String,
    platform: String,
}

#[derive(Serialize)]
struct CurrentUserResponse {
    user: UserResponse,
    device: DeviceIdentityResponse,
    latest_revision: i64,
}

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<DeviceRecord>,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    re_registration_supported: Option<bool>,
}

#[derive(Clone, Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    re_registration_supported: Option<bool>,
}

impl ApiError {
    fn invalid_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
            re_registration_supported: None,
        }
    }

    fn invalid_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_credentials",
            message: "username or password is invalid",
            re_registration_supported: None,
        }
    }

    fn invalid_session() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_session",
            message: "authentication is required",
            re_registration_supported: None,
        }
    }

    fn device_revoked_for_login() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "device_revoked",
            message: "this device identity is permanently revoked for the user",
            re_registration_supported: Some(true),
        }
    }

    fn rate_limited() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "login_rate_limited",
            message: "too many failed login attempts",
            re_registration_supported: None,
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "the server could not complete the request",
            re_registration_supported: None,
        }
    }
}

impl From<RepositoryError> for ApiError {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::NotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "not_found",
                message: "the requested resource was not found",
                re_registration_supported: None,
            },
            RepositoryError::AccessExpired => Self {
                status: StatusCode::UNAUTHORIZED,
                code: "access_token_expired",
                message: "the access token expired",
                re_registration_supported: None,
            },
            RepositoryError::DeviceRevoked => Self {
                status: StatusCode::UNAUTHORIZED,
                code: "device_revoked",
                message: "the authenticated device is revoked",
                re_registration_supported: Some(true),
            },
            RepositoryError::RefreshExpired
            | RepositoryError::SessionInvalid
            | RepositoryError::SessionRevoked
            | RepositoryError::UserDisabled => Self::invalid_session(),
            RepositoryError::Database
            | RepositoryError::Migration
            | RepositoryError::Backup
            | RepositoryError::Restore
            | RepositoryError::Integrity => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    re_registration_supported: self.re_registration_supported,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Debug)]
pub enum ServerInitError {
    Config(ConfigError),
    Repository(RepositoryError),
    PasswordHash,
    Clock,
    Task,
}

impl From<ConfigError> for ServerInitError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl fmt::Display for ServerInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "server configuration failed: {error}"),
            Self::Repository(error) => write!(formatter, "server repository failed: {error}"),
            Self::PasswordHash => formatter.write_str("password hashing failed"),
            Self::Clock => formatter.write_str("system clock is before the Unix epoch"),
            Self::Task => formatter.write_str("a blocking server initialization task failed"),
        }
    }
}

impl std::error::Error for ServerInitError {}

#[cfg(test)]
mod tests;
