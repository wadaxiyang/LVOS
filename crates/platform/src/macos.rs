#![allow(unsafe_code)]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, ContentFormat};
use fs2::FileExt;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use notify_rust::Notification;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuId, MenuItem},
};

use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSPasteboard, NSScreen, NSWorkspace};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use objc2_foundation::{NSString, NSURL};

use crate::{
    CaptureError, InstanceAcquisition, LogicalPoint, LogicalRect, LogicalSize, NotificationService,
    PlatformError, PopupPlacement, SelectionCapture, SingleInstanceGuard, SingleInstanceService,
    place_popup,
};

const KEYCHAIN_SERVICE: &str = "site.niuniu770.lvos";

#[derive(Debug, Default)]
pub struct MacOsCredentialStore;

impl CredentialStore for MacOsCredentialStore {
    fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError> {
        let entry = credential_entry(scope)?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                tracing::warn!(%error, "macOS Keychain read failed");
                Err(AuthError::CredentialStore)
            }
        }
    }

    fn contains(&self, scope: &CredentialScope) -> Result<bool, AuthError> {
        self.get(scope).map(|secret| secret.is_some())
    }

    fn set(&self, scope: &CredentialScope, secret: &[u8]) -> Result<(), AuthError> {
        if secret.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }
        credential_entry(scope)?
            .set_secret(secret)
            .map_err(|error| {
                tracing::warn!(%error, "macOS Keychain write failed");
                AuthError::CredentialStore
            })
    }

    fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError> {
        match credential_entry(scope)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "macOS Keychain delete failed");
                Err(AuthError::CredentialStore)
            }
        }
    }
}

fn credential_entry(scope: &CredentialScope) -> Result<keyring::Entry, AuthError> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &credential_account(scope))
        .map_err(|_| AuthError::CredentialStore)
}

fn credential_account(scope: &CredentialScope) -> String {
    format!(
        "{}|{}|{}|{}",
        scope.server_origin,
        scope.user_id,
        scope.device_id,
        credential_key_name(scope.key)
    )
}

const fn credential_key_name(key: CredentialKey) -> &'static str {
    match key {
        CredentialKey::RetiredTranslationApiKey => "google-api-key",
        CredentialKey::TencentTokenHubApiKey => "tokenhub-api-key",
        CredentialKey::ServerRefreshToken => "server-refresh-token",
    }
}

#[derive(Debug, Default)]
pub struct MacOsNotificationService;

impl NotificationService for MacOsNotificationService {
    fn error(&self, message: &str) -> Result<(), PlatformError> {
        show_notification("LVOS Error", message)
    }

    fn warning(&self, message: &str) -> Result<(), PlatformError> {
        show_notification("LVOS Warning", message)
    }
}

fn show_notification(title: &str, message: &str) -> Result<(), PlatformError> {
    Notification::new()
        .appname("LVOS")
        .summary(title)
        .body(message)
        .show()
        .map(|_| ())
        .map_err(|_| PlatformError::IntegrationFailure)
}

/// Returns whether macOS currently permits LVOS to post the Cmd+C capture event.
#[must_use]
pub fn accessibility_permission_granted() -> bool {
    CGPreflightPostEventAccess()
}

/// Asks macOS to register the current app in the Accessibility permission flow.
#[must_use]
pub fn request_accessibility_permission() -> bool {
    CGRequestPostEventAccess()
}

