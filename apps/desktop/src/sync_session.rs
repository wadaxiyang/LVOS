use std::{error::Error, fmt, sync::Arc};

use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use lvos_sync::{ChangesResponse, PushRequest, PushResponse};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    LoginCredentials, LoginIdentity, RemoteDevice, RevisionStreamEvent, SyncTransport,
    TransportError,
};

#[derive(Clone)]
struct AccessState {
    token: String,
    expires_at: i64,
}

impl fmt::Debug for AccessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessState")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// One authenticated Server session with persisted refresh-token rotation.
pub struct AuthenticatedSession<T> {
    transport: Arc<T>,
    credentials: Arc<dyn CredentialStore>,
    server_origin: String,
    user_id: String,
    username: String,
    device_id: String,
    platform: String,
    access: RwLock<AccessState>,
    refresh_gate: Mutex<()>,
}

impl<T> fmt::Debug for AuthenticatedSession<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedSession")
            .field("server_origin", &self.server_origin)
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .field("device_id", &self.device_id)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl<T: SyncTransport + 'static> AuthenticatedSession<T> {
    /// Authenticates and persists only the refresh token in the OS Credential Store.
    ///
    /// # Errors
    /// Returns an error for transport rejection or Credential Store failure.
    pub async fn login(
        transport: Arc<T>,
        credential_store: Arc<dyn CredentialStore>,
        server_origin: String,
        login: &LoginCredentials,
    ) -> Result<(Self, LoginIdentity), SessionError> {
        let identity = transport.login(&server_origin, login).await?;
        let scope = refresh_scope(&server_origin, &identity.user_id, &identity.device_id);
        credential_store.set(&scope, identity.refresh_token.as_bytes())?;
        let session = Self::from_identity(transport, credential_store, server_origin, &identity);
        Ok((session, identity))
    }

    /// Restores a persistent session by rotating the saved refresh token immediately.
    ///
    /// # Errors
    /// Returns an error when the credential is absent, invalid, revoked, or cannot be rotated.
    pub async fn resume(
        transport: Arc<T>,
        credential_store: Arc<dyn CredentialStore>,
        server_origin: String,
        user_id: String,
        username: String,
        device_id: String,
        platform: String,
    ) -> Result<Self, SessionError> {
        let scope = refresh_scope(&server_origin, &user_id, &device_id);
        let refresh = credential_store
            .get(&scope)?
            .ok_or(SessionError::NoPersistentSession)?;
        let refresh = String::from_utf8(refresh).map_err(|_| SessionError::InvalidCredential)?;
        let tokens = transport.refresh(&server_origin, &refresh).await?;
        credential_store.set(&scope, tokens.refresh_token.as_bytes())?;
        Ok(Self {
            transport,
            credentials: credential_store,
            server_origin,
            user_id,
            username,
            device_id,
            platform,
            access: RwLock::new(AccessState {
                token: tokens.access_token,
                expires_at: tokens.access_expires_at,
            }),
            refresh_gate: Mutex::new(()),
        })
    }

    fn from_identity(
        transport: Arc<T>,
        credentials: Arc<dyn CredentialStore>,
        server_origin: String,
        identity: &LoginIdentity,
    ) -> Self {
        Self {
            transport,
            credentials,
            server_origin,
            user_id: identity.user_id.clone(),
            username: identity.username.clone(),
            device_id: identity.device_id.clone(),
            platform: identity.platform.clone(),
            access: RwLock::new(AccessState {
                token: identity.access_token.clone(),
                expires_at: identity.access_expires_at,
            }),
            refresh_gate: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn server_origin(&self) -> &str {
        &self.server_origin
    }

    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    async fn token(&self) -> String {
        self.access.read().await.token.clone()
    }

    async fn refresh_after(&self, rejected_token: &str) -> Result<(), SessionError> {
        let _guard = self.refresh_gate.lock().await;
        if self.access.read().await.token != rejected_token {
            return Ok(());
        }
        let scope = self.credential_scope();
        let refresh = self
            .credentials
            .get(&scope)?
            .ok_or(SessionError::NoPersistentSession)?;
        let refresh = String::from_utf8(refresh).map_err(|_| SessionError::InvalidCredential)?;
        let tokens = self
            .transport
            .refresh(&self.server_origin, &refresh)
            .await?;
        // Persist the rotated refresh token before making its paired access token visible.
        self.credentials
            .set(&scope, tokens.refresh_token.as_bytes())?;
        *self.access.write().await = AccessState {
            token: tokens.access_token,
            expires_at: tokens.access_expires_at,
        };
        Ok(())
    }

    fn credential_scope(&self) -> CredentialScope {
        refresh_scope(&self.server_origin, &self.user_id, &self.device_id)
    }

    /// Pushes a batch, rotating credentials and retrying exactly once on access expiry.
    ///
    /// # Errors
    /// Returns the final transport or Credential Store error.
    pub async fn push(&self, request: &PushRequest) -> Result<PushResponse, SessionError> {
        let token = self.token().await;
        match self
            .transport
            .push(&self.server_origin, &token, request)
            .await
        {
            Err(TransportError::AccessExpired) => {
                self.refresh_after(&token).await?;
                let retry = self.token().await;
                Ok(self
                    .transport
                    .push(&self.server_origin, &retry, request)
                    .await?)
            }
            result => Ok(result?),
        }
    }

    /// Pulls a page, rotating credentials and retrying exactly once on access expiry.
    ///
    /// # Errors
    /// Returns the final transport or Credential Store error.
    pub async fn changes(&self, since: u64, limit: u32) -> Result<ChangesResponse, SessionError> {
        let token = self.token().await;
        match self
            .transport
            .changes(&self.server_origin, &token, since, limit)
            .await
        {
            Err(TransportError::AccessExpired) => {
                self.refresh_after(&token).await?;
                let retry = self.token().await;
                Ok(self
                    .transport
                    .changes(&self.server_origin, &retry, since, limit)
                    .await?)
            }
            result => Ok(result?),
        }
    }

    /// Lists account devices with one access-expiry refresh attempt.
    ///
    /// # Errors
    /// Returns the final transport or Credential Store error.
    pub async fn devices(&self) -> Result<Vec<RemoteDevice>, SessionError> {
        let token = self.token().await;
        match self.transport.devices(&self.server_origin, &token).await {
            Err(TransportError::AccessExpired) => {
                self.refresh_after(&token).await?;
                let retry = self.token().await;
                Ok(self.transport.devices(&self.server_origin, &retry).await?)
            }
            result => Ok(result?),
        }
    }

    /// Revokes one account device with one access-expiry refresh attempt.
    ///
    /// # Errors
    /// Returns the final transport or Credential Store error.
    pub async fn revoke_device(&self, device_id: &str) -> Result<(), SessionError> {
        let token = self.token().await;
        match self
            .transport
            .revoke_device(&self.server_origin, &token, device_id)
            .await
        {
            Err(TransportError::AccessExpired) => {
                self.refresh_after(&token).await?;
                let retry = self.token().await;
                Ok(self
                    .transport
                    .revoke_device(&self.server_origin, &retry, device_id)
                    .await?)
            }
            result => Ok(result?),
        }
    }

    /// Runs the revision-notice stream with one access-expiry refresh attempt.
    ///
    /// # Errors
    /// Returns the final transport or Credential Store error.
    pub async fn revision_stream(
        &self,
        events: mpsc::Sender<RevisionStreamEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), SessionError> {
        let token = self.token().await;
        match self
            .transport
            .revision_stream(
                &self.server_origin,
                &token,
                events.clone(),
                cancellation.clone(),
            )
            .await
        {
            Err(TransportError::AccessExpired) => {
                self.refresh_after(&token).await?;
                let retry = self.token().await;
                Ok(self
                    .transport
                    .revision_stream(&self.server_origin, &retry, events, cancellation)
                    .await?)
            }
            result => Ok(result?),
        }
    }

    /// Revokes the Server session and always removes its local refresh credential.
    ///
    /// # Errors
    /// Returns a transport error after local credential removal, or a Credential Store error.
    pub async fn logout(&self) -> Result<(), SessionError> {
        let token = self.token().await;
        let first = self.transport.logout(&self.server_origin, &token).await;
        let server_result = if matches!(first, Err(TransportError::AccessExpired)) {
            match self.refresh_after(&token).await {
                Ok(()) => {
                    let retry = self.token().await;
                    self.transport.logout(&self.server_origin, &retry).await
                }
                Err(error) => {
                    self.credentials.delete(&self.credential_scope())?;
                    return Err(error);
                }
            }
        } else {
            first
        };
        self.credentials.delete(&self.credential_scope())?;
        server_result.map_err(SessionError::Transport)
    }
}

fn refresh_scope(server_origin: &str, user_id: &str, device_id: &str) -> CredentialScope {
    CredentialScope {
        server_origin: server_origin.to_owned(),
        user_id: user_id.to_owned(),
        device_id: device_id.to_owned(),
        key: CredentialKey::ServerRefreshToken,
    }
}

#[derive(Debug)]
pub enum SessionError {
    Transport(TransportError),
    CredentialStore(AuthError),
    NoPersistentSession,
    InvalidCredential,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "Server session failed: {error}"),
            Self::CredentialStore(error) => write!(formatter, "Credential Store failed: {error}"),
            Self::NoPersistentSession => formatter.write_str("no persistent Server session exists"),
            Self::InvalidCredential => {
                formatter.write_str("the persistent Server credential is invalid")
            }
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::CredentialStore(error) => Some(error),
            Self::NoPersistentSession | Self::InvalidCredential => None,
        }
    }
}

