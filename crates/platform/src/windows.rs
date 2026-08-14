//! Windows 11 `x86_64` native platform adapters.
//!
//! Win32 and raw handle access is intentionally contained in this module. Selection capture copies
//! bounded, safely lockable `HGLOBAL` formats into LVOS-owned memory before changing clipboard
//! ownership, then restores those formats only while the clipboard sequence still belongs to LVOS.

#![allow(unsafe_code)]

use std::{
    cell::RefCell,
    mem::size_of,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use clipboard_rs::{Clipboard, ClipboardContext};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use lvos_auth::{AuthError, CredentialKey, CredentialScope, CredentialStore};
use notify_rust::Notification;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuId, MenuItem},
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, GlobalFree, HANDLE, HGLOBAL, LPARAM,
            LRESULT, WPARAM,
        },
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
                GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Ole::CF_UNICODETEXT,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
                RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW,
                RegOpenKeyExW, RegSetValueExW,
            },
            Threading::{
                CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentThreadId, OpenEventW,
                ReleaseMutex, SetEvent, WaitForMultipleObjects,
            },
        },
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, HWND_MESSAGE, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SW_SHOWNORMAL,
                SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL,
                WINDOW_EX_STYLE, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN,
                WM_XBUTTONDOWN, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{
    CaptureError, InstanceAcquisition, LogicalPoint, LogicalRect, LogicalSize, NotificationService,
    PlatformError, PopupPlacement, SelectionCapture, SingleInstanceGuard, SingleInstanceService,
    place_popup,
};

const CREDENTIAL_SERVICE: &str = "site.niuniu770.lvos";
const MUTEX_NAME: &str = "Local\\site.niuniu770.lvos.desktop";
const OPEN_EVENT_NAME: &str = "Local\\site.niuniu770.lvos.desktop.open";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "LVOS";

/// Opens one validated HTTPS page in the user's default browser.
///
/// # Errors
/// Returns an integration error for a non-HTTPS URL or when `ShellExecute` rejects the request.
pub fn open_web_url(value: &str) -> Result<(), PlatformError> {
    if !value.starts_with("https://") {
        return Err(PlatformError::IntegrationFailure);
    }
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    // SAFETY: every pointer is either null or points to a NUL-terminated immutable UTF-16 buffer
    // for the duration of this synchronous ShellExecuteW call. No returned handle is owned by LVOS.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(value.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

#[derive(Debug, Default)]
pub struct WindowsCredentialStore;

impl CredentialStore for WindowsCredentialStore {
    fn get(&self, scope: &CredentialScope) -> Result<Option<Vec<u8>>, AuthError> {
        let entry = credential_entry(scope)?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                tracing::warn!(%error, "Windows Credential Manager read failed");
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
                tracing::warn!(%error, "Windows Credential Manager write failed");
                AuthError::CredentialStore
            })
    }

    fn delete(&self, scope: &CredentialScope) -> Result<(), AuthError> {
        match credential_entry(scope)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "Windows Credential Manager delete failed");
                Err(AuthError::CredentialStore)
            }
        }
    }
}

fn credential_entry(scope: &CredentialScope) -> Result<keyring::Entry, AuthError> {
    keyring::Entry::new(CREDENTIAL_SERVICE, &credential_account(scope))
        .map_err(|_| AuthError::CredentialStore)
}

fn credential_account(scope: &CredentialScope) -> String {
    format!(
        "{}|{}|{}|{}",
        scope.server_origin,
        scope.user_id,
        scope.device_id,
        match scope.key {
            CredentialKey::RetiredTranslationApiKey => "google-api-key",
            CredentialKey::TencentTokenHubApiKey => "tokenhub-api-key",
            CredentialKey::ServerRefreshToken => "server-refresh-token",
        }
    )
}

#[derive(Debug, Default)]
pub struct WindowsNotificationService;

impl NotificationService for WindowsNotificationService {
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
        // A portable, unsigned executable has no installed Start Menu shortcut carrying LVOS's
        // AppUserModelID yet. `notify-rust` deliberately falls back to the registered PowerShell
        // notifier when no ID is supplied, so the error remains visible before Stage 13 packaging.
        .summary(title)
        .body(message)
        .show()
        .map(|_| ())
        .map_err(|_| PlatformError::IntegrationFailure)
}

pub struct WindowsHotKey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
    event_id: Arc<AtomicU32>,
}

impl std::fmt::Debug for WindowsHotKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsHotKey")
            .field("hotkey", &self.hotkey)
            .finish_non_exhaustive()
    }
}