/// Opens the macOS Accessibility privacy pane for manual approval.
///
/// # Errors
/// Returns an integration error if macOS rejects the System Settings URL.
pub fn open_accessibility_settings() -> Result<(), PlatformError> {
    let value = NSString::from_str(
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    );
    let url = NSURL::URLWithString(&value).ok_or(PlatformError::IntegrationFailure)?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

/// Opens one validated HTTPS page in the user's default browser.
///
/// # Errors
/// Returns an integration error for a non-HTTPS URL or if macOS rejects the request.
pub fn open_web_url(value: &str) -> Result<(), PlatformError> {
    if !value.starts_with("https://") {
        return Err(PlatformError::IntegrationFailure);
    }
    let value = NSString::from_str(value);
    let url = NSURL::URLWithString(&value).ok_or(PlatformError::IntegrationFailure)?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

/// Returns whether the unsigned app has a user-level `launchd` login item.
#[must_use]
pub fn start_at_login_enabled() -> bool {
    launch_agent_path().is_some_and(|path| path.is_file())
}

/// Registers or unregisters the unsigned app with the user's macOS `launchd` domain.
///
/// # Errors
/// Returns an integration error when the executable or user `LaunchAgents` directory is unavailable.
pub fn set_start_at_login(enabled: bool) -> Result<(), PlatformError> {
    let path = launch_agent_path().ok_or(PlatformError::IntegrationFailure)?;
    if enabled {
        let executable = std::env::current_exe().map_err(|_| PlatformError::IntegrationFailure)?;
        let executable = executable
            .to_str()
            .ok_or(PlatformError::IntegrationFailure)?;
        let parent = path.parent().ok_or(PlatformError::IntegrationFailure)?;
        std::fs::create_dir_all(parent).map_err(|_| PlatformError::IntegrationFailure)?;
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\n\
             <key>Label</key><string>{}</string>\n\
             <key>ProgramArguments</key><array><string>{}</string></array>\n\
             <key>RunAtLoad</key><true/>\n\
             </dict></plist>\n",
            lvos_core::DESKTOP_APP_ID,
            escape_xml(executable)
        );
        let temporary = path.with_extension("plist.tmp");
        std::fs::write(&temporary, plist).map_err(|_| PlatformError::IntegrationFailure)?;
        std::fs::rename(temporary, path).map_err(|_| PlatformError::IntegrationFailure)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(PlatformError::IntegrationFailure),
        }
    }
}

fn launch_agent_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", lvos_core::DESKTOP_APP_ID))
    })
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Debug)]
pub struct MacOsSingleInstanceService {
    lock_path: PathBuf,
}

impl MacOsSingleInstanceService {
    #[must_use]
    pub fn new(application_data_root: &Path) -> Self {
        Self {
            lock_path: application_data_root.join("desktop.lock"),
        }
    }
}

impl SingleInstanceService for MacOsSingleInstanceService {
    fn acquire(&self) -> Result<InstanceAcquisition, PlatformError> {
        if let Some(parent) = self.lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| PlatformError::IntegrationFailure)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(|_| PlatformError::IntegrationFailure)?;
        let socket_path = self.lock_path.with_extension("socket");
        match file.try_lock_exclusive() {
            Ok(()) => {
                if socket_path.exists() {
                    std::fs::remove_file(&socket_path)
                        .map_err(|_| PlatformError::IntegrationFailure)?;
                }
                let listener = UnixListener::bind(&socket_path)
                    .map_err(|_| PlatformError::IntegrationFailure)?;
                let handler = Arc::new(Mutex::new(None));
                let cancellation = Arc::new(AtomicBool::new(false));
                let thread_handler = Arc::clone(&handler);
                let thread_cancellation = Arc::clone(&cancellation);
                let listener_thread = std::thread::Builder::new()
                    .name("lvos-instance-signal".to_owned())
                    .spawn(move || {
                        listen_for_open_requests(&listener, &thread_handler, &thread_cancellation);
                    })
                    .map_err(|_| PlatformError::IntegrationFailure)?;
                Ok(InstanceAcquisition::Primary(Box::new(MacOsInstanceGuard {
                    file: Some(file),
                    socket_path,
                    existing: false,
                    handler,
                    cancellation,
                    listener_thread: Some(listener_thread),
                })))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(
                InstanceAcquisition::Existing(Box::new(MacOsInstanceGuard {
                    file: None,
                    socket_path,
                    existing: true,
                    handler: Arc::new(Mutex::new(None)),
                    cancellation: Arc::new(AtomicBool::new(false)),
                    listener_thread: None,
                })),
            ),
            Err(_) => Err(PlatformError::IntegrationFailure),
        }
    }
}