impl From<TransportError> for SessionError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<AuthError> for SessionError {
    fn from(value: AuthError) -> Self {
        Self::CredentialStore(value)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex as StdMutex};

    use async_trait::async_trait;
    use lvos_sync::{ChangesResponse, PushResponse};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{RefreshedTokens, TransportError};

    #[derive(Debug, Default)]
    struct MemoryCredentials(StdMutex<HashMap<CredentialScope, Vec<u8>>>);

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

    #[derive(Debug, Default)]
    struct ExpiringTransport {
        refreshes: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SyncTransport for ExpiringTransport {
        async fn login(
            &self,
            _server_origin: &str,
            credentials: &LoginCredentials,
        ) -> Result<LoginIdentity, TransportError> {
            Ok(LoginIdentity {
                user_id: "user-1".to_owned(),
                username: credentials.username.clone(),
                device_id: credentials.device_id.clone(),
                platform: credentials.platform.clone(),
                access_token: "access-old".to_owned(),
                access_expires_at: 1,
                refresh_token: "refresh-old".to_owned(),
                latest_revision: 0,
            })
        }

        async fn refresh(
            &self,
            _server_origin: &str,
            _refresh_token: &str,
        ) -> Result<RefreshedTokens, TransportError> {
            self.refreshes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            Ok(RefreshedTokens {
                access_token: "access-new".to_owned(),
                access_expires_at: 2,
                refresh_token: "refresh-new".to_owned(),
            })
        }

        async fn logout(
            &self,
            _server_origin: &str,
            access_token: &str,
        ) -> Result<(), TransportError> {
            if access_token == "access-old" {
                Err(TransportError::AccessExpired)
            } else {
                Ok(())
            }
        }

        async fn push(
            &self,
            _server_origin: &str,
            access_token: &str,
            _request: &PushRequest,
        ) -> Result<PushResponse, TransportError> {
            if access_token == "access-old" {
                Err(TransportError::AccessExpired)
            } else {
                Ok(PushResponse {
                    acknowledgements: Vec::new(),
                    latest_revision: 0,
                })
            }
        }

        async fn changes(
            &self,
            _server_origin: &str,
            _access_token: &str,
            _since: u64,
            _limit: u32,
        ) -> Result<ChangesResponse, TransportError> {
            unreachable!("not used")
        }

        async fn devices(
            &self,
            _server_origin: &str,
            _access_token: &str,
        ) -> Result<Vec<RemoteDevice>, TransportError> {
            unreachable!("not used")
        }

        async fn revoke_device(
            &self,
            _server_origin: &str,
            _access_token: &str,
            _device_id: &str,
        ) -> Result<(), TransportError> {
            unreachable!("not used")
        }

        async fn revision_stream(
            &self,
            _server_origin: &str,
            _access_token: &str,
            _events: mpsc::Sender<RevisionStreamEvent>,
            _cancellation: CancellationToken,
        ) -> Result<(), TransportError> {
            unreachable!("not used")
        }
    }

