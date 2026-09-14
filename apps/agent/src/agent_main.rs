#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod service;
mod ui_process;

use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use lvos::{DesktopApplication, LocalPreferenceStore, LookupMode, UiPreferences};
use lvos_ipc::{AgentToUi, UiToAgent};
use lvos_platform::{
    InstanceAcquisition, NotificationService, SelectionCapture, SingleInstanceService,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::WindowId,
};

use crate::{
    service::{AgentService, lookup_state},
    ui_process::{SnapshotProvider, UiProcessClient},
};

pub enum NativeAgentEvent {
    UpdateGlobalHotkey {
        shortcut: String,
        response: oneshot::Sender<Result<(), String>>,
    },
    UpdateStartAtLogin {
        enabled: bool,
        response: oneshot::Sender<Result<(), String>>,
    },
    OpenAccessibilitySettings(oneshot::Sender<Result<(), String>>),
    RequestAccessibilityPermission(oneshot::Sender<Result<(), String>>),
    RestartAgent(oneshot::Sender<Result<(), String>>),
    Quit,
}

impl std::fmt::Debug for NativeAgentEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UpdateGlobalHotkey { .. } => "UpdateGlobalHotkey",
            Self::UpdateStartAtLogin { .. } => "UpdateStartAtLogin",
            Self::OpenAccessibilitySettings(_) => "OpenAccessibilitySettings",
            Self::RequestAccessibilityPermission(_) => "RequestAccessibilityPermission",
            Self::RestartAgent(_) => "RestartAgent",
            Self::Quit => "Quit",
        })
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();
    tracing::info!(
        version = lvos_core::SOFTWARE_VERSION,
        process_id = std::process::id(),
        process = "agent",
        "LVOS Agent starting"
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("lvos-agent requires Windows or macOS".into());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    run_agent()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_agent() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "windows")]
    let instance = acquire_windows_instance()?;
    #[cfg(target_os = "macos")]
    let instance = acquire_macos_instance()?;

    let event_loop = EventLoop::<NativeAgentEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let native_events = event_loop.create_proxy();
    let runtime = tokio::runtime::Runtime::new()?;
    let data_root = application_data_root();
    let application = runtime.block_on(open_application(&data_root))?;
    let preferences = load_ui_preferences(&data_root);
    let hotkey_display = load_platform_hotkey(&data_root);
    let service = AgentService::new(
        Arc::clone(&application),
        data_root.clone(),
        preferences,
        hotkey_display.clone(),
        native_events.clone(),
    )?;
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<UiToAgent>();
    let snapshot_service = Arc::clone(&service);
    let snapshot_provider: SnapshotProvider = Arc::new(move || {
        let service = Arc::clone(&snapshot_service);
        Box::pin(async move { service.snapshot().await })
    });
    let ui = {
        let _runtime_context = runtime.enter();
        UiProcessClient::start(&data_root, snapshot_provider, inbound_tx)?
    };
    runtime.spawn(Arc::clone(&service).run_requests(inbound_rx, ui.clone()));
    runtime.spawn(Arc::clone(&service).resume_session(ui.clone()));
    runtime.spawn(Arc::clone(&service).startup_update_check(ui.clone()));

    install_open_handler(instance.as_ref(), &runtime, &ui)?;
    let tray = install_tray(&runtime, &ui, &native_events)?;
    let hotkey = install_hotkey(
        &runtime,
        &ui,
        Arc::clone(&application),
        Arc::clone(&service),
        &hotkey_display,
    )?;

    if !preferences.launch_minimized {
        let startup_ui = ui.clone();
        runtime.spawn(async move {
            if let Err(error) = startup_ui.send(AgentToUi::OpenMainWindow).await {
                tracing::warn!(%error, "failed to open management UI at startup");
            }
        });
    }
    #[cfg(target_os = "macos")]
    if !lvos_platform::macos::accessibility_permission_granted() {
        let permission_ui = ui.clone();
        runtime.spawn(async move {
            if let Err(error) = permission_ui
                .send(AgentToUi::ShowPermission {
                    status: "Accessibility permission is required for selection capture."
                        .to_owned(),
                })
                .await
            {
                tracing::warn!(%error, "failed to show Accessibility permission UI");
            }
        });
    }

    let mut application = NativeApplication {
        runtime,
        ui,
        _instance: instance,
        _tray: tray,
        hotkey,
        restart_requested: false,
    };
    event_loop.run_app(&mut application)?;
    application.runtime.block_on(application.ui.shutdown());
    let restart_requested = application.restart_requested;
    drop(application);
    if restart_requested {
        std::process::Command::new(std::env::current_exe()?).spawn()?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn open_application(data_root: &Path) -> Result<Arc<DesktopApplication>, Box<dyn Error>> {
    let credentials: Arc<dyn lvos_auth::CredentialStore> = {
        #[cfg(target_os = "macos")]
        {
            Arc::new(lvos_platform::macos::MacOsCredentialStore)
        }
        #[cfg(target_os = "windows")]
        {
            Arc::new(lvos_platform::windows::WindowsCredentialStore)
        }
    };
    #[cfg(target_os = "macos")]
    let platform = lvos_storage::Platform::Macos;
    #[cfg(target_os = "windows")]
    let platform = lvos_storage::Platform::Windows;
    Ok(DesktopApplication::open(
        data_root.to_path_buf(),
        platform,
        &device_name(),
        credentials,
    )
    .await?)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeApplication {
    runtime: tokio::runtime::Runtime,
    ui: UiProcessClient,
    _instance: Box<dyn lvos_platform::SingleInstanceGuard>,
    #[cfg(target_os = "windows")]
    _tray: lvos_platform::windows::WindowsTray,
    #[cfg(target_os = "macos")]
    _tray: lvos_platform::macos::MacOsTray,
    #[cfg(target_os = "windows")]
    hotkey: lvos_platform::windows::WindowsHotKey,
    #[cfg(target_os = "macos")]
    hotkey: lvos_platform::macos::MacOsHotKey,
    restart_requested: bool,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl ApplicationHandler<NativeAgentEvent> for NativeApplication {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: NativeAgentEvent) {
        match event {
            NativeAgentEvent::UpdateGlobalHotkey { shortcut, response } => {
                let result = self.hotkey.update(&shortcut).map_err(|error| match error {
                    lvos_platform::PlatformError::Conflict => {
                        "That shortcut is already in use. The previous hotkey remains active."
                            .to_owned()
                    }
                    _ => "The shortcut could not be registered.".to_owned(),
                });
                let _ = response.send(result);
            }
            NativeAgentEvent::UpdateStartAtLogin { enabled, response } => {
                #[cfg(target_os = "windows")]
                let result = lvos_platform::windows::set_start_at_login(enabled)
                    .map_err(|_| "Windows could not update current-user startup.".to_owned());
                #[cfg(target_os = "macos")]
                let result = lvos_platform::macos::set_start_at_login(enabled).map_err(|_| {
                    "Start at login is available only from the packaged LVOS app.".to_owned()
                });
                let _ = response.send(result);
            }
            NativeAgentEvent::OpenAccessibilitySettings(response) => {
                #[cfg(target_os = "macos")]
                let result = lvos_platform::macos::open_accessibility_settings()
                    .map_err(|_| "macOS Accessibility settings could not be opened.".to_owned());
                #[cfg(not(target_os = "macos"))]
                let result = Err("Accessibility settings are available only on macOS.".to_owned());
                let _ = response.send(result);
            }
            NativeAgentEvent::RequestAccessibilityPermission(response) => {
                #[cfg(target_os = "macos")]
                let result = if lvos_platform::macos::request_accessibility_permission() {
                    Ok(())
                } else {
                    Err(
                        "Grant Accessibility access to the LVOS Agent, then check again."
                            .to_owned(),
                    )
                };
                #[cfg(not(target_os = "macos"))]
                let result = Err("Accessibility permission is available only on macOS.".to_owned());
                let _ = response.send(result);
            }
            NativeAgentEvent::RestartAgent(response) => {
                self.restart_requested = true;
                let _ = response.send(Ok(()));
                event_loop.exit();
            }
            NativeAgentEvent::Quit => {
                self.runtime.block_on(self.ui.shutdown());
                event_loop.exit();
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn acquire_windows_instance() -> Result<Box<dyn lvos_platform::SingleInstanceGuard>, Box<dyn Error>>
{
    match lvos_platform::windows::WindowsSingleInstanceService.acquire()? {
        InstanceAcquisition::Primary(guard) => Ok(guard),
        InstanceAcquisition::Existing(guard) => {
            guard.signal_existing()?;
            std::process::exit(0);
        }
    }
}

#[cfg(target_os = "macos")]
fn acquire_macos_instance() -> Result<Box<dyn lvos_platform::SingleInstanceGuard>, Box<dyn Error>> {
    let service = lvos_platform::macos::MacOsSingleInstanceService::new(&application_data_root());
    match service.acquire()? {
        InstanceAcquisition::Primary(guard) => Ok(guard),
        InstanceAcquisition::Existing(guard) => {
            guard.signal_existing()?;
            std::process::exit(0);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn install_open_handler(
    instance: &dyn lvos_platform::SingleInstanceGuard,
    runtime: &tokio::runtime::Runtime,
    ui: &UiProcessClient,
) -> Result<(), lvos_platform::PlatformError> {
    let handle = runtime.handle().clone();
    let ui = ui.clone();
    instance.set_open_handler(Arc::new(move || {
        let ui = ui.clone();
        handle.spawn(async move {
            if let Err(error) = ui.send(AgentToUi::OpenMainWindow).await {
                tracing::warn!(%error, "failed to handle second-instance activation");
            }
        });
    }))
}

#[cfg(target_os = "windows")]
fn install_tray(
    runtime: &tokio::runtime::Runtime,
    ui: &UiProcessClient,
    native_events: &EventLoopProxy<NativeAgentEvent>,
) -> Result<lvos_platform::windows::WindowsTray, Box<dyn Error>> {
    use lvos_platform::windows::{TrayAction, WindowsTray};
    let tray = WindowsTray::install()?;
    let handle = runtime.handle().clone();
    let ui = ui.clone();
    let events = native_events.clone();
    tray.set_action_handler(Arc::new(move |action| match action {
        TrayAction::OpenMainWindow => {
            let ui = ui.clone();
            handle.spawn(async move {
                if let Err(error) = ui.send(AgentToUi::OpenMainWindow).await {
                    tracing::warn!(%error, "failed to open Main Window from tray");
                }
            });
        }
        TrayAction::Quit => {
            let _ = events.send_event(NativeAgentEvent::Quit);
        }
    }));
    Ok(tray)
}

#[cfg(target_os = "macos")]
fn install_tray(
    runtime: &tokio::runtime::Runtime,
    ui: &UiProcessClient,
    native_events: &EventLoopProxy<NativeAgentEvent>,
) -> Result<lvos_platform::macos::MacOsTray, Box<dyn Error>> {
    use lvos_platform::macos::{MacOsTray, TrayAction};
    let tray = MacOsTray::install()?;
    let handle = runtime.handle().clone();
    let ui = ui.clone();
    let events = native_events.clone();
    tray.set_action_handler(Arc::new(move |action| match action {
        TrayAction::OpenMainWindow => {
            let ui = ui.clone();
            handle.spawn(async move {
                if let Err(error) = ui.send(AgentToUi::OpenMainWindow).await {
                    tracing::warn!(%error, "failed to open Main Window from tray");
                }
            });
        }
        TrayAction::Quit => {
            let _ = events.send_event(NativeAgentEvent::Quit);
        }
    }));
    Ok(tray)
}

#[cfg(target_os = "windows")]
fn install_hotkey(
    runtime: &tokio::runtime::Runtime,
    ui: &UiProcessClient,
    application: Arc<DesktopApplication>,
    service: Arc<AgentService>,
    shortcut: &str,
) -> Result<lvos_platform::windows::WindowsHotKey, Box<dyn Error>> {
    use lvos_platform::windows::{
        WindowsHotKey, WindowsNotificationService, WindowsSelectionCapture,
    };
    let hotkey = WindowsHotKey::register(shortcut).inspect_err(|_| {
        let _ = WindowsNotificationService
            .error("The configured shortcut is unavailable. Choose another shortcut in Settings.");
    })?;
    let handle = runtime.handle().clone();
    let ui = ui.clone();
    let capture = Arc::new(WindowsSelectionCapture::default());
    hotkey.set_activation_handler(Arc::new(move || {
        if service.ui_is_blocking() {
            return;
        }
        let application = Arc::clone(&application);
        let capture = Arc::clone(&capture);
        let ui = ui.clone();
        handle.spawn(async move {
            match capture
                .capture_selected_text(Duration::from_millis(800))
                .await
            {
                Ok(source) => show_captured_lookup(application, ui, source).await,
                Err(lvos_platform::CaptureError::Busy) => {}
                Err(error) => {
                    tracing::warn!(%error, "Windows selection capture failed");
                    let _ = WindowsNotificationService.error(&error.to_string());
                }
            }
        });
    }));
    Ok(hotkey)
}

#[cfg(target_os = "macos")]
fn install_hotkey(
    runtime: &tokio::runtime::Runtime,
    ui: &UiProcessClient,
    application: Arc<DesktopApplication>,
    service: Arc<AgentService>,
    shortcut: &str,
) -> Result<lvos_platform::macos::MacOsHotKey, Box<dyn Error>> {
    use lvos_platform::macos::{MacOsHotKey, MacOsNotificationService, MacOsSelectionCapture};
    let registration = lvos_platform::macos::parse_hotkey_display(shortcut)?;
    let hotkey = MacOsHotKey::register(&registration).inspect_err(|_| {
        let _ = MacOsNotificationService
            .error("The configured shortcut is unavailable. Choose another shortcut in Settings.");
    })?;
    let handle = runtime.handle().clone();
    let ui = ui.clone();
    let capture = Arc::new(MacOsSelectionCapture::default());
    hotkey.set_pressed_handler(Arc::new(move || {
        if service.ui_is_blocking() {
            return;
        }
        if !lvos_platform::macos::accessibility_permission_granted() {
            let ui = ui.clone();
            handle.spawn(async move {
                let _ = ui
                    .send(AgentToUi::ShowPermission {
                        status: "Accessibility permission is required for selection capture."
                            .to_owned(),
                    })
                    .await;
            });
            return;
        }
        let application = Arc::clone(&application);
        let capture = Arc::clone(&capture);
        let ui = ui.clone();
        handle.spawn(async move {
            match capture
                .capture_selected_text(Duration::from_millis(800))
                .await
            {
                Ok(source) => show_captured_lookup(application, ui, source).await,
                Err(lvos_platform::CaptureError::Busy) => {}
                Err(error) => {
                    tracing::warn!(%error, "macOS selection capture failed");
                    let _ = MacOsNotificationService.error(&error.to_string());
                }
            }
        });
    }));
    Ok(hotkey)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn show_captured_lookup(
    application: Arc<DesktopApplication>,
    ui: UiProcessClient,
    source: String,
) {
    let loading = application.begin_lookup(source.clone());
    let query_id = loading.generation().unwrap_or(0);
    let display_session_id = Uuid::new_v4();
    let loading = match lookup_state(loading) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "invalid loading Lookup state");
            return;
        }
    };
    if let Err(error) = ui
        .send(AgentToUi::BeginLookup {
            display_session_id,
            state: loading,
        })
        .await
    {
        tracing::warn!(%error, "failed to begin Lookup UI session");
        return;
    }
    let state = application
        .complete_lookup(query_id, source, LookupMode::UseCache)
        .await;
    if !application.is_current(&state) {
        return;
    }
    match lookup_state(state) {
        Ok(state) => {
            // Results update the exact visible session only. A dismissed or exited GUI is never
            // restarted by a late completion.
            let _ = ui
                .send_if_ready(AgentToUi::UpdateLookup {
                    display_session_id,
                    state,
                })
                .await;
        }
        Err(error) => tracing::warn!(%error, "invalid completed Lookup state"),
    }
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn load_ui_preferences(data_root: &PathBuf) -> UiPreferences {
    match UiPreferences::load(&LocalPreferenceStore::new(data_root)) {
        Ok(preferences) => preferences,
        Err(error) => {
            tracing::warn!(%error, "invalid persisted UI preferences; using defaults");
            UiPreferences::default()
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn load_platform_hotkey(data_root: &PathBuf) -> String {
    let saved = LocalPreferenceStore::new(data_root).load_text("global-hotkey");
    #[cfg(target_os = "macos")]
    return saved
        .filter(|value| lvos_platform::macos::parse_hotkey_display(value).is_ok())
        .unwrap_or_else(|| "⌥D".to_owned());
    #[cfg(target_os = "windows")]
    return saved
        .filter(|value| lvos_platform::windows::parse_hotkey_display(value).is_ok())
        .unwrap_or_else(|| "Alt+D".to_owned());
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "LVOS Device".to_owned())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
}