struct MacOsInstanceGuard {
    file: Option<File>,
    socket_path: PathBuf,
    existing: bool,
    handler: OpenHandler,
    cancellation: Arc<AtomicBool>,
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

type OpenHandler = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

impl std::fmt::Debug for MacOsInstanceGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MacOsInstanceGuard")
            .field("socket_path", &self.socket_path)
            .field("existing", &self.existing)
            .finish_non_exhaustive()
    }
}

impl SingleInstanceGuard for MacOsInstanceGuard {
    fn signal_existing(&self) -> Result<(), PlatformError> {
        if !self.existing {
            return Ok(());
        }
        let mut stream = UnixStream::connect(&self.socket_path)
            .map_err(|_| PlatformError::IntegrationFailure)?;
        stream
            .write_all(b"open")
            .map_err(|_| PlatformError::IntegrationFailure)
    }

    fn set_open_handler(&self, handler: Arc<dyn Fn() + Send + Sync>) -> Result<(), PlatformError> {
        if self.existing {
            return Err(PlatformError::Unsupported);
        }
        *self
            .handler
            .lock()
            .map_err(|_| PlatformError::IntegrationFailure)? = Some(handler);
        Ok(())
    }
}

impl Drop for MacOsInstanceGuard {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(listener_thread) = self.listener_thread.take() {
            let _ = listener_thread.join();
        }
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

fn listen_for_open_requests(
    listener: &UnixListener,
    handler: &OpenHandler,
    cancellation: &AtomicBool,
) {
    for connection in listener.incoming() {
        if cancellation.load(Ordering::Acquire) {
            break;
        }
        let Ok(mut stream) = connection else {
            break;
        };
        let mut command = [0_u8; 4];
        if stream.read_exact(&mut command).is_ok()
            && command == *b"open"
            && let Ok(handler) = handler.lock()
            && let Some(handler) = handler.as_ref()
        {
            handler();
        }
    }
}

pub struct MacOsHotKey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
    event_id: Arc<AtomicU32>,
}

impl std::fmt::Debug for MacOsHotKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MacOsHotKey")
            .field("hotkey", &self.hotkey)
            .finish_non_exhaustive()
    }
}

impl MacOsHotKey {
    /// Registers a system-wide shortcut and fails when another application owns it.
    ///
    /// # Errors
    /// Returns a conflict/integration error if parsing or registration fails.
    pub fn register(shortcut: &str) -> Result<Self, PlatformError> {
        let hotkey = shortcut
            .parse::<HotKey>()
            .map_err(|_| PlatformError::IntegrationFailure)?;
        let manager = GlobalHotKeyManager::new().map_err(|_| PlatformError::IntegrationFailure)?;
        manager
            .register(hotkey)
            .map_err(|_| PlatformError::Conflict)?;
        Ok(Self {
            manager,
            hotkey,
            event_id: Arc::new(AtomicU32::new(hotkey.id())),
        })
    }

    /// Routes matching press events directly from the native event source.
    pub fn set_pressed_handler(&self, handler: Arc<dyn Fn() + Send + Sync>) {
        let event_id = Arc::clone(&self.event_id);
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == event_id.load(Ordering::Acquire) && event.state == HotKeyState::Pressed {
                handler();
            }
        }));
    }

    /// Replaces the registered shortcut without creating a period with no active shortcut.
    ///
    /// # Errors
    /// Returns a conflict/integration error and keeps the existing shortcut on failure.
    pub fn update(&mut self, shortcut: &str) -> Result<(), PlatformError> {
        let replacement = shortcut
            .parse::<HotKey>()
            .map_err(|_| PlatformError::IntegrationFailure)?;
        if replacement.id() == self.hotkey.id() {
            return Ok(());
        }
        self.manager
            .register(replacement)
            .map_err(|_| PlatformError::Conflict)?;
        if self.manager.unregister(self.hotkey).is_err() {
            let _ = self.manager.unregister(replacement);
            return Err(PlatformError::IntegrationFailure);
        }
        self.hotkey = replacement;
        self.event_id.store(replacement.id(), Ordering::Release);
        Ok(())
    }
}

