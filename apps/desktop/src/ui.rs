#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::cell::RefCell;
use std::{cell::Cell, error::Error, fmt, rc::Rc};

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

pub use generated::{DeviceRecord, MainWindow, PermissionWindow, QuickLookupPopup, UiRecord};

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
    popup.set_loading(false);
    popup.set_error_visible(true);
    popup.set_text_mode(source.split_whitespace().count() > 1);
    #[cfg(target_os = "macos")]
    {
        popup.show().map_err(UiControllerError::Platform)?;
        let popup_bounds = macos_window::show_without_activation_and_place(popup.window())?;
        let popup_weak = popup.as_weak();
        let dismiss = std::sync::Arc::new(move || {
            let popup_weak = popup_weak.clone();
            if let Err(error) = slint::invoke_from_event_loop(move || {
                if let Some(popup) = popup_weak.upgrade()
                    && let Err(error) = popup.hide()
                {
                    tracing::warn!(%error, "failed to hide captured Lookup Card");
                }
                CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
            }) {
                tracing::warn!(%error, "failed to dispatch captured Popup dismissal");
            }
        });
        let monitor = lvos_platform::macos::OutsideClickMonitor::install(popup_bounds, dismiss)
            .map_err(|_| macos_window::platform_error("outside-click monitor is unavailable"))?;
        CAPTURE_POPUP_MONITOR.with(|active| active.borrow_mut().replace(monitor));
    }
    #[cfg(target_os = "windows")]
    windows_window::show_without_activation_and_monitor(popup)?;
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
    #[cfg(target_os = "macos")]
    {
        popup.show().map_err(UiControllerError::Platform)?;
        let popup_bounds = macos_window::show_without_activation_and_place(popup.window())?;
        let popup_weak = popup.as_weak();
        let dismiss = std::sync::Arc::new(move || {
            let popup_weak = popup_weak.clone();
            if let Err(error) = slint::invoke_from_event_loop(move || {
                if let Some(popup) = popup_weak.upgrade()
                    && let Err(error) = popup.hide()
                {
                    tracing::warn!(%error, "failed to hide Lookup Card");
                }
                CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
            }) {
                tracing::warn!(%error, "failed to dispatch Lookup Card dismissal");
            }
        });
        let monitor = lvos_platform::macos::OutsideClickMonitor::install(popup_bounds, dismiss)
            .map_err(|_| macos_window::platform_error("outside-click monitor is unavailable"))?;
        CAPTURE_POPUP_MONITOR.with(|active| active.borrow_mut().replace(monitor));
    }
    #[cfg(target_os = "windows")]
    windows_window::show_without_activation_and_monitor(popup)?;
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
    permission.show().map_err(UiControllerError::Platform)?;
    macos_window::show_and_activate(permission.window())
}