impl WindowsHotKey {
    /// Registers a system-wide shortcut and reports ownership conflicts.
    ///
    /// # Errors
    /// Returns a conflict or integration error when registration fails.
    pub fn register(shortcut: &str) -> Result<Self, PlatformError> {
        let hotkey = parse_hotkey(shortcut)?;
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

    pub fn set_activation_handler(&self, handler: Arc<dyn Fn() + Send + Sync>) {
        let event_id = Arc::clone(&self.event_id);
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            tracing::debug!(event_id = event.id, state = ?event.state, "received Windows global hotkey event");
            if event.id == event_id.load(Ordering::Acquire) && event.state == HotKeyState::Released
            {
                handler();
            }
        }));
    }

    /// Replaces a shortcut while retaining the old registration if the replacement conflicts.
    ///
    /// # Errors
    /// Returns a conflict or integration error while leaving the previous shortcut active.
    pub fn update(&mut self, shortcut: &str) -> Result<(), PlatformError> {
        let replacement = parse_hotkey(shortcut)?;
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

impl Drop for WindowsHotKey {
    fn drop(&mut self) {
        let _ = self.manager.unregister(self.hotkey);
    }
}

/// Parses the user-facing Windows notation such as `Alt+D`.
///
/// # Errors
/// Returns an integration error unless the shortcut contains a modifier and one ASCII letter.
pub fn parse_hotkey_display(value: &str) -> Result<String, PlatformError> {
    let hotkey = parse_hotkey(value)?;
    Ok(hotkey.to_string())
}

fn parse_hotkey(value: &str) -> Result<HotKey, PlatformError> {
    let mut modifiers = Modifiers::empty();
    let mut key = None;
    let parts: Vec<_> = value.split('+').map(str::trim).collect();
    if parts.len() < 2 {
        return Err(PlatformError::IntegrationFailure);
    }
    for (index, part) in parts.iter().enumerate() {
        if index + 1 == parts.len() {
            let bytes = part.as_bytes();
            if bytes.len() != 1 || !bytes[0].is_ascii_alphabetic() {
                return Err(PlatformError::IntegrationFailure);
            }
            key = code_for_ascii_letter(bytes[0].to_ascii_uppercase());
        } else {
            match part.to_ascii_lowercase().as_str() {
                "alt" => modifiers |= Modifiers::ALT,
                "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
                "shift" => modifiers |= Modifiers::SHIFT,
                "win" | "super" => modifiers |= Modifiers::SUPER,
                _ => return Err(PlatformError::IntegrationFailure),
            }
        }
    }
    key.map(|key| HotKey::new(Some(modifiers), key))
        .ok_or(PlatformError::IntegrationFailure)
}

fn code_for_ascii_letter(letter: u8) -> Option<Code> {
    use Code::{
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    };
    Some(match letter {
        b'A' => KeyA,
        b'B' => KeyB,
        b'C' => KeyC,
        b'D' => KeyD,
        b'E' => KeyE,
        b'F' => KeyF,
        b'G' => KeyG,
        b'H' => KeyH,
        b'I' => KeyI,
        b'J' => KeyJ,
        b'K' => KeyK,
        b'L' => KeyL,
        b'M' => KeyM,
        b'N' => KeyN,
        b'O' => KeyO,
        b'P' => KeyP,
        b'Q' => KeyQ,
        b'R' => KeyR,
        b'S' => KeyS,
        b'T' => KeyT,
        b'U' => KeyU,
        b'V' => KeyV,
        b'W' => KeyW,
        b'X' => KeyX,
        b'Y' => KeyY,
        b'Z' => KeyZ,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TrayAction {
    OpenMainWindow,
    Quit,
}

pub struct WindowsTray {
    _icon: TrayIcon,
    open_id: MenuId,
    quit_id: MenuId,
}

impl std::fmt::Debug for WindowsTray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsTray")
            .finish_non_exhaustive()
    }
}

impl WindowsTray {
    /// Installs a Windows notification-area icon with open and quit actions.
    ///
    /// # Errors
    /// Returns an integration error if the shell rejects the icon or menu.
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
            .with_icon(tray_icon()?)
            .with_menu(Box::new(menu))
            .build()
            .map_err(|_| PlatformError::IntegrationFailure)?;
        Ok(Self {
            _icon: icon,
            open_id,
            quit_id,
        })
    }

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

fn tray_icon() -> Result<Icon, PlatformError> {
    const SIDE: u32 = 32;
    let mut pixels = vec![0_u8; (SIDE * SIDE * 4) as usize];
    for y in 4..28 {
        for x in 4..28 {
            let on = x == 4 || x == 27 || y == 4 || y == 27 || x == y || x + y == 31;
            if on {
                let offset = ((y * SIDE + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&[49, 46, 129, 255]);
            }
        }
    }
    Icon::from_rgba(pixels, SIDE, SIDE).map_err(|_| PlatformError::IntegrationFailure)
}

/// Samples the cursor and containing monitor once and returns a logical popup placement.
///
/// # Errors
/// Returns an integration error when Win32 cannot obtain the cursor, work area, or window DPI.
pub fn popup_placement(
    window: windows::Win32::Foundation::HWND,
    popup: LogicalSize,
) -> Result<PopupPlacement, PlatformError> {
    let mut cursor = windows::Win32::Foundation::POINT::default();
    // SAFETY: `cursor` is writable for the duration of the call.
    unsafe { GetCursorPos(&raw mut cursor) }.map_err(|_| PlatformError::IntegrationFailure)?;
    // SAFETY: cursor is a valid screen point and the nearest-monitor fallback always returns one.
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>())
            .map_err(|_| PlatformError::IntegrationFailure)?,
        ..Default::default()
    };
    // SAFETY: info has the documented size and remains writable.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(PlatformError::IntegrationFailure);
    }
    // SAFETY: the HWND belongs to the live Slint window.
    let dpi = unsafe { GetDpiForWindow(window) };
    let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 };
    let work = info.rcWork;
    Ok(place_popup(
        LogicalPoint {
            x: f64::from(cursor.x) / scale,
            y: f64::from(cursor.y) / scale,
        },
        popup,
        LogicalRect {
            origin: LogicalPoint {
                x: f64::from(work.left) / scale,
                y: f64::from(work.top) / scale,
            },
            size: LogicalSize {
                width: f64::from(work.right - work.left) / scale,
                height: f64::from(work.bottom - work.top) / scale,
            },
        },
        scale,
    ))
}

