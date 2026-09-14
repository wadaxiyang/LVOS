use std::{
    cell::RefCell,
    collections::HashMap,
    error::Error,
    fmt,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use lvos_core::ContentKey;
use lvos_translation::LookupCardErrorKind;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{LookupCardState, PopupFocusState, UiRecordData};

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

pub use generated::{
    DeviceRecord, FeedbackKind, MainWindow, PermissionWindow, QuickLookupPopup, UiRecord,
};

#[cfg(target_os = "macos")]
thread_local! {
    static CAPTURE_POPUP_MONITOR: RefCell<Option<lvos_platform::macos::OutsideClickMonitor>> =
        const { RefCell::new(None) };
}

#[cfg(target_os = "windows")]
thread_local! {
    static WINDOWS_POPUP_MONITOR: RefCell<Option<lvos_platform::windows::OutsideClickMonitor>> =
        const { RefCell::new(None) };
}

/// Displays a captured source when runtime Provider configuration is not yet available.
///
/// # Errors
/// Returns a platform error if the native Popup cannot be shown.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn show_captured_provider_error(
    popup: &QuickLookupPopup,
    source: &str,
) -> Result<(), UiControllerError> {
    let (title, detail) = error_copy(LookupCardErrorKind::ProviderConfigurationRequired);
    popup.set_source_text(source.into());
    popup.set_error_title(title.into());
    popup.set_error_detail(detail.into());
    set_popup_dimensions(popup, source, "");
    popup.set_loading(false);
    popup.set_error_visible(true);
    popup.set_text_mode(source.split_whitespace().count() > 1);
    queue_popup_show(popup)?;
    Ok(())
}

/// Renders and displays a production Lookup Card through the native no-activate path.
///
/// # Errors
/// Returns a platform error if the native Popup cannot be shown or monitored.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn show_lookup_state(
    popup: &QuickLookupPopup,
    state: &LookupCardState,
) -> Result<(), UiControllerError> {
    apply_lookup_state_to_popup(popup, state);
    queue_popup_show(popup)?;
    Ok(())
}

/// Shows the permission surface as the active, frontmost macOS window.
///
/// Permission recovery is an explicit user interaction, so unlike the Lookup Card this window is
/// intentionally allowed to activate LVOS and take keyboard focus.
///
/// # Errors
/// Returns a platform error when the Slint or native `AppKit` window cannot be shown.
#[cfg(target_os = "macos")]
pub fn show_permission_window(permission: &PermissionWindow) -> Result<(), UiControllerError> {
    use slint::winit_030::WinitWindowAccessor;
    permission.show().map_err(UiControllerError::Platform)?;
    let weak = permission.as_weak();
    slint::spawn_local(async move {
        if let Some(permission) = weak.upgrade() {
            match permission.window().winit_window().await {
                Ok(_) if permission.window().is_visible() => {
                    if let Err(error) = macos_window::show_and_activate(permission.window()) {
                        tracing::warn!(%error, "failed to activate permission window");
                    }
                }
                Err(error) => tracing::warn!(%error, "failed to create permission window"),
                _ => {}
            }
        }
    })
    .map_err(|error| UiControllerError::Platform(slint::PlatformError::Other(error.to_string())))?;
    Ok(())
}

pub(crate) const DEFAULT_POPUP_IDLE_TIMEOUT: Duration =
    Duration::from_secs(crate::DEFAULT_POPUP_IDLE_TIMEOUT_SECS as u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PopupLifecycleState {
    Cold,
    Active,
    Warm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PopupLifecycle {
    generation: u64,
    state: PopupLifecycleState,
}

impl Default for PopupLifecycle {
    fn default() -> Self {
        Self {
            generation: 0,
            state: PopupLifecycleState::Cold,
        }
    }
}

impl PopupLifecycle {
    fn activate(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.state = PopupLifecycleState::Active;
        self.generation
    }

    fn warm(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.state = PopupLifecycleState::Warm;
        self.generation
    }

    fn release_if_current(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.state != PopupLifecycleState::Warm {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.state = PopupLifecycleState::Cold;
        true
    }
}

pub(crate) struct PopupHost {
    popup: Rc<QuickLookupPopup>,
}

pub(crate) struct MainUiHost {
    main_window: Rc<MainWindow>,
    confirmations: crate::ConfirmationBroker,
}

pub(crate) struct PermissionHost {
    permission_window: Rc<PermissionWindow>,
}

type MainCreatedHook = Rc<dyn Fn(Rc<MainWindow>, crate::ConfirmationBroker)>;
type PopupCreatedHook = Rc<dyn Fn(Rc<QuickLookupPopup>)>;
type PermissionCreatedHook = Rc<dyn Fn(Rc<PermissionWindow>)>;
type PopupDismissedHook = Rc<dyn Fn()>;
type HostDestroyedHook = Rc<dyn Fn()>;

struct UiHosts {
    popup: Option<PopupHost>,
    main: Option<MainUiHost>,
    permission: Option<PermissionHost>,
    popup_lifecycle: PopupLifecycle,
    popup_idle_timeout: Duration,
    main_created_hooks: Vec<MainCreatedHook>,
    popup_created_hooks: Vec<PopupCreatedHook>,
    permission_created_hooks: Vec<PermissionCreatedHook>,
    popup_dismissed_hooks: Vec<PopupDismissedHook>,
    main_destroyed_hooks: Vec<HostDestroyedHook>,
    popup_destroyed_hooks: Vec<HostDestroyedHook>,
    permission_destroyed_hooks: Vec<HostDestroyedHook>,
}

impl UiHosts {
    fn new() -> Self {
        Self {
            popup: None,
            main: None,
            permission: None,
            popup_lifecycle: PopupLifecycle::default(),
            popup_idle_timeout: DEFAULT_POPUP_IDLE_TIMEOUT,
            main_created_hooks: Vec::new(),
            popup_created_hooks: Vec::new(),
            permission_created_hooks: Vec::new(),
            popup_dismissed_hooks: Vec::new(),
            main_destroyed_hooks: Vec::new(),
            popup_destroyed_hooks: Vec::new(),
            permission_destroyed_hooks: Vec::new(),
        }
    }
}

impl Drop for UiHosts {
    fn drop(&mut self) {
        if let Some(main) = self.main.take() {
            main.confirmations.invalidate();
            let _ = main.main_window.hide();
        }
        if let Some(popup) = self.popup.take() {
            popup.popup.set_native_show_requested(false);
            popup.popup.set_native_interactive(false);
            let _ = popup.popup.hide();
        }
        if let Some(permission) = self.permission.take() {
            let _ = permission.permission_window.hide();
        }
        #[cfg(target_os = "windows")]
        let _ = WINDOWS_POPUP_MONITOR.try_with(|monitor| monitor.borrow_mut().take());
        #[cfg(target_os = "macos")]
        let _ = CAPTURE_POPUP_MONITOR.try_with(|monitor| monitor.borrow_mut().take());
    }
}

impl fmt::Debug for UiHosts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiHosts")
            .field("popup", &self.popup.is_some())
            .field("main", &self.main.is_some())
            .field("permission", &self.permission.is_some())
            .field("popup_lifecycle", &self.popup_lifecycle)
            .finish_non_exhaustive()
    }
}

/// Owns independently-created UI hosts on the Slint event-loop thread.
#[derive(Clone)]
pub struct UiProcessCoordinator {
    id: u64,
    hosts: Rc<RefCell<UiHosts>>,
}

/// Compatibility name retained while callers migrate to `UiProcessCoordinator`.
pub type UiController = UiProcessCoordinator;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiControllerDispatcher {
    id: u64,
}

static NEXT_UI_CONTROLLER_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static UI_CONTROLLERS: RefCell<HashMap<u64, Weak<RefCell<UiHosts>>>> = RefCell::new(HashMap::new());
}