pub struct UiController {
    popup: QuickLookupPopup,
    main_window: MainWindow,
    permission_window: PermissionWindow,
    popup_focus: Rc<Cell<PopupFocusState>>,
    #[cfg(target_os = "macos")]
    outside_click_monitor: Rc<RefCell<Option<lvos_platform::macos::OutsideClickMonitor>>>,
    #[cfg(target_os = "windows")]
    outside_click_monitor: Rc<RefCell<Option<lvos_platform::windows::OutsideClickMonitor>>>,
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
            permission_window: PermissionWindow::new().map_err(UiControllerError::Platform)?,
            popup_focus: Rc::new(Cell::new(PopupFocusState::Hidden)),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            outside_click_monitor: Rc::new(RefCell::new(None)),
        };
        let popup_weak = controller.popup.as_weak();
        let dismiss_focus = Rc::clone(&controller.popup_focus);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let dismiss_monitor = Rc::clone(&controller.outside_click_monitor);
        controller.popup.on_dismiss_requested(move || {
            if let Some(popup) = popup_weak.upgrade()
                && let Err(error) = popup.hide()
            {
                tracing::warn!(%error, "failed to hide Lookup Card");
            }
            dismiss_focus.set(PopupFocusState::Hidden);
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                dismiss_monitor.borrow_mut().take();
                #[cfg(target_os = "macos")]
                CAPTURE_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
                #[cfg(target_os = "windows")]
                WINDOWS_POPUP_MONITOR.with(|monitor| monitor.borrow_mut().take());
            }
        });
        let interaction_focus = Rc::clone(&controller.popup_focus);
        controller.popup.on_interaction_started(move || {
            interaction_focus.set(PopupFocusState::Interactive);
        });
        let settings = controller.main_window.as_weak();
        controller.main_window.on_validate_provider_settings(
            move |tokenhub_model, tokenhub_key| {
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
    pub fn permission_window(&self) -> &PermissionWindow {
        &self.permission_window
    }

    #[must_use]
    pub fn popup_focus(&self) -> PopupFocusState {
        self.popup_focus.get()
    }

    /// Populates the Lookup Card without displaying Provider or sync metadata.
    pub fn apply_lookup_state(&self, state: &LookupCardState) {
        apply_lookup_state_to_popup(&self.popup, state);
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
        #[cfg(not(target_os = "windows"))]
        self.popup.show().map_err(UiControllerError::Platform)?;
        #[cfg(target_os = "macos")]
        {
            let popup_bounds =
                macos_window::show_without_activation_and_place(self.popup.window())?;
            let popup = self.popup.as_weak();
            let dismiss = std::sync::Arc::new(move || {
                let popup = popup.clone();
                if let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(popup) = popup.upgrade()
                        && let Err(error) = popup.hide()
                    {
                        tracing::warn!(%error, "failed to hide Lookup Card after outside click");
                    }
                }) {
                    tracing::warn!(%error, "failed to dispatch outside-click dismissal");
                }
            });
            let monitor = lvos_platform::macos::OutsideClickMonitor::install(popup_bounds, dismiss)
                .map_err(|_| {
                    macos_window::platform_error("outside-click monitor is unavailable")
                })?;
            self.outside_click_monitor.borrow_mut().replace(monitor);
        }
        #[cfg(target_os = "windows")]
        {
            // A Loading card can be replaced by Ready/Error while it is still visible. Stop the
            // previous hook before installing its replacement so their lifetimes cannot overlap.
            self.outside_click_monitor.borrow_mut().take();
            windows_window::prepare_no_activate(self.popup.window())?;
            self.popup.show().map_err(UiControllerError::Platform)?;
            windows_window::configure_visible_popup(self.popup.window())?;
            let monitor = windows_window::install_outside_click_monitor(&self.popup)?;
            self.outside_click_monitor.borrow_mut().replace(monitor);
        }
        self.mark_popup_visible_no_activate();
        Ok(())
    }

    /// Hides the Popup and updates its interaction lifecycle state.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be hidden.
    pub fn hide_lookup_card(&self) -> Result<(), UiControllerError> {
        self.popup.hide().map_err(UiControllerError::Platform)?;
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.outside_click_monitor.borrow_mut().take();
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

fn apply_lookup_state_to_popup(popup: &QuickLookupPopup, state: &LookupCardState) {
    match state {
        LookupCardState::Hidden => {}
        LookupCardState::Loading { source, .. } => {
            popup.set_source_text(source.into());
            popup.set_translated_text(SharedString::default());
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
            popup.set_loading(false);
            popup.set_error_visible(true);
            popup.set_text_mode(source.split_whitespace().count() > 1);
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows_window {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use windows::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_EX_TOOLWINDOW,
        },
    };

    use super::{QuickLookupPopup, UiControllerError, WINDOWS_POPUP_MONITOR};

    pub(super) fn show_without_activation_and_monitor(
        popup: &QuickLookupPopup,
    ) -> Result<(), UiControllerError> {
        // Runtime state transitions display the same Popup repeatedly (Loading -> Ready/Error).
        // Tear down the old hook first; replacing it after installation lets the old hook's
        // cleanup race with the new one.
        WINDOWS_POPUP_MONITOR.with(|active| active.borrow_mut().take());
        let hwnd = native_hwnd(popup.window())?;
        set_popup_style(hwnd, true);
        popup.show().map_err(UiControllerError::Platform)?;
        configure_visible_popup(popup.window())?;
        let monitor = install_outside_click_monitor(popup)?;
        WINDOWS_POPUP_MONITOR.with(|active| active.borrow_mut().replace(monitor));
        Ok(())
    }

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
                if let Some(popup) = popup_weak.upgrade()
                    && let Err(error) = popup.hide()
                {
                    tracing::warn!(%error, "failed to hide Windows Lookup Card");
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
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                (styles & !0x0800_0000_isize) | no_activate_style | tool_window_style,
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
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
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
