//! Validated, non-secret UI preferences shared by the Desktop hosts.

use std::{error::Error, fmt, io};

use crate::LocalPreferenceStore;

pub const DEFAULT_POPUP_IDLE_TIMEOUT_SECS: u32 = 30;
pub const MAX_POPUP_IDLE_TIMEOUT_SECS: u32 = 300;
pub const POPUP_IDLE_TIMEOUT_PRESETS: [u32; 5] = [0, 15, 30, 60, 120];
pub const POPUP_IDLE_TIMEOUT_KEY: &str = "popup_idle_timeout_secs";
pub const LAUNCH_MINIMIZED_KEY: &str = "launch-minimized";

/// Preferences that affect creation and retention of Desktop UI hosts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiPreferences {
    pub popup_idle_timeout_secs: u32,
    pub launch_minimized: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            popup_idle_timeout_secs: DEFAULT_POPUP_IDLE_TIMEOUT_SECS,
            // A fresh installation starts as a background Agent. Explicit user actions and
            // permission requirements may still create the UI.
            launch_minimized: true,
        }
    }
}

impl UiPreferences {
    /// Loads and validates UI preferences. Missing values use first-run defaults.
    ///
    /// # Errors
    /// Returns an error when a persisted timeout is malformed or outside `0..=300`.
    pub fn load(store: &LocalPreferenceStore) -> Result<Self, UiPreferenceError> {
        let defaults = Self::default();
        let popup_idle_timeout_secs = match store.load_text(POPUP_IDLE_TIMEOUT_KEY) {
            Some(value) => {
                let trimmed = value.trim();
                let parsed = trimmed.parse::<u32>().map_err(|_| {
                    UiPreferenceError::MalformedPopupIdleTimeout(trimmed.to_owned())
                })?;
                validate_popup_idle_timeout(parsed)?
            }
            None => defaults.popup_idle_timeout_secs,
        };
        let launch_minimized = match store.load_text(LAUNCH_MINIMIZED_KEY) {
            Some(value) if value.trim() == "false" => false,
            Some(value) if value.trim() == "true" => true,
            _ => defaults.launch_minimized,
        };
        Ok(Self {
            popup_idle_timeout_secs,
            launch_minimized,
        })
    }

    /// Validates and atomically persists the Popup idle timeout.
    ///
    /// # Errors
    /// Returns an error for values outside `0..=300` or when persistence fails.
    pub fn save_popup_idle_timeout(
        store: &LocalPreferenceStore,
        value: u32,
    ) -> Result<(), UiPreferenceError> {
        let value = validate_popup_idle_timeout(value)?;
        store
            .save_text(POPUP_IDLE_TIMEOUT_KEY, &value.to_string())
            .map_err(UiPreferenceError::Persistence)
    }
}

/// Returns a validated Popup retention timeout.
///
/// # Errors
/// Returns [`UiPreferenceError::InvalidPopupIdleTimeout`] above 300 seconds.
pub fn validate_popup_idle_timeout(value: u32) -> Result<u32, UiPreferenceError> {
    if value <= MAX_POPUP_IDLE_TIMEOUT_SECS {
        Ok(value)
    } else {
        Err(UiPreferenceError::InvalidPopupIdleTimeout(value))
    }
}

#[must_use]
pub fn popup_idle_timeout_from_preset_index(index: i32) -> Option<u32> {
    usize::try_from(index)
        .ok()
        .and_then(|index| POPUP_IDLE_TIMEOUT_PRESETS.get(index).copied())
}

#[must_use]
pub fn popup_idle_timeout_preset_index(value: u32) -> i32 {
    POPUP_IDLE_TIMEOUT_PRESETS
        .iter()
        .position(|preset| *preset == value)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

#[derive(Debug)]
pub enum UiPreferenceError {
    InvalidPopupIdleTimeout(u32),
    MalformedPopupIdleTimeout(String),
    Persistence(io::Error),
}

impl fmt::Display for UiPreferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPopupIdleTimeout(value) => write!(
                formatter,
                "Popup idle timeout {value} is outside the supported range 0..=300 seconds"
            ),
            Self::MalformedPopupIdleTimeout(value) => {
                write!(formatter, "Popup idle timeout `{value}` is not an integer")
            }
            Self::Persistence(error) => {
                write!(formatter, "UI preference could not be saved: {error}")
            }
        }
    }
}

impl Error for UiPreferenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Persistence(error) => Some(error),
            Self::InvalidPopupIdleTimeout(_) | Self::MalformedPopupIdleTimeout(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_POPUP_IDLE_TIMEOUT_SECS, POPUP_IDLE_TIMEOUT_KEY, POPUP_IDLE_TIMEOUT_PRESETS,
        UiPreferenceError, UiPreferences, popup_idle_timeout_from_preset_index,
        popup_idle_timeout_preset_index,
    };
    use crate::LocalPreferenceStore;

    #[test]
    fn first_run_defaults_to_background_start_and_thirty_second_retention()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let preferences = UiPreferences::load(&LocalPreferenceStore::new(root.path()))?;

        assert!(preferences.launch_minimized);
        assert_eq!(
            preferences.popup_idle_timeout_secs,
            DEFAULT_POPUP_IDLE_TIMEOUT_SECS
        );
        Ok(())
    }

    #[test]
    fn every_exposed_preset_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let store = LocalPreferenceStore::new(root.path());

        for (index, value) in (0_i32..).zip(POPUP_IDLE_TIMEOUT_PRESETS) {
            UiPreferences::save_popup_idle_timeout(&store, value)?;
            assert_eq!(UiPreferences::load(&store)?.popup_idle_timeout_secs, value);
            assert_eq!(popup_idle_timeout_preset_index(value), index);
            assert_eq!(popup_idle_timeout_from_preset_index(index), Some(value));
        }
        Ok(())
    }

    #[test]
    fn malformed_and_out_of_range_values_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let store = LocalPreferenceStore::new(root.path());
        store.save_text(POPUP_IDLE_TIMEOUT_KEY, "fast")?;
        assert!(matches!(
            UiPreferences::load(&store),
            Err(UiPreferenceError::MalformedPopupIdleTimeout(_))
        ));

        store.save_text(POPUP_IDLE_TIMEOUT_KEY, "301")?;
        assert!(matches!(
            UiPreferences::load(&store),
            Err(UiPreferenceError::InvalidPopupIdleTimeout(301))
        ));
        assert!(UiPreferences::save_popup_idle_timeout(&store, 301).is_err());
        assert_eq!(popup_idle_timeout_preset_index(45), -1);
        assert_eq!(popup_idle_timeout_from_preset_index(-1), None);
        Ok(())
    }
}