impl fmt::Debug for UiProcessCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiProcessCoordinator")
            .field("hosts", &self.hosts.borrow())
            .finish_non_exhaustive()
    }
}

impl Drop for UiProcessCoordinator {
    fn drop(&mut self) {
        if Rc::strong_count(&self.hosts) == 1 {
            let id = self.id;
            let _ = UI_CONTROLLERS.try_with(|controllers| {
                controllers.borrow_mut().remove(&id);
            });
        }
    }
}

thread_local! {
    static DESKTOP_BACKEND_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn initialize_desktop_backend() -> Result<(), UiControllerError> {
    DESKTOP_BACKEND_READY.with(|ready| {
        if ready.get() {
            return Ok(());
        }
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("femtovg".into())
            .with_winit_window_attributes_hook(|attributes| attributes.with_active(false))
            .select()
            .map_err(UiControllerError::Platform)?;
        ready.set(true);
        Ok(())
    })
}

impl UiProcessCoordinator {
    /// Initializes the GUI backend without constructing any Slint window.
    ///
    /// # Errors
    /// Returns a platform error if the selected Slint backend cannot be initialized.
    pub fn new() -> Result<Self, UiControllerError> {
        initialize_desktop_backend()?;
        let controller = Self {
            id: NEXT_UI_CONTROLLER_ID.fetch_add(1, Ordering::Relaxed),
            hosts: Rc::new(RefCell::new(UiHosts::new())),
        };
        UI_CONTROLLERS.with(|controllers| {
            controllers
                .borrow_mut()
                .insert(controller.id, Rc::downgrade(&controller.hosts));
        });
        Ok(controller)
    }

    #[must_use]
    pub const fn dispatcher(&self) -> UiControllerDispatcher {
        UiControllerDispatcher { id: self.id }
    }

    pub fn on_main_created(
        &self,
        hook: impl Fn(Rc<MainWindow>, crate::ConfirmationBroker) + 'static,
    ) {
        let hook: MainCreatedHook = Rc::new(hook);
        let existing = {
            let mut hosts = self.hosts.borrow_mut();
            hosts.main_created_hooks.push(Rc::clone(&hook));
            hosts
                .main
                .as_ref()
                .map(|host| (host.main_window.clone(), host.confirmations.clone()))
        };
        if let Some((main, confirmations)) = existing {
            hook(main, confirmations);
        }
    }

    pub fn on_popup_created(&self, hook: impl Fn(Rc<QuickLookupPopup>) + 'static) {
        let hook: PopupCreatedHook = Rc::new(hook);
        let existing = {
            let mut hosts = self.hosts.borrow_mut();
            hosts.popup_created_hooks.push(Rc::clone(&hook));
            hosts.popup.as_ref().map(|host| host.popup.clone())
        };
        if let Some(popup) = existing {
            hook(popup);
        }
    }

    pub fn on_permission_created(&self, hook: impl Fn(Rc<PermissionWindow>) + 'static) {
        let hook: PermissionCreatedHook = Rc::new(hook);
        let existing = {
            let mut hosts = self.hosts.borrow_mut();
            hosts.permission_created_hooks.push(Rc::clone(&hook));
            hosts
                .permission
                .as_ref()
                .map(|host| host.permission_window.clone())
        };
        if let Some(permission) = existing {
            hook(permission);
        }
    }

    pub fn on_popup_dismissed(&self, hook: impl Fn() + 'static) {
        self.hosts
            .borrow_mut()
            .popup_dismissed_hooks
            .push(Rc::new(hook));
    }

    pub fn on_main_destroyed(&self, hook: impl Fn() + 'static) {
        self.hosts
            .borrow_mut()
            .main_destroyed_hooks
            .push(Rc::new(hook));
    }

    pub fn on_popup_destroyed(&self, hook: impl Fn() + 'static) {
        self.hosts
            .borrow_mut()
            .popup_destroyed_hooks
            .push(Rc::new(hook));
    }

    pub fn on_permission_destroyed(&self, hook: impl Fn() + 'static) {
        self.hosts
            .borrow_mut()
            .permission_destroyed_hooks
            .push(Rc::new(hook));
    }

    fn ensure_main(&self) -> Result<Rc<MainWindow>, UiControllerError> {
        if let Some(main) = self
            .hosts
            .borrow()
            .main
            .as_ref()
            .map(|host| host.main_window.clone())
        {
            return Ok(main);
        }
        let main = Rc::new(MainWindow::new().map_err(UiControllerError::Platform)?);
        let confirmations = crate::ConfirmationBroker::install(&main);
        install_provider_validation(&main);
        let weak_hosts = Rc::downgrade(&self.hosts);
        main.window().on_close_requested(move || {
            let weak_hosts = weak_hosts.clone();
            slint::Timer::single_shot(Duration::ZERO, move || destroy_main(&weak_hosts));
            slint::CloseRequestResponse::HideWindow
        });
        let hooks = {
            let mut hosts = self.hosts.borrow_mut();
            if let Some(existing) = hosts.main.as_ref() {
                return Ok(existing.main_window.clone());
            }
            hosts.main = Some(MainUiHost {
                main_window: main.clone(),
                confirmations: confirmations.clone(),
            });
            hosts.main_created_hooks.clone()
        };
        tracing::debug!(event = "main_ui_created");
        for hook in hooks {
            hook(main.clone(), confirmations.clone());
        }
        Ok(main)
    }

    fn ensure_popup(&self) -> Result<Rc<QuickLookupPopup>, UiControllerError> {
        if let Some(popup) = self
            .hosts
            .borrow()
            .popup
            .as_ref()
            .map(|host| host.popup.clone())
        {
            return Ok(popup);
        }
        let popup = Rc::new(QuickLookupPopup::new().map_err(UiControllerError::Platform)?);
        let weak_hosts = Rc::downgrade(&self.hosts);
        popup.on_dismiss_requested(move || dismiss_popup_hosts(&weak_hosts));
        let interaction_popup = popup.as_weak();
        popup.on_interaction_started(move || {
            if let Some(popup) = interaction_popup.upgrade() {
                popup.set_native_interactive(true);
            }
        });
        let hooks = {
            let mut hosts = self.hosts.borrow_mut();
            if let Some(existing) = hosts.popup.as_ref() {
                return Ok(existing.popup.clone());
            }
            hosts.popup = Some(PopupHost {
                popup: popup.clone(),
            });
            hosts.popup_created_hooks.clone()
        };
        tracing::debug!(event = "popup_created");
        for hook in hooks {
            hook(popup.clone());
        }
        Ok(popup)
    }

    fn ensure_permission(&self) -> Result<Rc<PermissionWindow>, UiControllerError> {
        if let Some(permission) = self
            .hosts
            .borrow()
            .permission
            .as_ref()
            .map(|host| host.permission_window.clone())
        {
            return Ok(permission);
        }
        let permission = Rc::new(PermissionWindow::new().map_err(UiControllerError::Platform)?);
        let weak_hosts = Rc::downgrade(&self.hosts);
        permission.window().on_close_requested(move || {
            let weak_hosts = weak_hosts.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(hosts) = weak_hosts.upgrade() {
                    let _ = destroy_permission_controller(&hosts);
                }
            });
            slint::CloseRequestResponse::HideWindow
        });
        let hooks = {
            let mut hosts = self.hosts.borrow_mut();
            if let Some(existing) = hosts.permission.as_ref() {
                return Ok(existing.permission_window.clone());
            }
            hosts.permission = Some(PermissionHost {
                permission_window: permission.clone(),
            });
            hosts.permission_created_hooks.clone()
        };
        tracing::debug!(event = "permission_ui_created");
        for hook in hooks {
            hook(permission.clone());
        }
        Ok(permission)
    }

    #[must_use]
    pub fn confirmations(&self) -> crate::ConfirmationBroker {
        if self.ensure_main().is_err() {
            fatal_ui_construction("MainWindow");
        }
        self.hosts.borrow().main.as_ref().map_or_else(
            || fatal_ui_construction("MainWindow"),
            |host| host.confirmations.clone(),
        )
    }

    #[must_use]
    pub fn popup(&self) -> Rc<QuickLookupPopup> {
        let popup = self
            .ensure_popup()
            .unwrap_or_else(|_| fatal_ui_construction("QuickLookupPopup"));
        let warm_timer = {
            let mut hosts = self.hosts.borrow_mut();
            (hosts.popup_lifecycle.state == PopupLifecycleState::Cold)
                .then(|| (hosts.popup_lifecycle.warm(), hosts.popup_idle_timeout))
        };
        if let Some((generation, timeout)) = warm_timer {
            schedule_popup_release(&self.hosts, generation, timeout);
        }
        popup
    }

    #[must_use]
    pub fn main_window(&self) -> Rc<MainWindow> {
        self.ensure_main()
            .unwrap_or_else(|_| fatal_ui_construction("MainWindow"))
    }

    #[must_use]
    pub fn permission_window(&self) -> Rc<PermissionWindow> {
        self.ensure_permission()
            .unwrap_or_else(|_| fatal_ui_construction("PermissionWindow"))
    }

    #[must_use]
    pub fn popup_focus(&self) -> PopupFocusState {
        let Some(popup) = self
            .hosts
            .borrow()
            .popup
            .as_ref()
            .map(|host| host.popup.clone())
        else {
            return PopupFocusState::Hidden;
        };
        if !popup.get_native_show_requested() {
            PopupFocusState::Hidden
        } else if popup.get_native_interactive() {
            PopupFocusState::Interactive
        } else {
            PopupFocusState::VisibleNoActivate
        }
    }

    /// Populates the Lookup Card without displaying Provider or sync metadata.
    pub fn apply_lookup_state(&self, state: &LookupCardState) {
        if let Ok(popup) = self.ensure_popup() {
            apply_lookup_state_to_popup(&popup, state);
        }
    }

    /// Marks the Popup visible without activation. Stage 06/07 supplies native no-activate show.
    pub fn mark_popup_visible_no_activate(&self) {
        if let Ok(popup) = self.ensure_popup() {
            popup.set_native_show_requested(true);
            popup.set_native_interactive(false);
        }
    }

    pub fn mark_popup_interactive(&self) {
        if let Some(popup) = self.hosts.borrow().popup.as_ref() {
            popup.popup.set_native_interactive(true);
        }
    }

    pub fn mark_popup_hidden(&self) {
        if let Some(popup) = self.hosts.borrow().popup.as_ref() {
            popup.popup.set_native_show_requested(false);
            popup.popup.set_native_interactive(false);
        }
    }

    pub fn set_history(&self, records: Vec<UiRecord>) {
        if let Some(main) = self.hosts.borrow().main.as_ref() {
            main.main_window
                .set_history_records(ModelRc::new(VecModel::from(records)));
        }
    }

    pub fn set_favorites(&self, records: Vec<UiRecord>) {
        if let Some(main) = self.hosts.borrow().main.as_ref() {
            main.main_window
                .set_favorite_records(ModelRc::new(VecModel::from(records)));
        }
    }

    pub fn set_history_data(&self, records: &[UiRecordData]) {
        self.set_history(records.iter().map(ui_record_from_data).collect());
    }

    pub fn set_favorites_data(&self, records: &[UiRecordData]) {
        self.set_favorites(records.iter().map(ui_record_from_data).collect());
    }

    /// Queues native creation and no-activate presentation of the latest Lookup Card.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be shown.
    pub fn show_lookup_card(&self, state: &LookupCardState) -> Result<(), UiControllerError> {
        let popup = self.ensure_popup()?;
        let reused = {
            let mut hosts = self.hosts.borrow_mut();
            let reused = hosts.popup_lifecycle.state == PopupLifecycleState::Warm;
            hosts.popup_lifecycle.activate();
            reused
        };
        if reused {
            tracing::debug!(event = "popup_warm_reused");
        }
        apply_lookup_state_to_popup(&popup, state);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        queue_popup_show(&popup)?;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        popup.show().map_err(UiControllerError::Platform)?;
        tracing::debug!(event = "popup_shown");
        Ok(())
    }

    /// Replaces content only while an existing Popup is Active.
    ///
    /// This path never creates, re-shows, or restarts a GUI host, so a late business result cannot
    /// resurrect a Popup the user already dismissed.
    #[must_use]
    pub fn update_lookup_card_if_active(&self, state: &LookupCardState) -> bool {
        let popup = {
            let hosts = self.hosts.borrow();
            if hosts.popup_lifecycle.state != PopupLifecycleState::Active {
                return false;
            }
            hosts.popup.as_ref().map(|host| host.popup.clone())
        };
        let Some(popup) = popup else {
            return false;
        };
        apply_lookup_state_to_popup(&popup, state);
        tracing::debug!(event = "popup_updated_without_presentation");
        true
    }

    /// Presents the Provider-configuration error for a captured source.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be shown.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn show_captured_provider_error(&self, source: &str) -> Result<(), UiControllerError> {
        let popup = self.ensure_popup()?;
        self.hosts.borrow_mut().popup_lifecycle.activate();
        show_captured_provider_error(&popup, source)?;
        tracing::debug!(event = "popup_shown");
        Ok(())
    }

    /// Hides the Popup and updates its interaction lifecycle state.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be hidden.
    pub fn hide_lookup_card(&self) -> Result<(), UiControllerError> {
        dismiss_popup_controller(&self.hosts)?;
        Ok(())
    }

    pub fn set_devices(&self, records: Vec<DeviceRecord>) {
        if let Some(main) = self.hosts.borrow().main.as_ref() {
            main.main_window
                .set_devices(ModelRc::new(VecModel::from(records)));
        }
    }

    /// Shows the normal management window. Closing it must not stop background services.
    ///
    /// # Errors
    /// Returns a platform error if the native window cannot be shown.
    pub fn show_main_window(&self) -> Result<(), UiControllerError> {
        use slint::winit_030::WinitWindowAccessor;
        let main_window = self.ensure_main()?;
        main_window.show().map_err(UiControllerError::Platform)?;
        tracing::debug!(event = "main_ui_shown");
        let weak = main_window.as_weak();
        slint::spawn_local(async move {
            if let Some(main) = weak.upgrade() {
                match main.window().winit_window().await {
                    Ok(native) if main.window().is_visible() => native.focus_window(),
                    Err(error) => tracing::warn!(%error, "failed to activate management window"),
                    _ => {}
                }
            }
        })
        .map_err(|error| {
            UiControllerError::Platform(slint::PlatformError::Other(error.to_string()))
        })?;
        Ok(())
    }

    /// Hides the management window without affecting background services.
    ///
    /// # Errors
    /// Returns a platform error if the native window cannot be hidden.
    pub fn hide_main_window(&self) -> Result<(), UiControllerError> {
        destroy_main_controller(&self.hosts)
    }

    /// Creates and presents the permission host on demand.
    ///
    /// # Errors
    /// Returns a platform error if the permission window cannot be shown.
    pub fn show_permission_window(&self) -> Result<(), UiControllerError> {
        let permission = self.ensure_permission()?;
        #[cfg(target_os = "macos")]
        show_permission_window(&permission)?;
        #[cfg(not(target_os = "macos"))]
        permission.show().map_err(UiControllerError::Platform)?;
        Ok(())
    }

    /// Hides and releases the permission host.
    ///
    /// # Errors
    /// Returns a platform error if the window cannot be hidden.
    pub fn hide_permission_window(&self) -> Result<(), UiControllerError> {
        destroy_permission_controller(&self.hosts)
    }

    /// Applies a validated Popup retention timeout to the current lifecycle.
    ///
    /// A Warm Popup receives a new lifecycle generation and timer. Zero releases it immediately;
    /// Active and Cold Popups only use the new value on their next dismissal.
    ///
    /// # Errors
    /// Returns an error when `timeout_secs` is above the supported maximum.
    pub fn set_popup_idle_timeout_secs(
        &self,
        timeout_secs: u32,
    ) -> Result<(), crate::UiPreferenceError> {
        let timeout_secs = crate::validate_popup_idle_timeout(timeout_secs)?;
        let timeout = Duration::from_secs(u64::from(timeout_secs));
        let warm_generation = {
            let mut hosts = self.hosts.borrow_mut();
            hosts.popup_idle_timeout = timeout;
            (hosts.popup_lifecycle.state == PopupLifecycleState::Warm)
                .then(|| hosts.popup_lifecycle.warm())
        };
        if let Some(generation) = warm_generation {
            tracing::debug!(event = "popup_warm_reconfigured", generation, timeout_secs);
            schedule_popup_release(&self.hosts, generation, timeout);
        }
        Ok(())
    }

    #[must_use]
    pub fn popup_idle_timeout_secs(&self) -> u32 {
        u32::try_from(self.hosts.borrow().popup_idle_timeout.as_secs())
            .unwrap_or(crate::MAX_POPUP_IDLE_TIMEOUT_SECS)
    }

    #[must_use]
    pub fn popup_lifecycle_state(&self) -> PopupLifecycleState {
        self.hosts.borrow().popup_lifecycle.state
    }

    #[must_use]
    pub fn has_main_host(&self) -> bool {
        self.hosts.borrow().main.is_some()
    }

    #[must_use]
    pub fn has_popup_host(&self) -> bool {
        self.hosts.borrow().popup.is_some()
    }

    #[must_use]
    pub fn has_permission_host(&self) -> bool {
        self.hosts.borrow().permission.is_some()
    }

    #[must_use]
    pub fn has_live_ui(&self) -> bool {
        let hosts = self.hosts.borrow();
        hosts.main.is_some() || hosts.popup.is_some() || hosts.permission.is_some()
    }
}

