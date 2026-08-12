//! Native platform service boundaries.

use std::{error::Error, fmt, future::Future, pin::Pin, time::Duration};

#[cfg(target_os = "macos")]
pub mod macos;

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
    Busy,
    Timeout,
    Conflict,
    IntegrationFailure,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "the platform operation is unsupported",
            Self::PermissionDenied => "the platform permission was denied",
            Self::Busy => "the platform service is busy",
            Self::Timeout => "the platform operation timed out",
            Self::Conflict => "the requested platform resource is already in use",
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

    /// Installs the primary-process callback for subsequent activation requests.
    ///
    /// # Errors
    /// Returns [`PlatformError::Unsupported`] when activation signals cannot be received.
    fn set_open_handler(
        &self,
        _handler: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureError {
    Busy,
    NoSelection,
    PermissionDenied,
    Timeout,
    ClipboardUnavailable,
    InputInjectionFailed,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "another selection capture is already running",
            Self::NoSelection => "no selected text was detected",
            Self::PermissionDenied => "Accessibility or Input Monitoring permission is required",
            Self::Timeout => "selection capture timed out",
            Self::ClipboardUnavailable => "the system clipboard is unavailable",
            Self::InputInjectionFailed => "the copy shortcut could not be sent",
        })
    }
}

impl Error for CaptureError {}

pub trait SelectionCapture: Send + Sync {
    fn capture_selected_text(
        &self,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<String, CaptureError>> + Send + '_>>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalRect {
    pub origin: LogicalPoint,
    pub size: LogicalSize,
}

impl LogicalRect {
    #[must_use]
    pub fn right(self) -> f64 {
        self.origin.x + self.size.width
    }

    #[must_use]
    pub fn bottom(self) -> f64 {
        self.origin.y + self.size.height
    }

    #[must_use]
    pub fn contains(self, point: LogicalPoint) -> bool {
        point.x >= self.origin.x
            && point.x <= self.right()
            && point.y >= self.origin.y
            && point.y <= self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupPlacement {
    pub origin: LogicalPoint,
    pub scale_factor: f64,
}

/// Places a Popup near the one-shot cursor sample and keeps it in the current screen work area.
#[must_use]
pub fn place_popup(
    cursor: LogicalPoint,
    popup: LogicalSize,
    work_area: LogicalRect,
    scale_factor: f64,
) -> PopupPlacement {
    const OFFSET: f64 = 14.0;
    let desired_x = if cursor.x + OFFSET + popup.width <= work_area.right() {
        cursor.x + OFFSET
    } else {
        cursor.x - OFFSET - popup.width
    };
    let desired_y = if cursor.y + OFFSET + popup.height <= work_area.bottom() {
        cursor.y + OFFSET
    } else {
        cursor.y - OFFSET - popup.height
    };
    let max_x = (work_area.right() - popup.width).max(work_area.origin.x);
    let max_y = (work_area.bottom() - popup.height).max(work_area.origin.y);
    PopupPlacement {
        origin: LogicalPoint {
            x: desired_x.clamp(work_area.origin.x, max_x),
            y: desired_y.clamp(work_area.origin.y, max_y),
        },
        scale_factor: if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_flips_and_clamps_at_work_area_edges() {
        let work_area = LogicalRect {
            origin: LogicalPoint { x: 0.0, y: 24.0 },
            size: LogicalSize {
                width: 1_440.0,
                height: 876.0,
            },
        };
        let placement = place_popup(
            LogicalPoint {
                x: 1_430.0,
                y: 890.0,
            },
            LogicalSize {
                width: 360.0,
                height: 180.0,
            },
            work_area,
            2.0,
        );
        assert!((placement.origin.x - 1_056.0).abs() < f64::EPSILON);
        assert!((placement.origin.y - 696.0).abs() < f64::EPSILON);
        assert!((placement.scale_factor - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn popup_uses_safe_scale_and_clamps_oversized_surface() {
        let placement = place_popup(
            LogicalPoint { x: 50.0, y: 50.0 },
            LogicalSize {
                width: 800.0,
                height: 600.0,
            },
            LogicalRect {
                origin: LogicalPoint { x: 10.0, y: 20.0 },
                size: LogicalSize {
                    width: 500.0,
                    height: 400.0,
                },
            },
            f64::NAN,
        );
        assert_eq!(placement.origin, LogicalPoint { x: 10.0, y: 20.0 });
        assert!((placement.scale_factor - 1.0).abs() < f64::EPSILON);
    }
}