#[derive(Clone, Copy)]
struct HookBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl HookBounds {
    const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

struct HookState {
    bounds: HookBounds,
    dismiss: Arc<dyn Fn() + Send + Sync>,
    fired: AtomicBool,
}

thread_local! {
    // WH_MOUSE_LL callbacks execute on the thread that installed the hook. Keeping the callback
    // state on that same thread prevents an older monitor from clearing a newer monitor's state
    // while Popup content is replaced (for example, Loading -> Ready).
    static OUTSIDE_CLICK_STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

pub struct OutsideClickMonitor {
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for OutsideClickMonitor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutsideClickMonitor")
            .finish_non_exhaustive()
    }
}

impl OutsideClickMonitor {
    /// Installs a low-level mouse hook only for the lifetime of the visible Popup.
    ///
    /// # Errors
    /// Returns an integration error when the hook thread cannot be created or initialized.
    pub fn install(
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        dismiss: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, PlatformError> {
        let bounds = HookBounds {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("lvos-popup-dismiss".to_owned())
            .spawn(move || run_outside_click_hook(bounds, dismiss, &ready_tx))
            .map_err(|_| PlatformError::IntegrationFailure)?;
        let thread_id = ready_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| PlatformError::IntegrationFailure)??;
        Ok(Self {
            thread_id,
            thread: Some(thread),
        })
    }
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        // SAFETY: thread_id identifies the hook thread and WM_QUIT carries no pointers.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_outside_click_hook(
    bounds: HookBounds,
    dismiss: Arc<dyn Fn() + Send + Sync>,
    ready: &mpsc::SyncSender<Result<u32, PlatformError>>,
) {
    OUTSIDE_CLICK_STATE.with(|state| {
        state.borrow_mut().replace(HookState {
            bounds,
            dismiss,
            fired: AtomicBool::new(false),
        });
    });
    // SAFETY: null module selects the current process module; the callback has system ABI.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok();
    let hook =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module.map(Into::into), 0) };
    let Ok(hook) = hook else {
        let _ = ready.send(Err(PlatformError::IntegrationFailure));
        OUTSIDE_CLICK_STATE.with(|state| state.borrow_mut().take());
        return;
    };
    // SAFETY: called on the current thread and used only to post WM_QUIT later.
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Ok(thread_id));
    let mut message = MSG::default();
    // SAFETY: message storage remains writable for the loop; null HWND receives thread messages.
    while unsafe { GetMessageW(&raw mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    OUTSIDE_CLICK_STATE.with(|state| state.borrow_mut().take());
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && u32::try_from(wparam.0).is_ok_and(is_mouse_button_down) {
        // SAFETY: Win32 guarantees lparam points to MSLLHOOKSTRUCT for WH_MOUSE_LL callbacks.
        let mouse = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        OUTSIDE_CLICK_STATE.with(|active| {
            let state = active.borrow();
            if let Some(state) = state.as_ref()
                && !state.bounds.contains(mouse.pt.x, mouse.pt.y)
                && !state.fired.swap(true, Ordering::AcqRel)
            {
                (state.dismiss)();
            }
        });
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

const fn is_mouse_button_down(message: u32) -> bool {
    matches!(
        message,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    )
}

#[derive(Debug, Default)]
pub struct WindowsSingleInstanceService;

impl SingleInstanceService for WindowsSingleInstanceService {
    fn acquire(&self) -> Result<InstanceAcquisition, PlatformError> {
        let mutex_name = wide(MUTEX_NAME);
        // SAFETY: pointers are NUL-terminated for the call and no security attributes are passed.
        let mutex = unsafe { CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr())) }
            .map_err(|_| PlatformError::IntegrationFailure)?;
        // SAFETY: GetLastError is read immediately after CreateMutexW, as required by Win32.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            Ok(InstanceAcquisition::Existing(Box::new(
                WindowsInstanceGuard {
                    mutex: Some(mutex),
                    event: None,
                    stop: None,
                    listener: None,
                    existing: true,
                    handler: Arc::new(Mutex::new(None)),
                },
            )))
        } else {
            let event_name = wide(OPEN_EVENT_NAME);
            // SAFETY: the name is NUL-terminated and the unnamed security descriptor is valid.
            let event = unsafe { CreateEventW(None, false, false, PCWSTR(event_name.as_ptr())) }
                .map_err(|_| PlatformError::IntegrationFailure)?;
            let stop = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                .map_err(|_| PlatformError::IntegrationFailure)?;
            let handler = Arc::new(Mutex::new(None));
            let thread_handler = Arc::clone(&handler);
            let event_value = event.0 as usize;
            let stop_value = stop.0 as usize;
            let listener = std::thread::Builder::new()
                .name("lvos-instance-signal".to_owned())
                .spawn(move || {
                    listen_for_open_requests(
                        HANDLE(event_value as *mut _),
                        HANDLE(stop_value as *mut _),
                        &thread_handler,
                    );
                })
                .map_err(|_| PlatformError::IntegrationFailure)?;
            Ok(InstanceAcquisition::Primary(Box::new(
                WindowsInstanceGuard {
                    mutex: Some(mutex),
                    event: Some(event),
                    stop: Some(stop),
                    listener: Some(listener),
                    existing: false,
                    handler,
                },
            )))
        }
    }
}