impl UiControllerDispatcher {
    /// Resolves the coordinator on its event-loop thread without constructing a Host.
    pub fn with_local<R>(self, callback: impl FnOnce(&UiProcessCoordinator) -> R) -> Option<R> {
        with_controller(self.id, |controller| callback(&controller))
    }

    /// Queues a Lookup Card update without requiring a Popup to exist beforehand.
    ///
    /// # Errors
    /// Returns an error after the UI event loop or coordinator has stopped.
    pub fn show_lookup(self, state: LookupCardState) -> Result<(), UiControllerError> {
        slint::invoke_from_event_loop(move || {
            with_controller(self.id, |controller| {
                if let Err(error) = controller.show_lookup_card(&state) {
                    tracing::warn!(%error, "failed to show Lookup Card");
                }
            });
        })
        .map_err(|error| {
            UiControllerError::Platform(slint::PlatformError::Other(error.to_string()))
        })
    }

    /// Queues creation and presentation of the management window.
    ///
    /// # Errors
    /// Returns an error after the UI event loop or coordinator has stopped.
    pub fn show_main(self) -> Result<(), UiControllerError> {
        slint::invoke_from_event_loop(move || {
            with_controller(self.id, |controller| {
                if let Err(error) = controller.show_main_window() {
                    tracing::warn!(%error, "failed to show management window");
                }
            });
        })
        .map_err(|error| {
            UiControllerError::Platform(slint::PlatformError::Other(error.to_string()))
        })
    }

