//! Stable, platform-independent LVOS domain contracts.

mod content;
mod language;
mod time;

use std::{error::Error, fmt, future::Future, pin::Pin};

pub use content::{
    CanonicalContent, ContentKey, PreparedContent, TextKind, ValidationError, ValidationPolicy,
    prepare_content,
};
pub use language::{LanguageCode, LanguageCodeError};
pub use time::UnixTimestamp;

pub const PRODUCT_NAME: &str = "LVOS";
pub const FULL_NAME: &str = "Lightweight Vocabulary Overlay & Sync";
pub const SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const API_VERSION: &str = "v1";
pub const CONTENT_KEY_VERSION: u32 = 1;
pub const NORMALIZATION_VERSION: u32 = 1;
pub const EXPORT_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_UPDATE_CHANNEL: &str = "stable";
pub const DEFAULT_SERVER_URL: &str = "https://lvos.niuniu770.site";
pub const DEFAULT_SERVER_PORT: u16 = 7770;
pub const DESKTOP_APP_ID: &str = "site.niuniu770.lvos";

/// A version advertised by an update source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub channel: String,
    pub release_page: String,
}

/// A downloaded update artifact descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateArtifact {
    pub version: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Stable update boundary. Stage 13 supplies the GitHub Releases implementation.
pub trait UpdateService: Send + Sync {
    /// Checks the configured channel for an update.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError`] when the source is unavailable or its manifest is invalid.
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<UpdateInfo, UpdateError>> + Send + '_>>;

    /// Downloads and validates an artifact descriptor for `version`.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError`] when download, platform selection, or integrity validation fails.
    fn download(
        &self,
        version: &str,
    ) -> Pin<Box<dyn Future<Output = Result<UpdateArtifact, UpdateError>> + Send + '_>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateError {
    Network,
    InvalidManifest,
    UnsupportedPlatform,
    IntegrityMismatch,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Network => "the update service is unavailable",
            Self::InvalidManifest => "the update manifest is invalid",
            Self::UnsupportedPlatform => "no update is available for this platform",
            Self::IntegrityMismatch => "the update artifact failed integrity validation",
        })
    }
}

impl Error for UpdateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_constants_match_v1() {
        assert_eq!(SOFTWARE_VERSION, "0.1.0");
        assert_eq!(API_VERSION, "v1");
        assert_eq!(CONTENT_KEY_VERSION, 1);
        assert_eq!(DEFAULT_SERVER_PORT, 7770);
    }
}
