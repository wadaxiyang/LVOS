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

pub trait SingleInstanceGuard: Send {
    /// Signals the already-running process to open its Main Window.
    ///
    /// # Errors
    /// Returns [`PlatformError`] when native inter-process signaling fails.
    fn signal_existing(&self) -> Result<(), PlatformError>;
}

pub enum InstanceAcquisition {
    Primary(Box<dyn SingleInstanceGuard>),
    Existing(Box<dyn SingleInstanceGuard>),
}

impl fmt::Debug for InstanceAcquisition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Primary(_) => "InstanceAcquisition::Primary(..)",
            Self::Existing(_) => "InstanceAcquisition::Existing(..)",
        })
    }
}

pub trait SingleInstanceService: Send + Sync {
    /// Acquires the installation-wide process lock or identifies the primary process.
    ///
    /// # Errors
    /// Returns [`PlatformError`] when the native lock cannot be inspected or acquired.
    fn acquire(&self) -> Result<InstanceAcquisition, PlatformError>;
}

pub trait NotificationService: Send + Sync {
    /// Shows a native error notification without stealing focus.
    ///
    /// # Errors
    /// Returns [`PlatformError`] when notification delivery fails.
    fn error(&self, message: &str) -> Result<(), PlatformError>;

    /// Shows a native warning notification without stealing focus.
    ///
    /// # Errors
    /// Returns [`PlatformError`] when notification delivery fails.
    fn warning(&self, message: &str) -> Result<(), PlatformError>;
}
