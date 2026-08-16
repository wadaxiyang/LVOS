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
pub const MAX_PORTABLE_JSON_BYTES: usize = 16_777_216;
pub const MAX_PORTABLE_RECORDS: usize = 100_000;
pub const DEFAULT_UPDATE_CHANNEL: &str = "stable";
pub const GITHUB_OWNER: &str = "wadaxiyang";
pub const GITHUB_REPOSITORY: &str = "LVOS";
pub const GITHUB_RELEASES_URL: &str = "https://github.com/wadaxiyang/LVOS/releases";
pub const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/wadaxiyang/LVOS/releases/latest";
pub const GITHUB_REST_API_VERSION: &str = "2026-03-10";
pub const UPDATE_MANIFEST_VERSION: u32 = 1;
pub const UPDATE_CHECK_INTERVAL_SECONDS: i64 = 86_400;
pub const MAX_UPDATE_MANIFEST_BYTES: usize = 65_536;
pub const MAX_GITHUB_RELEASE_BYTES: usize = 1_048_576;
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 536_870_912;
pub const DEFAULT_SERVER_URL: &str = "https://lvos.niuniu770.site";
pub const DEFAULT_SERVER_PORT: u16 = 7770;
pub const DESKTOP_APP_ID: &str = "site.niuniu770.lvos";

/// A version advertised by an update source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateInfo {
    pub current_version: String,
    pub version: String,
    pub channel: String,
    pub release_page: String,
    pub available: bool,
    pub artifact: UpdateArtifact,
}

/// A validated manual-download artifact descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateArtifact {
    pub version: String,
    pub name: String,
    pub platform: String,
    pub architecture: String,
    pub download_url: String,
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

    /// Resolves and validates the release artifact descriptor for `version`.
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
    InvalidReleaseSource,
    InvalidVersion,
    UnsupportedChannel,
    UnsupportedPlatform,
    IntegrityMismatch,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Network => "the update service is unavailable",
            Self::InvalidManifest => "the update manifest is invalid",
            Self::InvalidReleaseSource => "the update metadata has an invalid release source",
            Self::InvalidVersion => "the update version is invalid",
            Self::UnsupportedChannel => "the update channel is unsupported",
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
        assert_eq!(SOFTWARE_VERSION, "0.1.3");
        assert_eq!(API_VERSION, "v1");
        assert_eq!(CONTENT_KEY_VERSION, 1);
        assert_eq!(DEFAULT_SERVER_PORT, 7770);
    }
}
