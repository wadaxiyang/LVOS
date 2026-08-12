use std::{error::Error, fmt};

use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use secrecy::SecretString;

#[derive(Debug)]
pub struct CredentialReader<'a, S: CredentialStore + ?Sized> {
    store: &'a S,
    server_origin: &'a str,
    user_id: &'a str,
    device_id: &'a str,
}

impl<'a, S: CredentialStore + ?Sized> CredentialReader<'a, S> {
    #[must_use]
    pub const fn new(
        store: &'a S,
        server_origin: &'a str,
        user_id: &'a str,
        device_id: &'a str,
    ) -> Self {
        Self {
            store,
            server_origin,
            user_id,
            device_id,
        }
    }

    /// Reads the user's Google Basic v2 API key from the native credential store.
    ///
    /// # Errors
    /// Returns an error when the credential is missing, malformed, or inaccessible.
    pub fn google_api_key(&self) -> Result<SecretString, ProviderCredentialError> {
        self.read(CredentialKey::GoogleApiKey)
    }

    /// Reads the user's Tencent `TokenHub` API key from the native credential store.
    ///
    /// # Errors
    /// Returns an error when the credential is missing, malformed, or inaccessible.
    pub fn tokenhub_api_key(&self) -> Result<SecretString, ProviderCredentialError> {
        self.read(CredentialKey::TencentTokenHubApiKey)
    }

    fn read(&self, key: CredentialKey) -> Result<SecretString, ProviderCredentialError> {
        let scope = CredentialScope {
            server_origin: self.server_origin.to_owned(),
            user_id: self.user_id.to_owned(),
            device_id: self.device_id.to_owned(),
            key,
        };
        let bytes = self
            .store
            .get(&scope)
            .map_err(ProviderCredentialError::Store)?
            .ok_or(ProviderCredentialError::Missing)?;
        let value = String::from_utf8(bytes).map_err(|_| ProviderCredentialError::InvalidUtf8)?;
        if value.trim().is_empty() {
            return Err(ProviderCredentialError::Missing);
        }
        Ok(SecretString::from(value))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderCredentialError {
    Missing,
    InvalidUtf8,
    Store(AuthError),
}

impl fmt::Display for ProviderCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("provider credential is missing"),
            Self::InvalidUtf8 => formatter.write_str("provider credential is not valid UTF-8"),
            Self::Store(error) => write!(formatter, "credential store failed: {error}"),
        }
    }
}

impl Error for ProviderCredentialError {}
