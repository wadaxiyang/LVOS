use std::{cell::Cell, error::Error, fmt, rc::Rc};

use lvos_core::ContentKey;
use lvos_translation::{LookupCardErrorKind, ProviderId};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{
    LookupCardState, PopupFocusState, ProviderSelection, ProviderSelectionError, UiRecordData,
};

#[allow(
    missing_debug_implementations,
    unreachable_pub,
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::panic
)]
mod generated {
    slint::include_modules!();
}

pub use generated::{DeviceRecord, MainWindow, QuickLookupPopup, UiRecord};

pub struct UiController {
    popup: QuickLookupPopup,
    main_window: MainWindow,
    popup_focus: Rc<Cell<PopupFocusState>>,
}

impl fmt::Debug for UiController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiController")
            .field("popup_focus", &self.popup_focus)
            .finish_non_exhaustive()
    }
}

impl UiController {
    /// Creates the two independent Desktop surfaces on the Slint event-loop thread.
    ///
    /// # Errors
    /// Returns a platform error if either native window cannot be constructed.
    pub fn new() -> Result<Self, UiControllerError> {
        let controller = Self {
            popup: QuickLookupPopup::new().map_err(UiControllerError::Platform)?,
            main_window: MainWindow::new().map_err(UiControllerError::Platform)?,
            popup_focus: Rc::new(Cell::new(PopupFocusState::Hidden)),
        };
        let popup_weak = controller.popup.as_weak();
        let dismiss_focus = Rc::clone(&controller.popup_focus);
        controller.popup.on_dismiss_requested(move || {
            if let Some(popup) = popup_weak.upgrade()
                && let Err(error) = popup.hide()
            {
                tracing::warn!(%error, "failed to hide Lookup Card");
            }
            dismiss_focus.set(PopupFocusState::Hidden);
        });
        let interaction_focus = Rc::clone(&controller.popup_focus);
        controller.popup.on_interaction_started(move || {
            interaction_focus.set(PopupFocusState::Interactive);
        });
        let settings = controller.main_window.as_weak();
        controller.main_window.on_validate_provider_settings(
            move |primary, fallback, tokenhub_key, google_key| {
                let Some(settings) = settings.upgrade() else {
                    return "Settings window is unavailable".into();
                };
                let mut configured = Vec::new();
                if settings.get_tokenhub_configured() || !tokenhub_key.trim().is_empty() {
                    configured.push(ProviderId::new("tencent-tokenhub"));
                }
                if settings.get_google_configured() || !google_key.trim().is_empty() {
                    configured.push(ProviderId::new("google-basic-v2"));
                }
                let selection = ProviderSelection {
                    primary: provider_id(&primary),
                    fallback: (fallback.as_str() != "Disabled").then(|| provider_id(&fallback)),
                    configured,
                };
                provider_validation_copy(selection.validate()).into()
            },
        );
        Ok(controller)
    }

    #[must_use]
    pub fn popup(&self) -> &QuickLookupPopup {
        &self.popup
    }

    #[must_use]
    pub fn main_window(&self) -> &MainWindow {
        &self.main_window
    }

    #[must_use]
    pub fn popup_focus(&self) -> PopupFocusState {
        self.popup_focus.get()
    }

    /// Populates the Lookup Card without displaying Provider or sync metadata.
    pub fn apply_lookup_state(&self, state: &LookupCardState) {
        match state {
            LookupCardState::Hidden => {}
            LookupCardState::Loading { source, .. } => {
                self.popup.set_source_text(source.into());
                self.popup.set_translated_text(SharedString::default());
                self.popup.set_loading(true);
                self.popup.set_error_visible(false);
                self.popup
                    .set_text_mode(source.split_whitespace().count() > 1);
            }
            LookupCardState::Ready {
                source,
                translation,
                favorite,
                effective_query_count,
                ..
            } => {
                self.popup.set_source_text(source.into());
                self.popup.set_translated_text(translation.into());
                self.popup.set_favorite(*favorite);
                self.popup
                    .set_effective_count(saturating_i32(*effective_query_count));
                self.popup.set_loading(false);
                self.popup.set_error_visible(false);
                self.popup
                    .set_text_mode(source.split_whitespace().count() > 1);
            }
            LookupCardState::Error { source, kind, .. } => {
                let (title, detail) = error_copy(*kind);
                self.popup.set_source_text(source.into());
                self.popup.set_error_title(title.into());
                self.popup.set_error_detail(detail.into());
                self.popup.set_loading(false);
                self.popup.set_error_visible(true);
                self.popup
                    .set_text_mode(source.split_whitespace().count() > 1);
            }
        }
    }

    /// Marks the Popup visible without activation. Stage 06/07 supplies native no-activate show.
    pub fn mark_popup_visible_no_activate(&self) {
        self.popup_focus.set(PopupFocusState::VisibleNoActivate);
    }