type OpenHandler = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

struct WindowsInstanceGuard {
    mutex: Option<HANDLE>,
    event: Option<HANDLE>,
    stop: Option<HANDLE>,
    listener: Option<std::thread::JoinHandle<()>>,
    existing: bool,
    handler: OpenHandler,
}

// SAFETY: Win32 kernel handles may be used and closed from any process thread. The listener is
// joined before its handles are closed, and mutable Rust state remains mutex-protected.
unsafe impl Send for WindowsInstanceGuard {}

impl std::fmt::Debug for WindowsInstanceGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsInstanceGuard")
            .field("existing", &self.existing)
            .finish_non_exhaustive()
    }
}

impl SingleInstanceGuard for WindowsInstanceGuard {
    fn signal_existing(&self) -> Result<(), PlatformError> {
        if !self.existing {
            return Ok(());
        }
        let name = wide(OPEN_EVENT_NAME);
        // SAFETY: OpenEventW receives a valid NUL-terminated name and least-required access.
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) }
            .map_err(|_| PlatformError::IntegrationFailure)?;
        // SAFETY: event is a valid event handle returned immediately above.
        let result = unsafe { SetEvent(event) }.map_err(|_| PlatformError::IntegrationFailure);
        let _ = unsafe { CloseHandle(event) };
        result
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