/// Parses the user-facing macOS shortcut notation into `global-hotkey` syntax.
///
/// # Errors
/// Returns an integration error for an empty or unsupported shortcut.
pub fn parse_hotkey_display(value: &str) -> Result<String, PlatformError> {
    let normalized = value
        .trim()
        .replace('⌥', "option+")
        .replace('⌘', "command+")
        .replace('⇧', "shift+")
        .replace('⌃', "control+")
        .replace(' ', "")
        .to_ascii_lowercase()
        .replace("option+", "alt+")
        .replace("command+", "super+")
        .replace("cmd+", "super+")
        .replace("control+", "ctrl+");
    let mut parts = normalized
        .split('+')
        .filter(|part| !part.is_empty())
        .peekable();
    let mut output = Vec::new();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if part.len() != 1 || !part.as_bytes()[0].is_ascii_alphanumeric() {
                return Err(PlatformError::IntegrationFailure);
            }
            output.push(format!("Key{}", part.to_ascii_uppercase()));
        } else if matches!(part, "alt" | "super" | "shift" | "ctrl") {
            output.push(part.to_owned());
        } else {
            return Err(PlatformError::IntegrationFailure);
        }
    }
    if output.len() < 2 {
        return Err(PlatformError::IntegrationFailure);
    }
    Ok(output.join("+"))
}

impl Drop for MacOsHotKey {
    fn drop(&mut self) {
        let _ = self.manager.unregister(self.hotkey);
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TrayAction {
    OpenMainWindow,
    Quit,
}

pub struct MacOsTray {
    _icon: TrayIcon,
    open_id: MenuId,
    quit_id: MenuId,
}

impl std::fmt::Debug for MacOsTray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("MacOsTray").finish_non_exhaustive()
    }
}

impl MacOsTray {
    /// Installs the macOS menu-bar icon and its explicit open/quit menu.
    ///
    /// # Errors
    /// Returns an integration error when `AppKit` rejects the icon or menu.
    pub fn install() -> Result<Self, PlatformError> {
        let menu = Menu::new();
        let open = MenuItem::with_id("lvos-open", "Open LVOS", true, None);
        let quit = MenuItem::with_id("lvos-quit", "Quit LVOS", true, None);
        menu.append_items(&[&open, &quit])
            .map_err(|_| PlatformError::IntegrationFailure)?;
        let open_id = open.id().clone();
        let quit_id = quit.id().clone();
        let icon = TrayIconBuilder::new()
            .with_tooltip("LVOS")
            .with_title("LVOS")
            .with_icon(menu_bar_icon()?)
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            .build()
            .map_err(|_| PlatformError::IntegrationFailure)?;
        Ok(Self {
            _icon: icon,
            open_id,
            quit_id,
        })
    }

    /// Routes matching menu events directly from the native event source.
    pub fn set_action_handler(&self, handler: Arc<dyn Fn(TrayAction) + Send + Sync>) {
        let open_id = self.open_id.clone();
        let quit_id = self.quit_id.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                handler(TrayAction::OpenMainWindow);
            } else if event.id == quit_id {
                handler(TrayAction::Quit);
            }
        }));
    }
}