    /// Queues creation and presentation of the permission window.
    ///
    /// # Errors
    /// Returns an error after the UI event loop or coordinator has stopped.
    pub fn show_permission(self, status: Option<String>) -> Result<(), UiControllerError> {
        slint::invoke_from_event_loop(move || {
            with_controller(self.id, |controller| {
                if let Some(status) = status
                    && let Ok(permission) = controller.ensure_permission()
                {
                    permission.set_status_text(status.into());
                }
                if let Err(error) = controller.show_permission_window() {
                    tracing::warn!(%error, "failed to show permission window");
                }
            });
        })
        .map_err(|error| {
            UiControllerError::Platform(slint::PlatformError::Other(error.to_string()))
        })
    }

    /// Queues destruction of the permission host.
    ///
    /// # Errors
    /// Returns an error after the UI event loop or coordinator has stopped.
    pub fn hide_permission(self) -> Result<(), UiControllerError> {
        slint::invoke_from_event_loop(move || {
            with_controller(self.id, |controller| {
                if let Err(error) = controller.hide_permission_window() {
                    tracing::warn!(%error, "failed to destroy permission window");
                }
            });
        })
        .map_err(|error| {
            UiControllerError::Platform(slint::PlatformError::Other(error.to_string()))
        })
    }
}

fn with_controller<R>(id: u64, callback: impl FnOnce(UiProcessCoordinator) -> R) -> Option<R> {
    let hosts = UI_CONTROLLERS.with(|controllers| {
        let mut controllers = controllers.borrow_mut();
        let hosts = controllers.get(&id).and_then(Weak::upgrade);
        if hosts.is_none() {
            controllers.remove(&id);
        }
        hosts
    });
    hosts.map(|hosts| callback(UiProcessCoordinator { id, hosts }))
}

