//! Authentication and credential boundaries.

use std::{error::Error, fmt};

pub const DEFAULT_ACCESS_TOKEN_TTL_MINUTES: u64 = 60;
pub const DEFAULT_REFRESH_SESSION_IDLE_TTL_DAYS: u64 = 90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKey {
    GoogleApiKey,
    TencentSecretId,
    TencentSecretKey,
    ServerRefreshToken,
}

pub trait CredentialStore: Send + Sync {
    /// Reports whether the named credential exists.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot be read.
    fn contains(&self, key: CredentialKey) -> Result<bool, AuthError>;

    /// Replaces the named credential with the supplied secret bytes.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot persist it.
    fn set(&self, key: CredentialKey, secret: &[u8]) -> Result<(), AuthError>;

    /// Removes the named credential when present.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::CredentialStore`] when the native credential store cannot remove it.
    fn delete(&self, key: CredentialKey) -> Result<(), AuthError>;
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
