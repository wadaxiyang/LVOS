//! Translation Provider boundary, credentials, and the official Tencent `TokenHub` client.

mod credentials;
mod http;
mod tokenhub;

use std::{error::Error, fmt};

use async_trait::async_trait;
use lvos_core::LanguageCode;

pub use credentials::{CredentialReader, ProviderCredentialError};
pub use http::{
    HeaderValue, HttpMethod, HttpRequest, HttpResponse, HttpTransport, ReqwestTransport,
    TimeoutConfig, TransportError,
};
pub use tokenhub::{
    DEFAULT_TOKENHUB_MODEL, MAX_TOKENHUB_MODEL_CHARS, TOKENHUB_TRANSLATE_ENDPOINT,
    TencentTokenHubProvider, validate_tokenhub_model,
};

pub const TOKENHUB_PROVIDER_ID: &str = "tencent-tokenhub";

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