fn fatal_ui_construction(component: &str) -> ! {
    tracing::error!(component, "fatal UI component construction failure");
    std::process::abort();
}

fn install_provider_validation(main: &MainWindow) {
    let settings = main.as_weak();
    main.on_validate_provider_settings(move |tokenhub_model, tokenhub_key| {
        if lvos_translation::validate_tokenhub_model(&tokenhub_model).is_err() {
            return "Tencent TokenHub model must be 1-128 characters without whitespace or control characters".into();
        }
        let Some(settings) = settings.upgrade() else {
            return "Settings window is unavailable".into();
        };
        if !settings.get_tokenhub_configured() && tokenhub_key.trim().is_empty() {
            return "Configure Tencent TokenHub before saving.".into();
        }
        "".into()
    });
}

fn destroy_main(weak_hosts: &Weak<RefCell<UiHosts>>) {
    if let Some(hosts) = weak_hosts.upgrade()
        && let Err(error) = destroy_main_controller(&hosts)
    {
        tracing::warn!(%error, "failed to destroy management window");
    }
}

fn destroy_main_controller(hosts: &Rc<RefCell<UiHosts>>) -> Result<(), UiControllerError> {
    let (host, hooks) = {
        let mut hosts = hosts.borrow_mut();
        (hosts.main.take(), hosts.main_destroyed_hooks.clone())
    };
    let Some(host) = host else {
        return Ok(());
    };
    host.confirmations.invalidate();
    host.main_window
        .hide()
        .map_err(UiControllerError::Platform)?;
    tracing::debug!(event = "main_ui_destroyed");
    for hook in hooks {
        hook();
    }
    Ok(())
}

fn destroy_permission_controller(hosts: &Rc<RefCell<UiHosts>>) -> Result<(), UiControllerError> {
    let (host, hooks) = {
        let mut hosts = hosts.borrow_mut();
        (
            hosts.permission.take(),
            hosts.permission_destroyed_hooks.clone(),
        )
    };
    let Some(host) = host else {
        return Ok(());
    };
    host.permission_window
        .hide()
        .map_err(UiControllerError::Platform)?;
    tracing::debug!(event = "permission_ui_destroyed");
    for hook in hooks {
        hook();
    }
    Ok(())
}

fn dismiss_popup_hosts(weak_hosts: &Weak<RefCell<UiHosts>>) {
    if let Some(hosts) = weak_hosts.upgrade()
        && let Err(error) = dismiss_popup_controller(&hosts)
    {
        tracing::warn!(%error, "failed to dismiss Lookup Card");
    }
}

