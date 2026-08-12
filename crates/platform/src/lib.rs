//! Native platform service boundaries.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    MacOs,
}

impl Platform {
    #[must_use]
    pub const fn protocol_name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    Unsupported,
    PermissionDenied,
    IntegrationFailure,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "the platform operation is unsupported",
            Self::PermissionDenied => "the platform permission was denied",
            Self::IntegrationFailure => "the native platform integration failed",
        })
    }
}

impl Error for PlatformError {}
