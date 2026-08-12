//! LVOS Desktop runtime orchestration.

mod db_worker;
mod generation;
mod lifecycle;
mod ui_bridge;

pub use db_worker::{DatabaseWorker, DatabaseWorkerError};
pub use generation::{CaptureAdmission, CaptureGate, QueryGeneration, QueryTicket};
pub use lifecycle::{
    BackgroundProfileServices, DesktopRuntime, ProfileLifecycle, RuntimeError, StartupDisposition,
    SwitchOutcome, acquire_single_instance,
};
pub use ui_bridge::{SlintUiDispatcher, UiDispatchError, UiDispatcher};