fn dismiss_popup_controller(hosts: &Rc<RefCell<UiHosts>>) -> Result<(), UiControllerError> {
    let (popup, generation, timeout, hooks) = {
        let mut hosts = hosts.borrow_mut();
        if hosts.popup_lifecycle.state != PopupLifecycleState::Active {
            return Ok(());
        }
        let Some(popup) = hosts.popup.as_ref().map(|host| host.popup.clone()) else {
            return Ok(());
        };
        dismiss_popup(&popup)?;
        let generation = hosts.popup_lifecycle.warm();
        (
            popup,
            generation,
            hosts.popup_idle_timeout,
            hosts.popup_dismissed_hooks.clone(),
        )
    };
    drop(popup);
    for hook in hooks {
        hook();
    }
    tracing::debug!(
        event = "popup_hidden",
        generation,
        configured_timeout_secs = timeout.as_secs()
    );
    tracing::debug!(
        event = "popup_warm_started",
        generation,
        timeout_secs = timeout.as_secs()
    );
    schedule_popup_release(hosts, generation, timeout);
    Ok(())
}

fn schedule_popup_release(hosts: &Rc<RefCell<UiHosts>>, generation: u64, timeout: Duration) {
    if timeout.is_zero() {
        release_popup(hosts, generation, timeout, "configured_zero");
        return;
    }
    let weak_hosts = Rc::downgrade(hosts);
    slint::Timer::single_shot(timeout, move || {
        if let Some(hosts) = weak_hosts.upgrade() {
            release_popup(&hosts, generation, timeout, "idle_timeout");
        }
    });
}

fn release_popup(
    hosts: &Rc<RefCell<UiHosts>>,
    generation: u64,
    timeout: Duration,
    reason: &'static str,
) {
    let (released, hooks) = {
        let mut hosts = hosts.borrow_mut();
        if !hosts.popup_lifecycle.release_if_current(generation) {
            return;
        }
        (
            hosts.popup.take().is_some(),
            hosts.popup_destroyed_hooks.clone(),
        )
    };
    if released {
        if reason == "idle_timeout" {
            tracing::debug!(
                event = "popup_idle_timeout",
                generation,
                configured_timeout_secs = timeout.as_secs(),
                reason
            );
        }
        #[cfg(target_os = "windows")]
        WINDOWS_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
        #[cfg(target_os = "macos")]
        CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
        tracing::debug!(
            event = "popup_destroyed",
            generation,
            configured_timeout_secs = timeout.as_secs(),
            reason
        );
        for hook in hooks {
            hook();
        }
    }
}

fn dismiss_popup(popup: &QuickLookupPopup) -> Result<(), UiControllerError> {
    popup.set_native_show_requested(false);
    popup.set_native_interactive(false);
    popup.set_native_request_id(popup.get_native_request_id().wrapping_add(1));
    #[cfg(target_os = "windows")]
    WINDOWS_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
    #[cfg(target_os = "macos")]
    CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
    popup.hide().map_err(UiControllerError::Platform)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn queue_popup_show(popup: &QuickLookupPopup) -> Result<(), UiControllerError> {
    use slint::winit_030::{
        EventResult, WinitWindowAccessor,
        winit::event::{ElementState, WindowEvent},
    };
    let request = popup.get_native_request_id().wrapping_add(1);
    popup.set_native_request_id(request);
    popup.set_native_show_requested(true);
    let weak = popup.as_weak();
    slint::spawn_local(async move {
        let Some(popup) = weak.upgrade() else {
            return;
        };
        // winit creates its native window hidden once its event loop is active.
        // Await it before touching HWND/NSWindow; never show once just to obtain a handle.
        if let Err(error) = popup.window().winit_window().await {
            tracing::warn!(%error, "failed to create native Lookup Card");
            popup.invoke_dismiss_requested();
            return;
        }
        if popup.get_native_request_id() != request || !popup.get_native_show_requested() {
            return;
        }
        let interaction = popup.as_weak();
        popup.window().on_winit_window_event(move |_, event| {
            if let Some(popup) = interaction.upgrade() {
                match event {
                    WindowEvent::KeyboardInput { event, .. }
                        if event.state == ElementState::Pressed
                            && event.logical_key
                                == slint::winit_030::winit::keyboard::Key::Named(
                                    slint::winit_030::winit::keyboard::NamedKey::Escape,
                                ) =>
                    {
                        popup.invoke_dismiss_requested();
                        return EventResult::PreventDefault;
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                    | WindowEvent::Focused(true) => {
                        if popup.get_native_show_requested() {
                            popup.invoke_interaction_started();
                        }
                    }
                    WindowEvent::CloseRequested => {
                        popup.invoke_dismiss_requested();
                    }
                    _ => {}
                }
            }
            EventResult::Propagate
        });
        let result = show_prepared_popup(&popup);
        if let Err(error) = result {
            tracing::warn!(%error, "failed to present native Lookup Card");
            popup.invoke_dismiss_requested();
        }
    })
    .map_err(|error| UiControllerError::Platform(slint::PlatformError::Other(error.to_string())))?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn show_prepared_popup(popup: &QuickLookupPopup) -> Result<(), UiControllerError> {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
        if !popup.window().is_visible() {
            windows_window::prepare_no_activate(popup.window())?;
        }
        popup.show().map_err(UiControllerError::Platform)?;
        windows_window::configure_visible_popup(popup.window())?;
        let monitor = windows_window::install_outside_click_monitor(popup)?;
        WINDOWS_POPUP_MONITOR.with(|active| active.borrow_mut().replace(monitor));
    }
    #[cfg(target_os = "macos")]
    {
        CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
        popup.show().map_err(UiControllerError::Platform)?;
        let bounds = macos_window::show_without_activation_and_place(popup.window())?;
        let weak = popup.as_weak();
        let monitor = lvos_platform::macos::OutsideClickMonitor::install(
            bounds,
            std::sync::Arc::new(move || {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(popup) = weak.upgrade() {
                        popup.invoke_dismiss_requested();
                    }
                });
            }),
        )
        .map_err(|_| macos_window::platform_error("outside-click monitor is unavailable"))?;
        CAPTURE_POPUP_MONITOR.with(|active| active.borrow_mut().replace(monitor));
    }
    Ok(())
}

