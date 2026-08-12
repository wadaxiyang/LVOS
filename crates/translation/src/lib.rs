//! Translation Provider contracts.

use std::{error::Error, fmt};

pub const DEFAULT_PRIMARY_PROVIDER: &str = "tencent-tmt";
pub const DEFAULT_FALLBACK_PROVIDER: &str = "google-basic-v2";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranslationError {
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
            Self::Network
                | Self::ConnectTimeout
                | Self::RequestTimeout
                | Self::RateLimited
                | Self::ProviderUnavailable
        )
    }
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_errors_never_fall_back() {
        assert!(!TranslationError::Unauthorized.permits_fallback());
        assert!(!TranslationError::InvalidResponse.permits_fallback());
        assert!(TranslationError::RequestTimeout.permits_fallback());
    }
}