impl Drop for WindowsInstanceGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.stop {
            let _ = unsafe { SetEvent(stop) };
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        if !self.existing
            && let Some(mutex) = self.mutex
        {
            // SAFETY: the primary created the mutex with initial ownership and releases it once.
            let _ = unsafe { ReleaseMutex(mutex) };
        }
        for handle in [self.event.take(), self.stop.take(), self.mutex.take()]
            .into_iter()
            .flatten()
        {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

fn listen_for_open_requests(event: HANDLE, stop: HANDLE, handler: &OpenHandler) {
    loop {
        // SAFETY: both handles remain owned by the guard until the listener joins.
        let result = unsafe { WaitForMultipleObjects(&[event, stop], false, u32::MAX) };
        if result.0 == 1 {
            break;
        }
        if result.0 == 0
            && let Ok(handler) = handler.lock()
            && let Some(handler) = handler.as_ref()
        {
            handler();
        }
    }
}

#[derive(Debug, Default)]
pub struct WindowsSelectionCapture {
    active: Arc<AtomicBool>,
}

impl SelectionCapture for WindowsSelectionCapture {
    fn capture_selected_text(
        &self,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, CaptureError>> + Send + '_>,
    > {
        let active = Arc::clone(&self.active);
        Box::pin(async move {
            if active.swap(true, Ordering::AcqRel) {
                tracing::debug!("rejected overlapping Windows selection capture");
                return Err(CaptureError::Busy);
            }
            tracing::debug!(
                timeout_ms = timeout.as_millis(),
                "starting blocking Windows selection capture"
            );
            let result = tokio::task::spawn_blocking(move || capture_blocking(timeout))
                .await
                .map_err(|_| CaptureError::ClipboardUnavailable);
            active.store(false, Ordering::Release);
            if let Err(error) = &result {
                tracing::warn!(%error, "Windows selection capture worker could not complete");
            }
            result?
        })
    }
}

fn capture_blocking(timeout: Duration) -> Result<String, CaptureError> {
    let started = Instant::now();
    let modifier_deadline = Instant::now() + Duration::from_secs(3);
    // WM_HOTKEY is delivered while the invoking chord can still be physically depressed. In
    // particular, sending Ctrl+C before Alt from the default Alt+D chord is released produces
    // Alt+Ctrl+C in the target application and no clipboard update. Microsoft documents that the
    // current keyboard state can interfere with SendInput, so wait before touching the clipboard.
    tracing::debug!("waiting for Windows hotkey modifiers to be released");
    wait_for_hotkey_modifiers_to_release(modifier_deadline).inspect_err(|error| {
        tracing::warn!(%error, elapsed_ms = started.elapsed().as_millis(), "timed out before all hotkey modifiers were released");
    })?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "Windows hotkey modifiers released"
    );
    tracing::debug!("copying original Windows clipboard into LVOS-owned memory");
    let snapshot = ClipboardSnapshot::capture()?;
    tracing::debug!(
        formats = snapshot.formats.len(),
        "copied original Windows clipboard snapshot"
    );
    let clipboard = ClipboardContext::new().map_err(|_| CaptureError::ClipboardUnavailable)?;
    clipboard
        .set_text(format!("LVOS-CAPTURE-{}", std::process::id()))
        .map_err(|_| CaptureError::ClipboardUnavailable)?;
    let marker_sequence = clipboard_sequence();
    tracing::debug!(
        marker_sequence,
        "installed Windows clipboard capture marker"
    );
    let mut guard = ClipboardRestoreGuard::new(snapshot, marker_sequence);
    send_copy_shortcut()?;
    tracing::debug!(marker_sequence, "sent Windows Ctrl+C input sequence");

    let deadline = Instant::now() + timeout;
    loop {
        let sequence = clipboard_sequence();
        if sequence != marker_sequence {
            tracing::debug!(
                marker_sequence,
                sequence,
                elapsed_ms = started.elapsed().as_millis(),
                "observed Windows clipboard sequence change"
            );
            let settled_sequence = wait_for_clipboard_to_settle(sequence, deadline);
            guard.mark_capture_sequence(settled_sequence);
            tracing::debug!("reading captured selection as Win32 CF_UNICODETEXT");
            let text = read_unicode_clipboard_text()?.trim().to_owned();
            tracing::debug!(text_bytes = text.len(), "read captured Win32 Unicode text");
            return if text.is_empty() {
                Err(CaptureError::NoSelection)
            } else {
                Ok(text)
            };
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                marker_sequence,
                current_sequence = sequence,
                elapsed_ms = started.elapsed().as_millis(),
                "timed out waiting for Ctrl+C to update the Windows clipboard"
            );
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(12));
    }
}

const MAX_CLIPBOARD_TEXT_BYTES: usize = 4 * 1024 * 1024;

fn read_unicode_clipboard_text() -> Result<String, CaptureError> {
    let _clipboard = ClipboardReadGuard::open()?;
    // SAFETY: the clipboard remains open through the copy and CF_UNICODETEXT returns an HGLOBAL.
    let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }
        .map_err(|_| CaptureError::NoSelection)?;
    let global = HGLOBAL(handle.0);
    // SAFETY: global is owned by the open clipboard. We validate its byte size before reading.
    let byte_len = unsafe { GlobalSize(global) };
    if byte_len < size_of::<u16>() || byte_len > MAX_CLIPBOARD_TEXT_BYTES {
        tracing::warn!(
            byte_len,
            "rejected invalid or oversized Win32 Unicode clipboard data"
        );
        return Err(CaptureError::ClipboardUnavailable);
    }
    // SAFETY: GlobalLock returns stable storage until the matching GlobalUnlock/CloseClipboard.
    let pointer = unsafe { GlobalLock(global) }.cast::<u16>();
    if pointer.is_null() {
        return Err(CaptureError::ClipboardUnavailable);
    }
    let _lock = GlobalMemoryLock(global);
    let code_units = byte_len / size_of::<u16>();
    // SAFETY: GlobalSize bounds the allocation and CF_UNICODETEXT is an array of UTF-16 units.
    let units = unsafe { std::slice::from_raw_parts(pointer, code_units) };
    let terminator = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(code_units);
    Ok(String::from_utf16_lossy(&units[..terminator]))
}