fn apply_lookup_state_to_popup(popup: &QuickLookupPopup, state: &LookupCardState) {
    match state {
        LookupCardState::Hidden => {}
        LookupCardState::Loading { source, .. } => {
            popup.set_source_text(source.into());
            popup.set_translated_text(SharedString::default());
            set_popup_dimensions(popup, source, "");
            popup.set_loading(true);
            popup.set_error_visible(false);
            popup.set_text_mode(source.split_whitespace().count() > 1);
        }
        LookupCardState::Ready {
            source,
            translation,
            favorite,
            effective_query_count,
            ..
        } => {
            popup.set_source_text(source.into());
            popup.set_translated_text(translation.into());
            set_popup_dimensions(popup, source, translation);
            popup.set_favorite(*favorite);
            popup.set_effective_count(saturating_i32(*effective_query_count));
            popup.set_loading(false);
            popup.set_error_visible(false);
            popup.set_text_mode(source.split_whitespace().count() > 1);
        }
        LookupCardState::Error { source, kind, .. } => {
            let (title, detail) = error_copy(*kind);
            popup.set_source_text(source.into());
            popup.set_error_title(title.into());
            popup.set_error_detail(detail.into());
            set_popup_dimensions(popup, source, "");
            popup.set_loading(false);
            popup.set_error_visible(true);
            popup.set_text_mode(source.split_whitespace().count() > 1);
        }
    }
}

const POPUP_MIN_WIDTH: u32 = 360;
const POPUP_MAX_WIDTH: u32 = 640;
const POPUP_MIN_HEIGHT: u32 = 220;
const POPUP_MAX_HEIGHT: u32 = 420;

fn set_popup_dimensions(popup: &QuickLookupPopup, source: &str, translation: &str) {
    // Estimate the layout using the rendered text length. Slint then performs the final
    // word-wrapping inside these bounded dimensions, keeping short lookups compact.
    let longest_line = source
        .lines()
        .chain(translation.lines())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let width_hint = u32::try_from(longest_line.saturating_mul(8)).unwrap_or(u32::MAX);
    let width = (120_u32.saturating_add(width_hint)).clamp(POPUP_MIN_WIDTH, POPUP_MAX_WIDTH);
    let chars_per_line = (width.saturating_sub(52) / 8).max(1) as usize;
    let source_lines = wrapped_line_count(source, chars_per_line);
    let translation_lines = wrapped_line_count(translation, chars_per_line);
    let content_lines =
        u32::try_from(source_lines.saturating_add(translation_lines).max(1)).unwrap_or(u32::MAX);
    let height =
        (126_u32 + content_lines.saturating_mul(22)).clamp(POPUP_MIN_HEIGHT, POPUP_MAX_HEIGHT);

    popup.set_popup_width(f32::from(u16::try_from(width).unwrap_or(u16::MAX)));
    popup.set_popup_height(f32::from(u16::try_from(height).unwrap_or(u16::MAX)));
}