fn menu_bar_icon() -> Result<Icon, PlatformError> {
    const SIDE: u32 = 18;
    let mut pixels = vec![0_u8; (SIDE * SIDE * 4) as usize];
    for y in 2..16 {
        for x in 2..16 {
            let on = x == 2 || x == 15 || y == 2 || y == 15 || x == y || x + y == 17;
            if on {
                let offset = ((y * SIDE + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    Icon::from_rgba(pixels, SIDE, SIDE).map_err(|_| PlatformError::IntegrationFailure)
}

/// Samples the cursor and current screen exactly once, then computes a clamped logical placement.
///
/// # Errors
/// Returns an integration error when called off the main thread or no screen is available.
pub fn popup_placement(popup: LogicalSize) -> Result<PopupPlacement, PlatformError> {
    let marker = MainThreadMarker::new().ok_or(PlatformError::IntegrationFailure)?;
    let screen = NSScreen::mainScreen(marker).ok_or(PlatformError::IntegrationFailure)?;
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let cursor = NSEvent::mouseLocation();
    let logical_cursor = LogicalPoint {
        x: cursor.x,
        y: frame.size.height - cursor.y,
    };
    let work_area = LogicalRect {
        origin: LogicalPoint {
            x: visible.origin.x,
            y: frame.size.height - visible.origin.y - visible.size.height,
        },
        size: LogicalSize {
            width: visible.size.width,
            height: visible.size.height,
        },
    };
    Ok(place_popup(
        logical_cursor,
        popup,
        work_area,
        screen.backingScaleFactor(),
    ))
}

pub struct OutsideClickMonitor {
    monitors: Arc<Mutex<Vec<MonitorHandle>>>,
}

struct MonitorHandle(Retained<AnyObject>);

// SAFETY: AppKit global event monitors are opaque registration tokens. Apple documents
// `removeMonitor` for removing the token from event-monitor callbacks; ownership is serialized by
// the mutex and removal occurs at most once.
unsafe impl Send for MonitorHandle {}
// SAFETY: no operation dereferences or mutates the Objective-C object except the serialized,
// one-shot AppKit removal call.
unsafe impl Sync for MonitorHandle {}

impl std::fmt::Debug for OutsideClickMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OutsideClickMonitor").finish()
    }
}

impl OutsideClickMonitor {
    /// Installs a temporary global mouse-down monitor. Drop removes it immediately.
    ///
    /// # Errors
    /// Returns an integration error when `AppKit` cannot install the monitor.
    pub fn install(
        popup_bounds: LogicalRect,
        dismiss: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, PlatformError> {
        let monitor_slot: Arc<Mutex<Vec<MonitorHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let screen_height = MainThreadMarker::new()
            .and_then(NSScreen::mainScreen)
            .map(|screen| screen.frame().size.height)
            .ok_or(PlatformError::IntegrationFailure)?;
        let global_slot = Arc::clone(&monitor_slot);
        let global_dismiss = Arc::clone(&dismiss);
        let global_block = block2::RcBlock::new(move |_event| {
            let cursor = NSEvent::mouseLocation();
            let point = LogicalPoint {
                x: cursor.x,
                y: screen_height - cursor.y,
            };
            if !popup_bounds.contains(point) {
                remove_monitors(&global_slot);
                global_dismiss();
            }
        });
        let global_monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
            &global_block,
        )
        .ok_or(PlatformError::IntegrationFailure)?;
        let local_slot = Arc::clone(&monitor_slot);
        let local_block = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let cursor = NSEvent::mouseLocation();
            let point = LogicalPoint {
                x: cursor.x,
                y: screen_height - cursor.y,
            };
            if !popup_bounds.contains(point) {
                remove_monitors(&local_slot);
                dismiss();
            }
            event.as_ptr()
        });
        // SAFETY: the local monitor returns the same non-null NSEvent pointer supplied by AppKit.
        let local_monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
                &local_block,
            )
        }
        .ok_or_else(|| {
            // SAFETY: this is the token just returned by AppKit's global monitor API.
            unsafe { NSEvent::removeMonitor(&global_monitor) };
            PlatformError::IntegrationFailure
        })?;
        monitor_slot
            .lock()
            .map_err(|_| PlatformError::IntegrationFailure)?
            .extend([MonitorHandle(global_monitor), MonitorHandle(local_monitor)]);
        Ok(Self {
            monitors: monitor_slot,
        })
    }
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        remove_monitors(&self.monitors);
    }
}

fn remove_monitors(slot: &Mutex<Vec<MonitorHandle>>) {
    if let Ok(mut monitors) = slot.lock() {
        for monitor in monitors.drain(..) {
            // SAFETY: `MonitorHandle` contains the exact object returned by AppKit's installation API;
            // mutex-protected take guarantees this token is removed no more than once.
            unsafe { NSEvent::removeMonitor(&monitor.0) };
        }
    }
}

#[derive(Debug, Default)]
pub struct MacOsSelectionCapture {
    active: Arc<AtomicBool>,
}