struct ClipboardReadGuard;

impl ClipboardReadGuard {
    fn open() -> Result<Self, CaptureError> {
        Self::open_for(windows::Win32::Foundation::HWND::default())
    }

    fn open_for(owner: windows::Win32::Foundation::HWND) -> Result<Self, CaptureError> {
        for _ in 0..10 {
            // SAFETY: a null handle is valid for reads; a supplied handle is a live owner window.
            let owner = if owner.0.is_null() { None } else { Some(owner) };
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(CaptureError::ClipboardUnavailable)
    }
}

impl Drop for ClipboardReadGuard {
    fn drop(&mut self) {
        // SAFETY: balances the successful OpenClipboard on this thread.
        if unsafe { CloseClipboard() }.is_err() {
            tracing::warn!("failed to close Windows clipboard after Unicode text read");
        }
    }
}

struct GlobalMemoryLock(HGLOBAL);

impl Drop for GlobalMemoryLock {
    fn drop(&mut self) {
        // SAFETY: balances the successful GlobalLock. A false return can mean the lock count became
        // zero, so GetLastError is not used as a failure signal here.
        let _ = unsafe { GlobalUnlock(self.0) };
    }
}

fn wait_for_clipboard_to_settle(initial: u32, deadline: Instant) -> u32 {
    let mut current = initial;
    let mut unchanged_since = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        let next = clipboard_sequence();
        if next != current {
            tracing::debug!(
                previous = current,
                next,
                "Windows clipboard copy burst continued"
            );
            current = next;
            unchanged_since = Instant::now();
        } else if unchanged_since.elapsed() >= Duration::from_millis(40) {
            break;
        }
    }
    tracing::debug!(
        settled_sequence = current,
        "Windows clipboard copy burst settled"
    );
    current
}