fn wrapped_line_count(text: &str, chars_per_line: usize) -> usize {
    text.lines()
        .map(|line| line.chars().count().div_ceil(chars_per_line).max(1))
        .sum()
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows_window {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use windows::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SWP_FRAMECHANGED,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
            WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        },
    };

    use super::{QuickLookupPopup, UiControllerError, WINDOWS_POPUP_MONITOR};

    pub(super) fn configure_visible_popup(window: &slint::Window) -> Result<(), UiControllerError> {
        let hwnd = native_hwnd(window)?;
        set_popup_style(hwnd, false);
        let scale = f64::from(window.scale_factor());
        let size = window.size();
        let logical_size = lvos_platform::LogicalSize {
            width: f64::from(size.width) / scale,
            height: f64::from(size.height) / scale,
        };
        let placement = lvos_platform::windows::popup_placement(hwnd, logical_size)
            .map_err(|_| platform_error("Windows Popup placement is unavailable"))?;
        let x = saturating_physical(placement.origin.x, placement.scale_factor);
        let y = saturating_physical(placement.origin.y, placement.scale_factor);
        window.set_position(slint::PhysicalPosition::new(x, y));
        // SAFETY: hwnd is the live Popup window; flags preserve size and avoid activation.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|_| platform_error("Windows Popup could not be shown without activation"))
    }

    pub(super) fn install_outside_click_monitor(
        popup: &QuickLookupPopup,
    ) -> Result<lvos_platform::windows::OutsideClickMonitor, UiControllerError> {
        let hwnd = native_hwnd(popup.window())?;
        let mut bounds = RECT::default();
        // SAFETY: bounds remains writable and hwnd is live.
        unsafe { GetWindowRect(hwnd, &raw mut bounds) }
            .map_err(|_| platform_error("Windows Popup bounds are unavailable"))?;
        let popup_weak = popup.as_weak();
        let dismiss = std::sync::Arc::new(move || {
            let popup_weak = popup_weak.clone();
            if let Err(error) = slint::invoke_from_event_loop(move || {
                if let Some(popup) = popup_weak.upgrade() {
                    popup.invoke_dismiss_requested();
                }
                WINDOWS_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
            }) {
                tracing::warn!(%error, "failed to dispatch Windows Popup dismissal");
            }
        });
        lvos_platform::windows::OutsideClickMonitor::install(
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            dismiss,
        )
        .map_err(|_| platform_error("Windows outside-click hook is unavailable"))
    }

    pub(super) fn prepare_no_activate(window: &slint::Window) -> Result<(), UiControllerError> {
        let hwnd = native_hwnd(window)?;
        set_popup_style(hwnd, true);
        Ok(())
    }

    fn set_popup_style(hwnd: HWND, no_activate: bool) {
        // SAFETY: reading/updating this window's extended style is valid while hwnd is live.
        let styles = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let no_activate_style = if no_activate { 0x0800_0000_isize } else { 0 };
        let Ok(tool_window_style) = isize::try_from(WS_EX_TOOLWINDOW.0) else {
            tracing::warn!("Windows tool-window style is not representable");
            return;
        };
        let Ok(app_window_style) = isize::try_from(WS_EX_APPWINDOW.0) else {
            tracing::warn!("Windows app-window style is not representable");
            return;
        };
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                (styles & !0x0800_0000_isize & !app_window_style)
                    | no_activate_style
                    | tool_window_style,
            );
        }
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        };
    }

    fn native_hwnd(window: &slint::Window) -> Result<HWND, UiControllerError> {
        let handle = window.window_handle();
        let raw = handle
            .window_handle()
            .map_err(|_| platform_error("native Windows Popup handle is unavailable"))?
            .as_raw();
        let RawWindowHandle::Win32(handle) = raw else {
            return Err(platform_error("native Popup is not a Win32 window"));
        };
        Ok(HWND(handle.hwnd.get() as *mut _))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn saturating_physical(value: f64, scale: f64) -> i32 {
        let physical = value * scale;
        if physical <= f64::from(i32::MIN) {
            i32::MIN
        } else if physical >= f64::from(i32::MAX) {
            i32::MAX
        } else {
            physical.round() as i32
        }
    }

    fn platform_error(message: &'static str) -> UiControllerError {
        UiControllerError::Platform(slint::PlatformError::Other(message.into()))
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

#[cfg(test)]
mod lifecycle_tests {
    use super::{DEFAULT_POPUP_IDLE_TIMEOUT, PopupLifecycle, PopupLifecycleState, UiHosts};
    use std::time::Duration;

    #[test]
    fn popup_idle_timeout_defaults_to_thirty_seconds() {
        assert_eq!(DEFAULT_POPUP_IDLE_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn reconfiguring_a_warm_popup_invalidates_its_previous_timer_generation() {
        let mut lifecycle = PopupLifecycle::default();
        lifecycle.activate();
        let old_timer = lifecycle.warm();
        let replacement_timer = lifecycle.warm();

        assert_ne!(old_timer, replacement_timer);
        assert!(!lifecycle.release_if_current(old_timer));
        assert!(lifecycle.release_if_current(replacement_timer));
        assert_eq!(lifecycle.state, PopupLifecycleState::Cold);
    }

    #[test]
    fn coordinator_starts_without_any_window_host() {
        let hosts = UiHosts::new();
        assert!(hosts.main.is_none());
        assert!(hosts.popup.is_none());
        assert!(hosts.permission.is_none());
        assert_eq!(hosts.popup_lifecycle.state, PopupLifecycleState::Cold);
    }

    #[test]
    fn popup_moves_from_cold_to_active_to_warm_to_cold() {
        let mut lifecycle = PopupLifecycle::default();
        assert_eq!(lifecycle.state, PopupLifecycleState::Cold);

        lifecycle.activate();
        assert_eq!(lifecycle.state, PopupLifecycleState::Active);

        let warm_generation = lifecycle.warm();
        assert_eq!(lifecycle.state, PopupLifecycleState::Warm);
        assert!(lifecycle.release_if_current(warm_generation));
        assert_eq!(lifecycle.state, PopupLifecycleState::Cold);
    }

    #[test]
    fn popup_warm_reuse_invalidates_the_old_timeout() {
        let mut lifecycle = PopupLifecycle::default();
        lifecycle.activate();
        let stale_generation = lifecycle.warm();

        lifecycle.activate();
        assert!(!lifecycle.release_if_current(stale_generation));
        assert_eq!(lifecycle.state, PopupLifecycleState::Active);
    }

    #[test]
    fn timeout_cannot_release_a_newer_warm_session() {
        let mut lifecycle = PopupLifecycle::default();
        lifecycle.activate();
        let stale_generation = lifecycle.warm();
        lifecycle.activate();
        let current_generation = lifecycle.warm();

        assert!(!lifecycle.release_if_current(stale_generation));
        assert_eq!(lifecycle.state, PopupLifecycleState::Warm);
        assert!(lifecycle.release_if_current(current_generation));
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos_window {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::UiControllerError;

    pub(super) fn show_without_activation_and_place(
        window: &slint::Window,
    ) -> Result<lvos_platform::LogicalRect, UiControllerError> {
        let handle = window.window_handle();
        let raw = handle
            .window_handle()
            .map_err(|_| platform_error("native Popup handle is unavailable"))?
            .as_raw();
        let RawWindowHandle::AppKit(handle) = raw else {
            return Err(platform_error("native Popup is not an AppKit window"));
        };
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        view.window()
            .ok_or_else(|| platform_error("native Popup NSWindow is unavailable"))?
            .orderFrontRegardless();
        let scale = f64::from(window.scale_factor());
        let size = window.size();
        let placement = lvos_platform::macos::popup_placement(lvos_platform::LogicalSize {
            width: f64::from(size.width) / scale,
            height: f64::from(size.height) / scale,
        })
        .map_err(|_| platform_error("Popup placement is unavailable"))?;
        window.set_position(slint::PhysicalPosition::new(
            saturating_physical(placement.origin.x, placement.scale_factor),
            saturating_physical(placement.origin.y, placement.scale_factor),
        ));
        Ok(lvos_platform::LogicalRect {
            origin: placement.origin,
            size: lvos_platform::LogicalSize {
                width: f64::from(size.width) / scale,
                height: f64::from(size.height) / scale,
            },
        })
    }

    pub(super) fn show_and_activate(window: &slint::Window) -> Result<(), UiControllerError> {
        let handle = window.window_handle();
        let raw = handle
            .window_handle()
            .map_err(|_| platform_error("native permission window handle is unavailable"))?
            .as_raw();
        let RawWindowHandle::AppKit(handle) = raw else {
            return Err(platform_error(
                "native permission window is not an AppKit window",
            ));
        };
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let native_window = view
            .window()
            .ok_or_else(|| platform_error("native permission NSWindow is unavailable"))?;
        let marker = MainThreadMarker::new().ok_or_else(|| {
            platform_error("permission window must be activated on the main thread")
        })?;
        #[allow(deprecated)]
        NSApplication::sharedApplication(marker).activateIgnoringOtherApps(true);
        native_window.makeKeyAndOrderFront(None);
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn saturating_physical(value: f64, scale: f64) -> i32 {
        let physical = value * scale;
        if physical <= f64::from(i32::MIN) {
            i32::MIN
        } else if physical >= f64::from(i32::MAX) {
            i32::MAX
        } else {
            physical.round() as i32
        }
    }

    pub(super) fn platform_error(message: &'static str) -> UiControllerError {
        UiControllerError::Platform(slint::PlatformError::Other(message.into()))
    }
}