impl SelectionCapture for MacOsSelectionCapture {
    fn capture_selected_text(
        &self,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, CaptureError>> + Send + '_>,
    > {
        let active = Arc::clone(&self.active);
        Box::pin(async move {
            if active.swap(true, Ordering::AcqRel) {
                return Err(CaptureError::Busy);
            }
            let result = tokio::task::spawn_blocking(move || capture_blocking(timeout))
                .await
                .map_err(|_| CaptureError::ClipboardUnavailable);
            active.store(false, Ordering::Release);
            result?
        })
    }
}

fn capture_blocking(timeout: Duration) -> Result<String, CaptureError> {
    let clipboard = ClipboardContext::new().map_err(|_| CaptureError::ClipboardUnavailable)?;
    let snapshot = PasteboardSnapshot::read(&clipboard)?;
    let marker = capture_marker();
    clipboard
        .set_text(marker.clone())
        .map_err(|_| CaptureError::ClipboardUnavailable)?;
    let guard = PasteboardRestoreGuard::new(clipboard, snapshot);
    send_copy_shortcut()?;

    let deadline = Instant::now() + timeout;
    loop {
        let current_change_count = guard.current_change_count();
        if current_change_count != guard.marker_change_count() {
            guard.mark_capture_change(current_change_count);
            let current = guard.current_text()?;
            let text = current.trim().to_owned();
            return if text.is_empty() {
                Err(CaptureError::NoSelection)
            } else {
                Ok(text)
            };
        }
        if Instant::now() >= deadline {
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(12));
    }
}

fn capture_marker() -> String {
    format!("LVOS-CAPTURE-{}", std::process::id())
}

fn send_copy_shortcut() -> Result<(), CaptureError> {
    const C_KEY_CODE: u16 = 8;
    if !CGPreflightPostEventAccess() {
        return Err(CaptureError::PermissionDenied);
    }
    // macOS virtual key code 8 is the physical C key. CoreGraphics avoids HIToolbox keyboard
    // layout queries, which are main-queue-only on macOS 15.
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .ok_or(CaptureError::InputInjectionFailed)?;
    let key_down = CGEvent::new_keyboard_event(Some(&source), C_KEY_CODE, true)
        .ok_or(CaptureError::InputInjectionFailed)?;
    let key_up = CGEvent::new_keyboard_event(Some(&source), C_KEY_CODE, false)
        .ok_or(CaptureError::InputInjectionFailed)?;
    CGEvent::set_flags(Some(&key_down), CGEventFlags::MaskCommand);
    CGEvent::set_flags(Some(&key_up), CGEventFlags::MaskCommand);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_down));
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_up));
    Ok(())
}

struct PasteboardSnapshot {
    contents: Vec<ClipboardContent>,
}

impl PasteboardSnapshot {
    fn read(clipboard: &ClipboardContext) -> Result<Self, CaptureError> {
        let mut formats = vec![
            ContentFormat::Text,
            ContentFormat::Rtf,
            ContentFormat::Html,
            ContentFormat::Image,
            ContentFormat::Files,
        ];
        formats.extend(
            clipboard
                .available_formats()
                .map_err(|_| CaptureError::ClipboardUnavailable)?
                .into_iter()
                .filter(|format| is_custom_pasteboard_format(format))
                .map(ContentFormat::Other),
        );
        let contents = clipboard
            .get(&formats)
            .map_err(|_| CaptureError::ClipboardUnavailable)?;
        Ok(Self { contents })
    }
}

fn is_custom_pasteboard_format(format: &str) -> bool {
    !format.starts_with("CorePasteboardFlavorType ")
        && !matches!(
            format,
            "public.utf8-plain-text"
                | "public.utf16-external-plain-text"
                | "public.rtf"
                | "public.html"
                | "public.png"
                | "public.tiff"
                | "public.file-url"
                | "NSStringPboardType"
        )
}

struct PasteboardRestoreGuard {
    clipboard: ClipboardContext,
    pasteboard: Retained<NSPasteboard>,
    snapshot: Mutex<Option<PasteboardSnapshot>>,
    expected_change_count: Mutex<isize>,
}