    pub fn mark_popup_interactive(&self) {
        self.popup_focus.set(PopupFocusState::Interactive);
    }

    pub fn mark_popup_hidden(&self) {
        self.popup_focus.set(PopupFocusState::Hidden);
    }

    pub fn set_history(&self, records: Vec<UiRecord>) {
        self.main_window
            .set_history_records(ModelRc::new(VecModel::from(records)));
    }

    pub fn set_favorites(&self, records: Vec<UiRecord>) {
        self.main_window
            .set_favorite_records(ModelRc::new(VecModel::from(records)));
    }

    pub fn set_history_data(&self, records: &[UiRecordData]) {
        self.set_history(records.iter().map(ui_record_from_data).collect());
    }

    pub fn set_favorites_data(&self, records: &[UiRecordData]) {
        self.set_favorites(records.iter().map(ui_record_from_data).collect());
    }

    /// Shows the Slint Popup using normal activation semantics.
    ///
    /// Stage 06/07 replaces the platform show operation with native no-activate behavior while
    /// retaining this state and rendering path.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be shown.
    pub fn show_lookup_card(&self, state: &LookupCardState) -> Result<(), UiControllerError> {
        self.apply_lookup_state(state);
        self.popup.show().map_err(UiControllerError::Platform)?;
        self.mark_popup_visible_no_activate();
        Ok(())
    }

    /// Hides the Popup and updates its interaction lifecycle state.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be hidden.
    pub fn hide_lookup_card(&self) -> Result<(), UiControllerError> {
        self.popup.hide().map_err(UiControllerError::Platform)?;
        self.mark_popup_hidden();
        Ok(())
    }

    pub fn set_devices(&self, records: Vec<DeviceRecord>) {
        self.main_window
            .set_devices(ModelRc::new(VecModel::from(records)));
    }

    /// Shows the normal management window. Closing it must not stop background services.
    ///
    /// # Errors
    /// Returns a platform error if the native window cannot be shown.
    pub fn show_main_window(&self) -> Result<(), UiControllerError> {
        self.main_window.show().map_err(UiControllerError::Platform)
    }

    /// Hides the management window without affecting background services.
    ///
    /// # Errors
    /// Returns a platform error if the native window cannot be hidden.
    pub fn hide_main_window(&self) -> Result<(), UiControllerError> {
        self.main_window.hide().map_err(UiControllerError::Platform)
    }
}

fn ui_record_from_data(record: &UiRecordData) -> UiRecord {
    ui_record(
        record.key,
        &record.source,
        &record.translation,
        record.count,
        record.favorite,
        &record.metadata,
    )
}

fn provider_id(label: &str) -> ProviderId {
    match label {
        "Tencent TokenHub" => ProviderId::new("tencent-tokenhub"),
        "Google Basic v2" => ProviderId::new("google-basic-v2"),
        other => ProviderId::new(other),
    }
}

fn provider_validation_copy(result: Result<(), ProviderSelectionError>) -> &'static str {
    match result {
        Ok(()) => "",
        Err(ProviderSelectionError::PrimaryNotConfigured) => {
            "Configure the Primary Provider before saving."
        }
        Err(ProviderSelectionError::FallbackNotConfigured) => {
            "Configure the Fallback Provider before saving."
        }
        Err(ProviderSelectionError::DuplicateProvider) => {
            "Primary and Fallback Providers must be different."
        }
    }
}

fn saturating_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn error_copy(kind: LookupCardErrorKind) -> (&'static str, &'static str) {
    match kind {
        LookupCardErrorKind::ProviderConfigurationRequired => (
            "Translation provider not configured",
            "Open Settings and configure the selected provider.",
        ),
        LookupCardErrorKind::ProviderUnauthorized => (
            "Provider credentials rejected",
            "Check the API key in Translation Settings.",
        ),
        LookupCardErrorKind::TranslationUnavailable => {
            ("Translation unavailable", "Try again later or use Refresh.")
        }
        LookupCardErrorKind::UnsupportedInput => (
            "Unsupported text",
            "The selected provider cannot translate this input.",
        ),
    }
}

#[must_use]
pub fn ui_record(
    key: ContentKey,
    source: &str,
    translation: &str,
    count: u64,
    favorite: bool,
    metadata: &str,
) -> UiRecord {
    UiRecord {
        key: key.to_string().into(),
        source: source.into(),
        translation: translation.into(),
        count: saturating_i32(count),
        favorite,
        metadata: metadata.into(),
    }
}

#[derive(Debug)]
pub enum UiControllerError {
    Platform(slint::PlatformError),
}

impl fmt::Display for UiControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Platform(error) => write!(formatter, "Desktop UI failed: {error}"),
        }
    }
}

impl Error for UiControllerError {}

impl From<slint::PlatformError> for UiControllerError {
    fn from(value: slint::PlatformError) -> Self {
        Self::Platform(value)
    }
}
