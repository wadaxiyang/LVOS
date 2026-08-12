//! LVOS Desktop runtime orchestration.

mod db_worker;
mod generation;
mod lifecycle;
mod lookup;
mod ui;
mod ui_bridge;
mod ui_service;
mod ui_state;

pub use db_worker::{DatabaseWorker, DatabaseWorkerError};
pub use generation::{CaptureAdmission, CaptureGate, QueryGeneration, QueryTicket};
pub use lifecycle::{
    BackgroundProfileServices, DesktopRuntime, ProfileLifecycle, RuntimeError, StartupDisposition,
    SwitchOutcome, acquire_single_instance,
};
pub use lookup::{LookupError, LookupMode, LookupOutcome, LookupService};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use ui::show_captured_provider_error;
#[cfg(target_os = "macos")]
pub use ui::show_permission_window;
pub use ui::{
    DeviceRecord, MainWindow, PermissionWindow, QuickLookupPopup, UiController, UiControllerError,
    UiRecord, ui_record,
};
pub use ui_bridge::{SlintUiDispatcher, UiDispatchError, UiDispatcher};
pub use ui_service::{UiDataError, UiDataService, UiRecordData};
pub use ui_state::{
    DeviceUiState, LookupCardState, MainSection, PopupFocusState, ProviderSelection,
    ProviderSelectionError, SettingsSection, SyncUiState,
};