fn wait_for_hotkey_modifiers_to_release(deadline: Instant) -> Result<(), CaptureError> {
    const MODIFIERS: [VIRTUAL_KEY; 5] = [VK_MENU, VK_CONTROL, VK_SHIFT, VK_LWIN, VK_RWIN];
    while MODIFIERS.into_iter().any(key_is_down) {
        if Instant::now() >= deadline {
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

fn key_is_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: querying the asynchronous state of a documented virtual key has no pointer or
    // lifetime preconditions. The high bit indicates that the key is currently down.
    (unsafe { GetAsyncKeyState(i32::from(key.0)) }) < 0
}

struct ClipboardFormatSnapshot {
    format: u32,
    bytes: Vec<u8>,
}

struct ClipboardSnapshot {
    formats: Vec<ClipboardFormatSnapshot>,
}

impl ClipboardSnapshot {
    fn capture() -> Result<Self, CaptureError> {
        const MAX_FORMAT_BYTES: usize = 32 * 1024 * 1024;
        const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
        let _clipboard = ClipboardReadGuard::open()?;
        let mut formats = Vec::new();
        let mut format = 0_u32;
        let mut total = 0_usize;
        loop {
            // SAFETY: the clipboard is open and passing the previous result enumerates formats.
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            // SAFETY: the clipboard is open; requesting a delayed format asks its live owner to
            // render it now. The returned handle remains clipboard-owned.
            let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
                tracing::debug!(format, "skipped unavailable Windows clipboard format");
                continue;
            };
            let global = HGLOBAL(handle.0);
            // SAFETY: HGLOBAL-compatible formats report their allocation size. Other handle types
            // report zero and are skipped rather than interpreted as bytes.
            let size = unsafe { GlobalSize(global) };
            if size == 0 || size > MAX_FORMAT_BYTES || total.saturating_add(size) > MAX_TOTAL_BYTES
            {
                tracing::debug!(
                    format,
                    size,
                    "skipped non-memory or oversized clipboard format"
                );
                continue;
            }
            // SAFETY: global remains owned by the open clipboard and is copied before close.
            let pointer = unsafe { GlobalLock(global) }.cast::<u8>();
            if pointer.is_null() {
                tracing::debug!(format, size, "could not lock Windows clipboard format");
                continue;
            }
            let _lock = GlobalMemoryLock(global);
            // SAFETY: GlobalSize bounds the readable allocation while its lock is held.
            let bytes = unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec();
            total += size;
            formats.push(ClipboardFormatSnapshot { format, bytes });
        }
        Ok(Self { formats })
    }

    fn restore(self) -> Result<(), CaptureError> {
        let owner = ClipboardOwnerWindow::create()?;
        let _clipboard = ClipboardReadGuard::open_for(owner.0)?;
        // SAFETY: owner is a valid live window for this open clipboard operation.
        unsafe { EmptyClipboard() }.map_err(|_| CaptureError::ClipboardUnavailable)?;
        for entry in self.formats {
            restore_clipboard_format(&entry);
        }
        Ok(())
    }
}

fn restore_clipboard_format(entry: &ClipboardFormatSnapshot) {
    // SAFETY: allocating movable global memory is required by SetClipboardData.
    let Ok(global) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, entry.bytes.len()) }) else {
        tracing::warn!(
            format = entry.format,
            "could not allocate restored clipboard format"
        );
        return;
    };
    // SAFETY: newly allocated memory is exclusively owned here.
    let pointer = unsafe { GlobalLock(global) }.cast::<u8>();
    if pointer.is_null() {
        // SAFETY: allocation ownership has not been transferred to the clipboard.
        let _ = unsafe { GlobalFree(Some(global)) };
        return;
    }
    // SAFETY: destination allocation exactly matches the source length and does not overlap.
    unsafe { std::ptr::copy_nonoverlapping(entry.bytes.as_ptr(), pointer, entry.bytes.len()) };
    let _ = unsafe { GlobalUnlock(global) };
    // SAFETY: the clipboard is open and empty; success transfers allocation ownership to Windows.
    if unsafe { SetClipboardData(entry.format, Some(HANDLE(global.0))) }.is_err() {
        tracing::warn!(
            format = entry.format,
            "could not restore Windows clipboard format"
        );
        // SAFETY: failed SetClipboardData leaves ownership with LVOS.
        let _ = unsafe { GlobalFree(Some(global)) };
    }
}

struct ClipboardOwnerWindow(windows::Win32::Foundation::HWND);

impl ClipboardOwnerWindow {
    fn create() -> Result<Self, CaptureError> {
        let class = wide("STATIC");
        let title = wide("LVOS Clipboard Owner");
        // SAFETY: STATIC is a built-in class and HWND_MESSAGE creates an invisible message window.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map_err(|_| CaptureError::ClipboardUnavailable)?;
        Ok(Self(window))
    }
}

impl Drop for ClipboardOwnerWindow {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the hidden window on its creating thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

struct ClipboardRestoreGuard {
    snapshot: Option<ClipboardSnapshot>,
    expected_sequence: u32,
}

impl ClipboardRestoreGuard {
    fn new(snapshot: ClipboardSnapshot, expected_sequence: u32) -> Self {
        Self {
            snapshot: Some(snapshot),
            expected_sequence,
        }
    }

