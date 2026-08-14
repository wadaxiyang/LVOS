#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::error::Error;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "windows")]
use std::{cell::RefCell, path::Path, rc::Rc};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{path::PathBuf, sync::Arc};

use lvos::{DesktopRuntime, SlintUiDispatcher, UiController};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use lvos::{
    GitHubUpdateConfig, GitHubUpdateService, HttpUpdateTransport, NativeReleasePageOpener,
    UpdateCheckOutcome, UpdateCoordinator,
};
use lvos_core::{DEFAULT_UPDATE_CHANNEL, PRODUCT_NAME, SOFTWARE_VERSION};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use slint::ComponentHandle;

#[cfg(target_os = "windows")]
use lvos_platform::{
    InstanceAcquisition, NotificationService, SelectionCapture, SingleInstanceService,
    windows::{
        TrayAction as WindowsTrayAction, WindowsHotKey, WindowsNotificationService,
        WindowsSelectionCapture, WindowsSingleInstanceService, WindowsTray,
    },
};
#[cfg(target_os = "macos")]
use lvos_platform::{
    InstanceAcquisition, SingleInstanceService,
    macos::{
        MacOsHotKey, MacOsNotificationService, MacOsSingleInstanceService, MacOsTray, TrayAction,
    },
};
#[cfg(target_os = "macos")]
use lvos_platform::{NotificationService, SelectionCapture};

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "macos")]
    wait_for_restart_predecessor();
    #[cfg(target_os = "windows")]
    let log_path = init_windows_tracing()?;
    #[cfg(not(target_os = "windows"))]
    init_tracing();
    tracing::info!(
        version = SOFTWARE_VERSION,
        process_id = std::process::id(),
        "{PRODUCT_NAME} starting"
    );
    #[cfg(target_os = "windows")]
    tracing::info!(path = %log_path.display(), "persistent Windows log initialized");
    #[cfg(target_os = "macos")]
    let instance = acquire_macos_instance()?;
    #[cfg(target_os = "windows")]
    let instance = acquire_windows_instance()?;
    let runtime = DesktopRuntime::new(SlintUiDispatcher);
    let ui = UiController::new()?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    install_update_runtime(&ui, &runtime)?;
    #[cfg(target_os = "macos")]
    let native = install_macos_runtime(&ui, &runtime, instance)?;
    #[cfg(target_os = "windows")]
    let native = install_windows_runtime(&ui, &runtime, instance, &log_path)?;
    #[cfg(target_os = "macos")]
    if !load_boolean_preference("launch-minimized") {
        ui.show_main_window()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    ui.show_main_window()?;
    #[cfg(target_os = "windows")]
    if !load_boolean_preference("launch-minimized") {
        ui.show_main_window()?;
    }
    #[cfg(target_os = "macos")]
    show_accessibility_ui_if_needed(&ui)?;
    slint::run_event_loop_until_quit()?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    drop(native);
    runtime.shutdown();
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn install_update_runtime(
    ui: &UiController,
    runtime: &DesktopRuntime<SlintUiDispatcher>,
) -> Result<(), Box<dyn Error>> {
    let channel =
        std::env::var("LVOS_UPDATE_CHANNEL").unwrap_or_else(|_| DEFAULT_UPDATE_CHANNEL.to_owned());
    let config = GitHubUpdateConfig::lvos(channel.clone())?;
    let service = Arc::new(GitHubUpdateService::new(
        HttpUpdateTransport::new()?,
        config,
    ));
    let coordinator = Arc::new(UpdateCoordinator::new(
        service,
        Arc::new(NativeReleasePageOpener),
        &application_data_root(),
    ));
    ui.main_window()
        .set_update_status(format!("Current {SOFTWARE_VERSION} · {channel} · Not checked").into());

    let manual_main = ui.main_window().as_weak();
    let manual_coordinator = Arc::clone(&coordinator);
    let manual_runtime = runtime.runtime_handle();
    ui.main_window().on_check_update_requested(move || {
        if let Some(main) = manual_main.upgrade() {
            main.set_update_status("Checking GitHub Releases…".into());
        }
        let main = manual_main.clone();
        let coordinator = Arc::clone(&manual_coordinator);
        let channel = channel.clone();
        manual_runtime.spawn(async move {
            let result = coordinator.manual_check(current_unix_timestamp()).await;
            let status = match result {
                Ok(UpdateCheckOutcome::Available(info)) => {
                    if let Err(error) = coordinator.open_available(&info) {
                        tracing::warn!(%error, "failed to open update Release page");
                        format!(
                            "Version {} is available, but the Release page could not be opened.",
                            info.version
                        )
                    } else {
                        format!(
                            "Version {} is available. GitHub Releases opened for manual download.",
                            info.version
                        )
                    }
                }
                Ok(UpdateCheckOutcome::UpToDate(info)) => {
                    format!(
                        "Current {} · {} · Up to date",
                        info.current_version, info.channel
                    )
                }
                Ok(UpdateCheckOutcome::Skipped) => {
                    format!("Current {SOFTWARE_VERSION} · {channel} · Check skipped")
                }
                Err(error) => {
                    tracing::warn!(%error, "manual update check failed");
                    "Update check failed. Try again later.".to_owned()
                }
            };
            if let Err(error) = slint::invoke_from_event_loop(move || {
                if let Some(main) = main.upgrade() {
                    main.set_update_status(status.into());
                }
            }) {
                tracing::warn!(%error, "failed to dispatch manual update status");
            }
        });
    });

    let startup_main = ui.main_window().as_weak();
    runtime.spawn(async move {
        let result = coordinator.startup_check(current_unix_timestamp()).await;
        let status = match result {
            Ok(UpdateCheckOutcome::Available(info)) => Some(format!(
                "Version {} is available. Click Check for Updates to open GitHub Releases.",
                info.version
            )),
            Ok(UpdateCheckOutcome::UpToDate(info)) => Some(format!(
                "Current {} · {} · Up to date",
                info.current_version, info.channel
            )),
            Ok(UpdateCheckOutcome::Skipped) => None,
            Err(error) => {
                tracing::warn!(%error, "startup update check failed");
                Some("Automatic update check failed. Manual retry is available.".to_owned())
            }
        };
        if let Some(status) = status
            && let Err(error) = slint::invoke_from_event_loop(move || {
                if let Some(main) = startup_main.upgrade() {
                    main.set_update_status(status.into());
                }
            })
        {
            tracing::warn!(%error, "failed to dispatch startup update status");
        }
    });
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn current_unix_timestamp() -> lvos_core::UnixTimestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    lvos_core::UnixTimestamp::from_seconds(seconds)
}

#[cfg(target_os = "windows")]
fn acquire_windows_instance() -> Result<Box<dyn lvos_platform::SingleInstanceGuard>, Box<dyn Error>>
{
    match WindowsSingleInstanceService.acquire()? {
        InstanceAcquisition::Primary(guard) => Ok(guard),
        InstanceAcquisition::Existing(guard) => {
            guard.signal_existing()?;
            std::process::exit(0);
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_lines)]
fn install_windows_runtime(
    ui: &UiController,
    runtime: &DesktopRuntime<SlintUiDispatcher>,
    instance: Box<dyn lvos_platform::SingleInstanceGuard>,
    log_path: &Path,
) -> Result<WindowsRuntime, Box<dyn Error>> {
    let main = ui.main_window().as_weak();
    instance.set_open_handler(Arc::new(move || {
        let main = main.clone();
        if let Err(error) = slint::invoke_from_event_loop(move || {
            if let Some(main) = main.upgrade()
                && let Err(error) = main.show()
            {
                tracing::warn!(%error, "failed to open Main Window from second instance");
            }
        }) {
            tracing::warn!(%error, "failed to dispatch second-instance activation");
        }
    }))?;

    let tray = WindowsTray::install()?;
    let launch_minimized = load_boolean_preference("launch-minimized");
    ui.main_window().set_launch_minimized(launch_minimized);
    ui.main_window().on_update_launch_minimized(move |enabled| {
        if save_boolean_preference("launch-minimized", enabled).is_ok() {
            "".into()
        } else {
            "The launch preference could not be saved.".into()
        }
    });
    ui.main_window()
        .set_start_at_login(lvos_platform::windows::start_at_login_enabled());
    ui.main_window().on_update_start_at_login(move |enabled| {
        match lvos_platform::windows::set_start_at_login(enabled) {
            Ok(()) => "".into(),
            Err(_) => "Windows could not update the current-user startup registration.".into(),
        }
    });

    let hotkey_display = load_windows_hotkey();
    ui.main_window()
        .set_global_hotkey(hotkey_display.clone().into());
    let hotkey = WindowsHotKey::register(&hotkey_display).inspect_err(|_error| {
        let _ = WindowsNotificationService.error(
            "The default Alt+D shortcut is unavailable. Choose another shortcut in Settings.",
        );
    })?;
    let popup = ui.popup().as_weak();
    let async_runtime = runtime.runtime_handle();
    let capture = Arc::new(WindowsSelectionCapture::default());
    let capture_log_path = log_path.to_path_buf();
    hotkey.set_activation_handler(Arc::new(move || {
        tracing::info!("Windows global hotkey released; scheduling selection capture");
        let popup = popup.clone();
        let capture = Arc::clone(&capture);
        let capture_log_path = capture_log_path.clone();
        async_runtime.spawn(async move {
            tracing::info!(timeout_ms = 800_u64, "Windows selection capture task started");
            match capture.capture_selected_text(std::time::Duration::from_millis(800)).await {
                Ok(source) => {
                    tracing::info!(selected_text_bytes = source.len(), "Windows selection capture completed");
                    if let Err(error) = slint::invoke_from_event_loop(move || {
                        if let Some(popup) = popup.upgrade()
                            && let Err(error) = lvos::show_captured_provider_error(&popup, &source)
                        {
                            tracing::warn!(%error, "failed to show captured Lookup Card");
                        }
                    }) {
                        tracing::warn!(%error, "failed to dispatch captured selection");
                    }
                }
                Err(lvos_platform::CaptureError::Busy) => {}
                Err(error) => {
                    tracing::warn!(%error, "Windows selection capture failed");
                    let detail = if error == lvos_platform::CaptureError::InputInjectionFailed {
                        "LVOS could not send Ctrl+C. Elevated applications cannot be captured from a non-elevated LVOS process."
                            .to_owned()
                    } else {
                        error.to_string()
                    };
                    let message = format!(
                        "{detail}\nDiagnostic log: {}",
                        capture_log_path.display()
                    );
                    let _ = WindowsNotificationService.error(&message);
                }
            }
        });
    }));
    // The Win32 registration owns a hidden HWND and must remain on the Slint UI thread.
    let hotkey = Rc::new(RefCell::new(hotkey));
    let settings_hotkey = Rc::clone(&hotkey);
    ui.main_window().on_update_global_hotkey(move |display| {
        if lvos_platform::windows::parse_hotkey_display(display.as_str()).is_err() {
            return "Use a modifier and one letter, for example Alt+D.".into();
        }
        let mut hotkey = settings_hotkey.borrow_mut();
        match hotkey.update(display.as_str()) {
            Ok(()) => match save_windows_hotkey(display.as_str()) {
                Ok(()) => "".into(),
                Err(()) => "The hotkey changed but its preference could not be saved.".into(),
            },
            Err(lvos_platform::PlatformError::Conflict) => {
                "That shortcut is already in use. The previous hotkey remains active.".into()
            }
            Err(_) => {
                "The shortcut could not be registered. The previous hotkey remains active.".into()
            }
        }
    });

    let main = ui.main_window().as_weak();
    tray.set_action_handler(Arc::new(move |action| {
        let main = main.clone();
        if let Err(error) = slint::invoke_from_event_loop(move || match action {
            WindowsTrayAction::OpenMainWindow => {
                if let Some(main) = main.upgrade()
                    && let Err(error) = main.show()
                {
                    tracing::warn!(%error, "failed to open Main Window from tray");
                }
            }
            WindowsTrayAction::Quit => {
                if let Err(error) = slint::quit_event_loop() {
                    tracing::warn!(%error, "failed to quit Desktop event loop");
                }
            }
        }) {
            tracing::warn!(%error, "failed to dispatch tray event");
        }
    }));

    Ok(WindowsRuntime {
        _instance: instance,
        _tray: tray,
        _hotkey: hotkey,
    })
}

#[cfg(target_os = "windows")]
struct WindowsRuntime {
    _instance: Box<dyn lvos_platform::SingleInstanceGuard>,
    _tray: WindowsTray,
    _hotkey: Rc<RefCell<WindowsHotKey>>,
}

#[cfg(target_os = "macos")]
fn acquire_macos_instance() -> Result<Box<dyn lvos_platform::SingleInstanceGuard>, Box<dyn Error>> {
    let service = MacOsSingleInstanceService::new(&application_data_root());
    match service.acquire()? {
        InstanceAcquisition::Primary(guard) => Ok(guard),
        InstanceAcquisition::Existing(guard) => {
            guard.signal_existing()?;
            std::process::exit(0);
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_lines)]
fn install_macos_runtime(
    ui: &UiController,
    runtime: &DesktopRuntime<SlintUiDispatcher>,
    instance: Box<dyn lvos_platform::SingleInstanceGuard>,
) -> Result<MacOsRuntime, Box<dyn Error>> {
    let main = ui.main_window().as_weak();
    instance.set_open_handler(Arc::new(move || {
        let main = main.clone();
        if let Err(error) = slint::invoke_from_event_loop(move || {
            if let Some(main) = main.upgrade()
                && let Err(error) = main.show()
            {
                tracing::warn!(%error, "failed to open Main Window from second instance");
            }
        }) {
            tracing::warn!(%error, "failed to dispatch second-instance activation");
        }
    }))?;

    let tray = MacOsTray::install()?;
    install_accessibility_ui(ui);
    ui.main_window()
        .set_start_at_login(lvos_platform::macos::start_at_login_enabled());
    ui.main_window().on_update_start_at_login(move |enabled| {
        match lvos_platform::macos::set_start_at_login(enabled) {
            Ok(()) => "".into(),
            Err(lvos_platform::PlatformError::PermissionDenied) => {
                "Allow LVOS under System Settings > General > Login Items.".into()
            }
            Err(_) => "Start at login is available only from the packaged LVOS app.".into(),
        }
    });
    let launch_minimized = load_boolean_preference("launch-minimized");
    ui.main_window().set_launch_minimized(launch_minimized);
    ui.main_window().on_update_launch_minimized(move |enabled| {
        if save_boolean_preference("launch-minimized", enabled).is_ok() {
            "".into()
        } else {
            "The launch preference could not be saved.".into()
        }
    });
    let hotkey_display = load_macos_hotkey();
    ui.main_window()
        .set_global_hotkey(hotkey_display.as_str().into());
    let hotkey_registration = lvos_platform::macos::parse_hotkey_display(&hotkey_display)?;
    let hotkey = MacOsHotKey::register(&hotkey_registration).inspect_err(|_error| {
        let _ = MacOsNotificationService.error(
            "The default Option+D shortcut is unavailable. Close the conflicting application and restart LVOS.",
        );
    })?;
    let popup = ui.popup().as_weak();
    let permission = ui.permission_window().as_weak();
    let async_runtime = runtime.runtime_handle();
    let capture = Arc::new(lvos_platform::macos::MacOsSelectionCapture::default());
    hotkey.set_pressed_handler(Arc::new(move || {
        let popup = popup.clone();
        let permission = permission.clone();
        let capture = Arc::clone(&capture);
        async_runtime.spawn(async move {
            match capture
                .capture_selected_text(std::time::Duration::from_millis(800))
                .await
            {
                Ok(source) => {
                    if let Err(error) = slint::invoke_from_event_loop(move || {
                        if let Some(popup) = popup.upgrade()
                            && let Err(error) = lvos::show_captured_provider_error(&popup, &source)
                        {
                            tracing::warn!(%error, "failed to show captured Lookup Card");
                        }
                    }) {
                        tracing::warn!(%error, "failed to dispatch captured selection");
                    }
                }
                Err(lvos_platform::CaptureError::Busy) => {}
                Err(lvos_platform::CaptureError::PermissionDenied) => {
                    let permission = permission.clone();
                    if let Err(error) = slint::invoke_from_event_loop(move || {
                        if let Some(permission) = permission.upgrade() {
                            permission.set_status_text(
                                "Permission is still disabled. Enable LVOS in System Settings, then click Check Again.".into(),
                            );
                            if let Err(error) = lvos::show_permission_window(&permission) {
                                tracing::warn!(%error, "failed to show permission window");
                            }
                        }
                    }) {
                        tracing::warn!(%error, "failed to dispatch permission window");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "selection capture failed");
                    let _ = MacOsNotificationService.error(&error.to_string());
                }
            }
        });
    }));
    let hotkey = Arc::new(Mutex::new(hotkey));
    let settings_hotkey = Arc::clone(&hotkey);
    ui.main_window().on_update_global_hotkey(move |display| {
        let Ok(registration) = lvos_platform::macos::parse_hotkey_display(display.as_str()) else {
            return "Use a modifier and one letter, for example ⌥D.".into();
        };
        let Ok(mut hotkey) = settings_hotkey.lock() else {
            return "The global hotkey service is unavailable.".into();
        };
        match hotkey.update(&registration) {
            Ok(()) => match save_macos_hotkey(display.as_str()) {
                Ok(()) => "".into(),
                Err(()) => "The hotkey changed but its preference could not be saved.".into(),
            },
            Err(lvos_platform::PlatformError::Conflict) => {
                "That shortcut is already in use. The previous hotkey remains active.".into()
            }
            Err(_) => {
                "The shortcut could not be registered. The previous hotkey remains active.".into()
            }
        }
    });
    let main = ui.main_window().as_weak();
    tray.set_action_handler(Arc::new(move |action| {
        let main = main.clone();
        if let Err(error) = slint::invoke_from_event_loop(move || match action {
            TrayAction::OpenMainWindow => {
                if let Some(main) = main.upgrade()
                    && let Err(error) = main.show()
                {
                    tracing::warn!(%error, "failed to open Main Window from menu bar");
                }
            }
            TrayAction::Quit => {
                if let Err(error) = slint::quit_event_loop() {
                    tracing::warn!(%error, "failed to quit Desktop event loop");
                }
            }
        }) {
            tracing::warn!(%error, "failed to dispatch menu-bar event");
        }
    }));
    Ok(MacOsRuntime {
        _instance: instance,
        _tray: tray,
        _hotkey: hotkey,
    })
}

#[cfg(target_os = "macos")]
fn install_accessibility_ui(ui: &UiController) {
    let permission = ui.permission_window().as_weak();
    let settings_permission = permission.clone();
    ui.permission_window().on_open_settings(move || {
        if let Err(error) = lvos_platform::macos::open_accessibility_settings() {
            tracing::warn!(%error, "failed to open Accessibility Settings");
            if let Some(permission) = settings_permission.upgrade() {
                permission.set_status_text(
                    "System Settings could not be opened. Open Privacy & Security > Accessibility manually."
                        .into(),
                );
            }
        }
    });
    let check_permission = permission.clone();
    ui.permission_window().on_check_again(move || {
        if let Some(permission) = check_permission.upgrade() {
            if lvos_platform::macos::accessibility_permission_granted() {
                permission.set_status_text("Permission granted. LVOS is ready.".into());
                if let Err(error) = permission.hide() {
                    tracing::warn!(%error, "failed to hide permission window");
                }
            } else {
                permission.set_status_text(
                    "Permission changes may require a restart. Enable LVOS, then click Restart LVOS."
                        .into(),
                );
            }
        }
    });
    ui.permission_window().on_restart_requested(move || {
        if let Err(error) = restart_lvos() {
            tracing::warn!(%error, "failed to restart LVOS");
        }
    });
    if !lvos_platform::macos::accessibility_permission_granted() {
        let _ = lvos_platform::macos::request_accessibility_permission();
    }
}

#[cfg(target_os = "macos")]
fn show_accessibility_ui_if_needed(ui: &UiController) -> Result<(), Box<dyn Error>> {
    if !lvos_platform::macos::accessibility_permission_granted() {
        lvos::show_permission_window(ui.permission_window())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn restart_lvos() -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    std::process::Command::new(executable)
        .env("LVOS_RESTART_PREDECESSOR", std::process::id().to_string())
        .spawn()?;
    slint::quit_event_loop()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn wait_for_restart_predecessor() {
    if std::env::var_os("LVOS_RESTART_PREDECESSOR").is_some() {
        std::thread::sleep(std::time::Duration::from_millis(800));
    }
}

#[cfg(target_os = "macos")]
struct MacOsRuntime {
    _instance: Box<dyn lvos_platform::SingleInstanceGuard>,
    _tray: MacOsTray,
    _hotkey: Arc<Mutex<MacOsHotKey>>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn application_data_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".lvos"),
        |home| PathBuf::from(home).join("Library/Application Support/LVOS"),
    );
    #[cfg(target_os = "windows")]
    return std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("LVOS"),
        |root| PathBuf::from(root).join("LVOS"),
    );
}

#[cfg(target_os = "macos")]
fn hotkey_preference_path() -> PathBuf {
    application_data_root().join("global-hotkey.txt")
}

#[cfg(target_os = "macos")]
fn load_macos_hotkey() -> String {
    std::fs::read_to_string(hotkey_preference_path())
        .ok()
        .filter(|value| lvos_platform::macos::parse_hotkey_display(value).is_ok())
        .unwrap_or_else(|| "⌥D".to_owned())
}

#[cfg(target_os = "macos")]
fn save_macos_hotkey(value: &str) -> Result<(), ()> {
    let path = hotkey_preference_path();
    let parent = path.parent().ok_or(())?;
    std::fs::create_dir_all(parent).map_err(|_| ())?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, value.trim().as_bytes()).map_err(|_| ())?;
    std::fs::rename(temporary, path).map_err(|_| ())
}

#[cfg(target_os = "windows")]
fn windows_hotkey_preference_path() -> PathBuf {
    application_data_root().join("global-hotkey.txt")
}

#[cfg(target_os = "windows")]
fn load_windows_hotkey() -> String {
    std::fs::read_to_string(windows_hotkey_preference_path())
        .ok()
        .filter(|value| lvos_platform::windows::parse_hotkey_display(value).is_ok())
        .unwrap_or_else(|| "Alt+D".to_owned())
}

#[cfg(target_os = "windows")]
fn save_windows_hotkey(value: &str) -> Result<(), ()> {
    let path = windows_hotkey_preference_path();
    let parent = path.parent().ok_or(())?;
    std::fs::create_dir_all(parent).map_err(|_| ())?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, value.trim().as_bytes()).map_err(|_| ())?;
    std::fs::rename(temporary, path).map_err(|_| ())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn load_boolean_preference(name: &str) -> bool {
    std::fs::read_to_string(application_data_root().join(format!("{name}.txt")))
        .is_ok_and(|value| value.trim() == "true")
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn save_boolean_preference(name: &str, value: bool) -> Result<(), ()> {
    let path = application_data_root().join(format!("{name}.txt"));
    let parent = path.parent().ok_or(())?;
    std::fs::create_dir_all(parent).map_err(|_| ())?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, if value { "true" } else { "false" }).map_err(|_| ())?;
    std::fs::rename(temporary, path).map_err(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();
}

#[cfg(target_os = "windows")]
fn init_windows_tracing() -> Result<PathBuf, Box<dyn Error>> {
    use std::fs::OpenOptions;

    let sibling = std::env::current_exe()?.parent().map_or_else(
        || PathBuf::from("LVOS.log"),
        |parent| parent.join("LVOS.log"),
    );
    let (file, path, sibling_error) =
        match OpenOptions::new().create(true).append(true).open(&sibling) {
            Ok(file) => (file, sibling.clone(), None),
            Err(error) => {
                let fallback = application_data_root().join("LVOS.log");
                if let Some(parent) = fallback.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&fallback)?;
                (file, fallback, Some(error))
            }
        };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_ansi(false)
        .with_target(false)
        .with_writer(file)
        .compact()
        .init();
    if let Some(error) = sibling_error {
        tracing::warn!(%error, requested = %sibling.display(), fallback = %path.display(), "executable directory is not writable; using fallback log path");
    }
    Ok(path)
}
