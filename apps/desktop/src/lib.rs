//! LVOS Desktop runtime orchestration.

mod application;
#[cfg(feature = "ui")]
mod confirmation;
mod db_worker;
mod device_identity;
mod generation;
mod lifecycle;
mod local_store;
mod lookup;
mod preferences;
mod sync_engine;
mod sync_session;
mod sync_transport;
#[cfg(feature = "ui")]
mod ui;
mod ui_bridge;
mod ui_service;
mod ui_state;
mod update;

pub use application::{
    ApplicationError, DesktopApplication, NetworkPreferences, ProviderPreferences, ProxyKind,
    default_server_url,
};
#[cfg(feature = "ui")]
pub use confirmation::{ConfirmationBroker, ConfirmationRequest};
pub use db_worker::{DatabaseWorker, DatabaseWorkerError};
pub use device_identity::{DeviceIdentityError, DeviceIdentityManager};
pub use generation::{CaptureAdmission, CaptureGate, QueryGeneration, QueryTicket};
pub use lifecycle::{
    BackgroundProfileServices, DesktopRuntime, ProfileLifecycle, RuntimeError, StartupDisposition,
    SwitchOutcome, acquire_single_instance,
};
pub use local_store::{LocalPreferenceStore, atomic_write};
pub use lookup::{LookupError, LookupMode, LookupOutcome, LookupService};
pub use preferences::{
    DEFAULT_POPUP_IDLE_TIMEOUT_SECS, LAUNCH_MINIMIZED_KEY, MAX_POPUP_IDLE_TIMEOUT_SECS,
    POPUP_IDLE_TIMEOUT_KEY, POPUP_IDLE_TIMEOUT_PRESETS, UiPreferenceError, UiPreferences,
    popup_idle_timeout_from_preset_index, popup_idle_timeout_preset_index,
    validate_popup_idle_timeout,
};
pub use sync_engine::{
    SyncEngine, SyncEngineError, SyncProfileServices, SyncRunOutcome, SyncWorker, SyncWorkerHandle,
};
pub use sync_session::{AuthenticatedSession, SessionError};
pub use sync_transport::{
    FavoriteConflict, HttpSyncTransport, LoginCredentials, LoginIdentity, RefreshedTokens,
    RemoteDevice, RevisionStreamEvent, ServerCompatibility, SyncTransport, TransportError,
};
#[cfg(all(feature = "ui", target_os = "macos"))]
pub use ui::show_permission_window;
#[cfg(feature = "ui")]
pub use ui::{
    DeviceRecord, FeedbackKind, MainWindow, PermissionWindow, PopupLifecycleState,
    QuickLookupPopup, UiController, UiControllerDispatcher, UiControllerError,
    UiProcessCoordinator, UiRecord, ui_record,
};
#[cfg(all(feature = "ui", any(target_os = "macos", target_os = "windows")))]
pub use ui::{show_captured_provider_error, show_lookup_state};
#[cfg(feature = "ui")]
pub use ui_bridge::SlintUiDispatcher;
pub use ui_bridge::{UiDispatchError, UiDispatcher};
pub use ui_service::{UiDataError, UiDataService, UiRecordData};
pub use ui_state::{
    DeviceUiState, LookupCardState, MainSection, PopupFocusState, SettingsSection, SyncUiState,
};
pub use update::{
    GitHubUpdateConfig, GitHubUpdateService, HttpUpdateTransport, NativeReleasePageOpener,
    ReleasePageOpener, UpdateCheckOutcome, UpdateCoordinator, UpdateTarget, UpdateTransport,
};
