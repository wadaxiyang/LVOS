#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::cell::RefCell;
use std::{error::Error, fmt};

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

pub struct UiController {
    popup: QuickLookupPopup,
    confirmations: crate::ConfirmationBroker,
    main_window: MainWindow,
    permission_window: PermissionWindow,
}

impl fmt::Debug for UiController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiController")
            .field("popup_focus", &self.popup_focus())
            .finish_non_exhaustive()
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

impl UiController {
    /// Creates the three independent Desktop surfaces on the Slint event-loop thread.
    ///
    /// # Errors
    /// Returns a platform error if a generated window cannot be constructed.
    pub fn new() -> Result<Self, UiControllerError> {
        initialize_desktop_backend()?;
        let main_window = MainWindow::new().map_err(UiControllerError::Platform)?;
        let confirmations = crate::ConfirmationBroker::install(&main_window);
        let controller = Self {
            confirmations,
            popup: QuickLookupPopup::new().map_err(UiControllerError::Platform)?,
            main_window,
            permission_window: PermissionWindow::new().map_err(UiControllerError::Platform)?,
        };
        let popup_weak = controller.popup.as_weak();
        controller.popup.on_dismiss_requested(move || {
            if let Some(popup) = popup_weak.upgrade()
                && let Err(error) = dismiss_popup(&popup)
            {
                tracing::warn!(%error, "failed to hide Lookup Card");
            }
        });
        let interaction_popup = controller.popup.as_weak();
        controller.popup.on_interaction_started(move || {
            if let Some(popup) = interaction_popup.upgrade() {
                popup.set_native_interactive(true);
            }
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
    pub fn confirmations(&self) -> &crate::ConfirmationBroker {
        &self.confirmations
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
        if !self.popup.get_native_show_requested() {
            PopupFocusState::Hidden
        } else if self.popup.get_native_interactive() {
            PopupFocusState::Interactive
        } else {
            PopupFocusState::VisibleNoActivate
        }
    }

    /// Populates the Lookup Card without displaying Provider or sync metadata.
    pub fn apply_lookup_state(&self, state: &LookupCardState) {
        apply_lookup_state_to_popup(&self.popup, state);
    }

    /// Marks the Popup visible without activation. Stage 06/07 supplies native no-activate show.
    pub fn mark_popup_visible_no_activate(&self) {
        self.popup.set_native_show_requested(true);
        self.popup.set_native_interactive(false);
    }

    pub fn mark_popup_interactive(&self) {
        self.popup.set_native_interactive(true);
    }

    pub fn mark_popup_hidden(&self) {
        self.popup.set_native_show_requested(false);
        self.popup.set_native_interactive(false);
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

    /// Queues native creation and no-activate presentation of the latest Lookup Card.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be shown.
    pub fn show_lookup_card(&self, state: &LookupCardState) -> Result<(), UiControllerError> {
        self.apply_lookup_state(state);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        queue_popup_show(&self.popup)?;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        self.popup.show().map_err(UiControllerError::Platform)?;
        Ok(())
    }

    /// Hides the Popup and updates its interaction lifecycle state.
    ///
    /// # Errors
    /// Returns a platform error if the native Popup cannot be hidden.
    pub fn hide_lookup_card(&self) -> Result<(), UiControllerError> {
        dismiss_popup(&self.popup)?;
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
        use slint::winit_030::WinitWindowAccessor;
        self.main_window
            .show()
            .map_err(UiControllerError::Platform)?;
        let weak = self.main_window.as_weak();
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
        self.confirmations.invalidate();
        self.main_window.hide().map_err(UiControllerError::Platform)
    }
}

impl Drop for UiController {
    fn drop(&mut self) {
        self.confirmations.invalidate();
        let _ = dismiss_popup(&self.popup);
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
            let _ = dismiss_popup(&popup);
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
                        let _ = dismiss_popup(&popup);
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
                        let _ = dismiss_popup(&popup);
                    }
                    _ => {}
                }
            }
            EventResult::Propagate
        });
        let result = show_prepared_popup(&popup);
        if let Err(error) = result {
            tracing::warn!(%error, "failed to present native Lookup Card");
            let _ = dismiss_popup(&popup);
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
                        let _ = dismiss_popup(&popup);
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

    use super::{QuickLookupPopup, UiControllerError, WINDOWS_POPUP_MONITOR, dismiss_popup};

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
                    && let Err(error) = dismiss_popup(&popup)
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
