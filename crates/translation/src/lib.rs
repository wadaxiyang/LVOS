//! Translation Provider registry, routing, credentials, and official protocol clients.

mod credentials;
mod google;
mod http;
mod registry;
mod tokenhub;

use std::{error::Error, fmt};

use async_trait::async_trait;
use lvos_core::LanguageCode;

pub use credentials::{CredentialReader, ProviderCredentialError};
pub use google::{GOOGLE_TRANSLATE_ENDPOINT, GoogleBasicV2Provider};
pub use http::{
    HeaderValue, HttpMethod, HttpRequest, HttpResponse, HttpTransport, ReqwestTransport,
    TimeoutConfig, TransportError,
};
pub use registry::{ProviderRegistry, RouterSettings, SettingsError, TranslationRouter};
pub use tokenhub::{DEFAULT_TOKENHUB_MODEL, TOKENHUB_TRANSLATE_ENDPOINT, TencentTokenHubProvider};

pub const DEFAULT_PRIMARY_PROVIDER: &str = "tencent-tokenhub";
pub const DEFAULT_FALLBACK_PROVIDER: &str = "google-basic-v2";

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(String);

impl ProviderId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationRequest {
    pub text: String,
    pub source_language: LanguageCode,
    pub target_language: LanguageCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranslationResult {
    pub text: String,
    pub provider: ProviderId,
}

#[async_trait]
pub trait TranslationProvider: fmt::Debug + Send + Sync {
    fn id(&self) -> ProviderId;

    async fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranslationError {
    MissingConfiguration,
    Network,
    ConnectTimeout,
    RequestTimeout,
    RateLimited,
    Unauthorized,
    ProviderUnavailable,
    InvalidResponse,
    UnsupportedInput,
}

impl TranslationError {
    #[must_use]
    pub const fn permits_fallback(&self) -> bool {
        matches!(
            self,
            Self::ConnectTimeout
                | Self::RequestTimeout
                | Self::RateLimited
                | Self::ProviderUnavailable
        )
    }

    #[must_use]
    pub const fn lookup_card_kind(&self) -> LookupCardErrorKind {
        match self {
            Self::MissingConfiguration => LookupCardErrorKind::ProviderConfigurationRequired,
            Self::Unauthorized => LookupCardErrorKind::ProviderUnauthorized,
            Self::UnsupportedInput => LookupCardErrorKind::UnsupportedInput,
            Self::Network
            | Self::ConnectTimeout
            | Self::RequestTimeout
            | Self::RateLimited
            | Self::ProviderUnavailable
            | Self::InvalidResponse => LookupCardErrorKind::TranslationUnavailable,
        }
    }
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingConfiguration => "translation provider configuration is missing",
            Self::Network => "translation network failure",
            Self::ConnectTimeout => "translation connection timed out",
            Self::RequestTimeout => "translation request timed out",
            Self::RateLimited => "translation provider rate limited the request",
            Self::Unauthorized => "translation provider credentials are invalid",
            Self::ProviderUnavailable => "translation provider is temporarily unavailable",
            Self::InvalidResponse => "translation provider returned an invalid response",
            Self::UnsupportedInput => "translation provider rejected the input",
        })
    }
}

impl Error for TranslationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupCardErrorKind {
    ProviderConfigurationRequired,
    ProviderUnauthorized,
    TranslationUnavailable,
    UnsupportedInput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_transient_errors_fall_back() {
        assert!(TranslationError::ConnectTimeout.permits_fallback());
        assert!(TranslationError::RequestTimeout.permits_fallback());
        assert!(TranslationError::RateLimited.permits_fallback());
        assert!(TranslationError::ProviderUnavailable.permits_fallback());
        assert!(!TranslationError::Network.permits_fallback());
        assert!(!TranslationError::Unauthorized.permits_fallback());
        assert!(!TranslationError::MissingConfiguration.permits_fallback());
    }
}