    fn mark_capture_sequence(&mut self, sequence: u32) {
        if sequence != self.expected_sequence {
            self.expected_sequence = sequence;
        }
    }
}

impl Drop for ClipboardRestoreGuard {
    fn drop(&mut self) {
        let current = clipboard_sequence();
        if !should_restore_clipboard(current, self.expected_sequence) {
            tracing::debug!(
                current,
                expected = self.expected_sequence,
                "preserved newer Windows clipboard content"
            );
            return;
        }
        if let Some(snapshot) = self.snapshot.take() {
            tracing::debug!("restoring LVOS-owned Windows clipboard snapshot");
            if snapshot.restore().is_err() {
                tracing::warn!("failed to restore LVOS-owned Windows clipboard snapshot");
            } else {
                tracing::debug!("restored LVOS-owned Windows clipboard snapshot");
            }
        }
    }
}

fn clipboard_sequence() -> u32 {
    // SAFETY: GetClipboardSequenceNumber has no pointer or thread-affinity preconditions.
    unsafe { GetClipboardSequenceNumber() }
}

fn send_copy_shortcut() -> Result<(), CaptureError> {
    let key = |virtual_key, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [
        key(
            VK_CONTROL,
            windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS::default(),
        ),
        key(
            VK_C,
            windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS::default(),
        ),
        key(VK_C, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    // SAFETY: INPUT is initialized as keyboard input and the slice remains valid for the call.
    let input_size =
        i32::try_from(size_of::<INPUT>()).map_err(|_| CaptureError::InputInjectionFailed)?;
    let expected = u32::try_from(inputs.len()).map_err(|_| CaptureError::InputInjectionFailed)?;
    let inserted = unsafe { SendInput(&inputs, input_size) };
    tracing::debug!(inserted, expected, "Windows SendInput completed");
    if inserted == expected {
        Ok(())
    } else {
        Err(CaptureError::InputInjectionFailed)
    }
}

const fn should_restore_clipboard(current: u32, expected: u32) -> bool {
    current == expected
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns whether LVOS is registered in the current user's Startup `Run` key.
#[must_use]
pub fn start_at_login_enabled() -> bool {
    let Ok(key) = open_registry_key(RUN_KEY, KEY_READ) else {
        return false;
    };
    let value = wide(RUN_VALUE);
    let mut size = 0_u32;
    // SAFETY: the key is valid and this first call requests only the required byte count.
    let status = unsafe {
        RegGetValueW(
            key.0,
            PCWSTR::null(),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&raw mut size),
        )
    };
    status.is_ok() && size >= 2
}

/// Adds or removes the current executable from the current user's Startup `Run` key.
///
/// # Errors
/// Returns an integration error when the registry or executable path is unavailable.
pub fn set_start_at_login(enabled: bool) -> Result<(), PlatformError> {
    let key = create_registry_key(RUN_KEY)?;
    let value = wide(RUN_VALUE);
    let status = if enabled {
        let executable = std::env::current_exe().map_err(|_| PlatformError::IntegrationFailure)?;
        let command = format!("\"{}\"", executable.to_string_lossy());
        let command = wide(&command);
        // SAFETY: UTF-16 data includes a terminating NUL and is reinterpreted as bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                command.as_ptr().cast::<u8>(),
                command.len() * size_of::<u16>(),
            )
        };
        unsafe { RegSetValueExW(key.0, PCWSTR(value.as_ptr()), None, REG_SZ, Some(bytes)) }
    } else {
        // SAFETY: key and value name are valid; deleting a missing value is treated as success.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(value.as_ptr())) };
        if status.0 == 2 {
            windows::Win32::Foundation::ERROR_SUCCESS
        } else {
            status
        }
    };
    if status.is_ok() {
        Ok(())
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: the key is owned by this wrapper and closed exactly once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn open_registry_key(
    path: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<RegistryKey, PlatformError> {
    let path = wide(path);
    let mut key = HKEY::default();
    // SAFETY: path is NUL-terminated and key remains writable.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            access,
            &raw mut key,
        )
    };
    if status.is_ok() {
        Ok(RegistryKey(key))
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

fn create_registry_key(path: &str) -> Result<RegistryKey, PlatformError> {
    let path = wide(path);
    let mut key = HKEY::default();
    // SAFETY: path/class are NUL-terminated, no custom security descriptor is supplied, and the
    // returned current-user key is wrapped immediately for deterministic close.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &raw mut key,
            None,
        )
    };
    if status.is_ok() {
        Ok(RegistryKey(key))
    } else {
        Err(PlatformError::IntegrationFailure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hotkey_display_is_normalized() {
        assert!(matches!(
            parse_hotkey_display("Alt+D"),
            Ok(value) if value == "alt+KeyD"
        ));
        assert!(matches!(
            parse_hotkey_display("Ctrl + Shift + Q"),
            Ok(value) if value == "shift+control+KeyQ"
        ));
        assert!(parse_hotkey_display("D").is_err());
        assert!(parse_hotkey_display("Alt+F12").is_err());
    }

    #[test]
    fn clipboard_restore_requires_the_expected_sequence() {
        assert!(should_restore_clipboard(77, 77));
        assert!(!should_restore_clipboard(78, 77));
    }

    #[test]
    fn popup_hook_bounds_use_win32_exclusive_right_and_bottom_edges() {
        let bounds = HookBounds {
            left: 10,
            top: 20,
            right: 30,
            bottom: 40,
        };
        assert!(bounds.contains(10, 20));
        assert!(bounds.contains(29, 39));
        assert!(!bounds.contains(30, 39));
        assert!(!bounds.contains(29, 40));
    }

    #[test]
    fn popup_hook_recognizes_every_supported_button_down() {
        assert!(is_mouse_button_down(WM_LBUTTONDOWN));
        assert!(is_mouse_button_down(WM_RBUTTONDOWN));
        assert!(is_mouse_button_down(WM_MBUTTONDOWN));
        assert!(is_mouse_button_down(WM_XBUTTONDOWN));
        assert!(!is_mouse_button_down(
            windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE
        ));
    }
}