    fn login() -> LoginCredentials {
        LoginCredentials {
            username: "alice".to_owned(),
            password: "secret-password".to_owned(),
            device_id: "device-1".to_owned(),
            platform: "macos".to_owned(),
            device_name: None,
        }
    }

    #[tokio::test]
    async fn concurrent_access_expiry_rotates_refresh_token_once() {
        let transport = Arc::new(ExpiringTransport::default());
        let credentials = Arc::new(MemoryCredentials::default());
        let store: Arc<dyn CredentialStore> = credentials.clone();
        let (session, _) = AuthenticatedSession::login(
            Arc::clone(&transport),
            store,
            "https://sync.example".to_owned(),
            &login(),
        )
        .await
        .unwrap_or_else(|error| unreachable!("login fixture: {error}"));
        let session = Arc::new(session);
        let request = PushRequest { events: Vec::new() };
        let (first, second) = tokio::join!(session.push(&request), session.push(&request));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(
            transport
                .refreshes
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            credentials
                .get(&session.credential_scope())
                .unwrap_or_default(),
            Some(b"refresh-new".to_vec())
        );
    }

    #[tokio::test]
    async fn logout_removes_local_refresh_credential() {
        let transport = Arc::new(ExpiringTransport::default());
        let credentials = Arc::new(MemoryCredentials::default());
        let store: Arc<dyn CredentialStore> = credentials.clone();
        let (session, _) = AuthenticatedSession::login(
            Arc::clone(&transport),
            store,
            "https://sync.example".to_owned(),
            &login(),
        )
        .await
        .unwrap_or_else(|error| unreachable!("login fixture: {error}"));
        session
            .logout()
            .await
            .unwrap_or_else(|error| unreachable!("logout fixture: {error}"));
        assert_eq!(
            transport
                .refreshes
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            !credentials
                .contains(&session.credential_scope())
                .unwrap_or(true)
        );
    }

    #[test]
    fn secret_bearing_debug_output_is_redacted() {
        let login = login();
        assert!(!format!("{login:?}").contains("secret-password"));
        let identity = LoginIdentity {
            user_id: "u".to_owned(),
            username: "alice".to_owned(),
            device_id: "d".to_owned(),
            platform: "macos".to_owned(),
            access_token: "access-secret".to_owned(),
            access_expires_at: 1,
            refresh_token: "refresh-secret".to_owned(),
            latest_revision: 0,
        };
        let output = format!("{identity:?}");
        assert!(!output.contains("access-secret"));
        assert!(!output.contains("refresh-secret"));
    }
}
