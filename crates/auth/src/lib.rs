//! Authentication and credential boundaries.

use std::{error::Error, fmt};

pub const DEFAULT_ACCESS_TOKEN_TTL_MINUTES: u64 = 60;
pub const DEFAULT_REFRESH_SESSION_IDLE_TTL_DAYS: u64 = 90;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CredentialKey {
    GoogleApiKey,
    TencentTokenHubApiKey,
    ServerRefreshToken,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CredentialScope {
    pub server_origin: String,
    pub user_id: String,
    pub device_id: String,
    pub key: CredentialKey,
}

pub trait CredentialStore: Send + Sync {
    /// Reads the named credential without exposing it through diagnostic formatting.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot be read.
    fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError>;

    /// Reports whether the named credential exists.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot be read.
    fn contains(&self, scope: &CredentialScope) -> Result<bool, AuthError>;

    /// Replaces the named credential with the supplied secret bytes.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot persist it.
    fn set(&self, scope: &CredentialScope, secret: &[u8]) -> Result<(), AuthError>;

    /// Removes the named credential when present.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot remove it.
    fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthError {
    CredentialStore,
    InvalidCredentials,
    SessionExpired,
    DeviceRevoked,
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CredentialStore => "the OS credential store operation failed",
            Self::InvalidCredentials => "the credentials are invalid",
            Self::SessionExpired => "the authentication session expired",
            Self::DeviceRevoked => "the current device is revoked",
        })
    }
}

impl Error for AuthError {}