impl PasteboardRestoreGuard {
    fn new(clipboard: ClipboardContext, snapshot: PasteboardSnapshot) -> Self {
        let pasteboard = NSPasteboard::generalPasteboard();
        let expected_change_count = pasteboard.changeCount();
        Self {
            clipboard,
            pasteboard,
            snapshot: Mutex::new(Some(snapshot)),
            expected_change_count: Mutex::new(expected_change_count),
        }
    }

    fn current_text(&self) -> Result<String, CaptureError> {
        self.clipboard
            .get_text()
            .map_err(|_| CaptureError::ClipboardUnavailable)
    }

    fn current_change_count(&self) -> isize {
        self.pasteboard.changeCount()
    }

    fn marker_change_count(&self) -> isize {
        self.expected_change_count
            .lock()
            .map_or(-1, |expected| *expected)
    }

    fn mark_capture_change(&self, current_change_count: isize) {
        if let Ok(mut expected) = self.expected_change_count.lock()
            && is_single_capture_change(*expected, current_change_count)
        {
            *expected = current_change_count;
        }
    }
}

impl Drop for PasteboardRestoreGuard {
    fn drop(&mut self) {
        let expected = self.expected_change_count.lock().map(|value| *value).ok();
        let current = self.pasteboard.changeCount();
        let may_restore = expected.is_some_and(|value| should_restore_pasteboard(current, value));
        if !may_restore {
            tracing::debug!(
                current,
                ?expected,
                "preserved newer macOS pasteboard content"
            );
            return;
        }
        if let Ok(mut snapshot) = self.snapshot.lock()
            && let Some(snapshot) = snapshot.take()
        {
            let restored = if snapshot.contents.is_empty() {
                self.clipboard.clear()
            } else {
                self.clipboard.set(snapshot.contents)
            };
            if let Err(error) = restored {
                tracing::warn!(%error, "failed to restore macOS pasteboard snapshot");
            }
        }
    }
}

const fn should_restore_pasteboard(
    current_change_count: isize,
    expected_change_count: isize,
) -> bool {
    current_change_count == expected_change_count
}

const fn is_single_capture_change(marker_change_count: isize, current_change_count: isize) -> bool {
    current_change_count == marker_change_count.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{is_single_capture_change, parse_hotkey_display, should_restore_pasteboard};

    #[test]
    fn pasteboard_restores_when_capture_value_is_unchanged() {
        assert!(should_restore_pasteboard(42, 42));
    }

    #[test]
    fn pasteboard_preserves_a_user_copy_during_capture() {
        assert!(!should_restore_pasteboard(43, 42));
    }

    #[test]
    fn extra_pasteboard_change_is_not_claimed_as_the_injected_copy() {
        assert!(is_single_capture_change(40, 41));
        assert!(!is_single_capture_change(40, 42));
    }

    #[test]
    fn macos_hotkey_display_notation_is_normalized() {
        assert_eq!(parse_hotkey_display("⌥D").as_deref(), Ok("alt+KeyD"));
        assert_eq!(
            parse_hotkey_display("Option + D").as_deref(),
            Ok("alt+KeyD")
        );
        assert!(parse_hotkey_display("D").is_err());
    }

    #[test]
    fn standard_pasteboard_aliases_are_not_read_as_custom_uti_values() {
        assert!(!super::is_custom_pasteboard_format("NSStringPboardType"));
        assert!(!super::is_custom_pasteboard_format(
            "CorePasteboardFlavorType 0x75747874"
        ));
        assert!(!super::is_custom_pasteboard_format(
            "CorePasteboardFlavorType 0x54455854"
        ));
        assert!(super::is_custom_pasteboard_format(
            "com.example.application.private-data"
        ));
    }

    #[test]
    fn launch_agent_xml_escapes_executable_paths() {
        assert_eq!(
            super::escape_xml("/Applications/LVOS & Tools/'Test'.app"),
            "/Applications/LVOS &amp; Tools/&apos;Test&apos;.app"
        );
    }
}
