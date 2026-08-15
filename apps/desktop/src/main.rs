#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::error::Error;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "windows")]
use std::{cell::RefCell, path::Path, rc::Rc};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{path::PathBuf, sync::Arc};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use lvos::{
    DesktopApplication, GitHubUpdateConfig, GitHubUpdateService, HttpUpdateTransport, LookupMode,
    NativeReleasePageOpener, ProviderPreferences, UpdateCheckOutcome, UpdateCoordinator,
};
use lvos::{DesktopRuntime, SlintUiDispatcher, UiController};
use lvos_core::{DEFAULT_UPDATE_CHANNEL, PRODUCT_NAME, SOFTWARE_VERSION};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use slint::{ComponentHandle, ModelRc, VecModel};

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
    let application = install_application_runtime(&ui, &runtime)?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    install_update_runtime(&ui, &runtime)?;
    #[cfg(target_os = "macos")]
    let native = install_macos_runtime(&ui, &runtime, instance, Arc::clone(&application))?;
    #[cfg(target_os = "windows")]
    let native =
        install_windows_runtime(&ui, &runtime, instance, &log_path, Arc::clone(&application))?;
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
fn install_application_runtime(
    ui: &UiController,
    runtime: &DesktopRuntime<SlintUiDispatcher>,
) -> Result<Arc<DesktopApplication>, Box<dyn Error>> {
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
    let platform = {
        #[cfg(target_os = "macos")]
        {
            lvos_storage::Platform::Macos
        }
        #[cfg(target_os = "windows")]
        {
            lvos_storage::Platform::Windows
        }
    };
    let application = runtime.runtime_handle().block_on(DesktopApplication::open(
        application_data_root(),
        platform,
        &device_name(),
        credentials,
    ))?;
    let preferences = application.provider_preferences();
    ui.main_window()
        .set_tokenhub_model(preferences.tokenhub_model.clone().into());
    ui.main_window()
        .set_tokenhub_configured(application.provider_configuration()?);
    let profile = application.profile();
    ui.main_window().set_server_url(
        profile
            .server_origin
            .as_deref()
            .unwrap_or(lvos_core::DEFAULT_SERVER_URL)
            .into(),
    );
    ui.main_window()
        .set_username(profile.username.as_deref().unwrap_or_default().into());
    let installation = application.installation();
    ui.main_window()
        .set_current_device(installation.device_name.as_str().into());
    let should_resume = profile.user_id.is_some();
    ui.main_window().set_sync_status(
        if should_resume {
            "Restoring session…"
        } else {
            "Login required"
        }
        .into(),
    );
    install_local_ui_callbacks(ui, runtime, Arc::clone(&application));
    refresh_history(
        ui.main_window().as_weak(),
        &runtime.runtime_handle(),
        Arc::clone(&application),
        String::new(),
    );
    refresh_favorites(
        ui.main_window().as_weak(),
        &runtime.runtime_handle(),
        Arc::clone(&application),
        String::new(),
    );
    if should_resume {
        let main = ui.main_window().as_weak();
        let resume_application = Arc::clone(&application);
        let resume_handle = runtime.runtime_handle();
        resume_handle.clone().spawn(async move {
            match resume_application.resume_session().await {
                Ok(()) => {
                    apply_account_state(&main, &resume_application, "Connected");
                    refresh_devices(main, &tokio::runtime::Handle::current(), resume_application);
                }
                Err(error) => apply_account_state(
                    &main,
                    &resume_application,
                    &format!("Session restore failed: {error}"),
                ),
            }
        });
    }
    Ok(application)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[allow(clippy::too_many_lines)]
fn install_local_ui_callbacks(
    ui: &UiController,
    runtime: &DesktopRuntime<SlintUiDispatcher>,
    application: Arc<DesktopApplication>,
) {
    let handle = runtime.runtime_handle();
    let main = ui.main_window().as_weak();
    let history_application = Arc::clone(&application);
    let history_handle = handle.clone();
    ui.main_window().on_history_search(move |term| {
        refresh_history(
            main.clone(),
            &history_handle,
            Arc::clone(&history_application),
            term.to_string(),
        );
    });

    let main = ui.main_window().as_weak();
    let favorites_application = Arc::clone(&application);
    let favorites_handle = handle.clone();
    ui.main_window().on_favorites_search(move |term| {
        refresh_favorites(
            main.clone(),
            &favorites_handle,
            Arc::clone(&favorites_application),
            term.to_string(),
        );
    });

    let main = ui.main_window().as_weak();
    let toggle_application = Arc::clone(&application);
    let toggle_handle = handle.clone();
    ui.main_window()
        .on_favorite_toggled(move |key, currently_active| {
            let main = main.clone();
            let application = Arc::clone(&toggle_application);
            toggle_handle.spawn(async move {
                if let Err(error) = application
                    .set_favorite(key.to_string(), !currently_active)
                    .await
                {
                    set_settings_error(&main, format!("Favorite update failed: {error}"));
                    return;
                }
                refresh_history(
                    main.clone(),
                    &tokio::runtime::Handle::current(),
                    Arc::clone(&application),
                    String::new(),
                );
                refresh_favorites(
                    main,
                    &tokio::runtime::Handle::current(),
                    application,
                    String::new(),
                );
            });
        });

    let main = ui.main_window().as_weak();
    let clear_application = Arc::clone(&application);
    let clear_handle = handle.clone();
    ui.main_window().on_clear_history_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&clear_application);
        clear_handle.spawn(async move {
            match application.clear_history().await {
                Ok(()) => refresh_history(
                    main,
                    &tokio::runtime::Handle::current(),
                    application,
                    String::new(),
                ),
                Err(error) => set_settings_error(&main, format!("Clear History failed: {error}")),
            }
        });
    });

    let main = ui.main_window().as_weak();
    let settings_application = Arc::clone(&application);
    ui.main_window()
        .on_persist_provider_settings(move |tokenhub_model, tokenhub_key| {
            let preferences = ProviderPreferences {
                tokenhub_model: tokenhub_model.to_string(),
            };
            match settings_application.save_provider_settings(preferences, tokenhub_key.as_str()) {
                Ok(()) => {
                    let tokenhub_model = settings_application.provider_preferences().tokenhub_model;
                    if let Ok(tokenhub) = settings_application.provider_configuration()
                        && let Some(main) = main.upgrade()
                    {
                        main.set_tokenhub_model(tokenhub_model.into());
                        main.set_tokenhub_configured(tokenhub);
                        main.set_settings_error("Provider settings saved.".into());
                    }
                }
                Err(error) => set_settings_error(&main, error.to_string()),
            }
        });

    let main = ui.main_window().as_weak();
    let test_application = Arc::clone(&application);
    let test_handle = handle.clone();
    ui.main_window()
        .on_test_provider(move |tokenhub_model, tokenhub_key| {
            let main = main.clone();
            let application = Arc::clone(&test_application);
            let tokenhub_model = tokenhub_model.to_string();
            let tokenhub_key = tokenhub_key.to_string();
            test_handle.spawn(async move {
                let message = match application
                    .test_provider(&tokenhub_model, &tokenhub_key)
                    .await
                {
                    Ok(()) => "Provider test succeeded.".to_owned(),
                    Err(error) => format!("Provider test failed: {error}"),
                };
                set_settings_error(&main, message);
            });
        });

    let main = ui.main_window().as_weak();
    let login_application = Arc::clone(&application);
    let login_handle = handle.clone();
    ui.main_window()
        .on_login_requested(move |server, username, password| {
            if let Some(main) = main.upgrade() {
                main.set_sync_status("Signing in…".into());
            }
            let main = main.clone();
            let application = Arc::clone(&login_application);
            login_handle.spawn(async move {
                match application
                    .login(
                        server.to_string(),
                        username.to_string(),
                        password.to_string(),
                    )
                    .await
                {
                    Ok(()) => {
                        apply_account_state(&main, &application, "Connected");
                        refresh_devices(
                            main.clone(),
                            &tokio::runtime::Handle::current(),
                            Arc::clone(&application),
                        );
                        refresh_history(
                            main.clone(),
                            &tokio::runtime::Handle::current(),
                            Arc::clone(&application),
                            String::new(),
                        );
                        refresh_favorites(
                            main,
                            &tokio::runtime::Handle::current(),
                            application,
                            String::new(),
                        );
                    }
                    Err(error) => {
                        apply_account_state(&main, &application, &format!("Login failed: {error}"));
                    }
                }
            });
        });

    let main = ui.main_window().as_weak();
    let logout_application = Arc::clone(&application);
    let logout_handle = handle.clone();
    ui.main_window().on_logout_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&logout_application);
        logout_handle.spawn(async move {
            let status = match application.logout().await {
                Ok(()) => "Login required".to_owned(),
                Err(error) => format!("Logout completed locally; Server error: {error}"),
            };
            apply_account_state(&main, &application, &status);
        });
    });

    let main = ui.main_window().as_weak();
    let manual_application = Arc::clone(&application);
    let manual_handle = handle.clone();
    ui.main_window().on_manual_sync_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&manual_application);
        manual_handle.spawn(async move {
            let status = if application.manual_sync().await {
                "Sync requested"
            } else {
                "Login required"
            };
            apply_account_state(&main, &application, status);
        });
    });

    let main = ui.main_window().as_weak();
    let connection_application = Arc::clone(&application);
    let connection_handle = handle.clone();
    ui.main_window().on_test_connection_requested(move || {
        let origin = main
            .upgrade()
            .map(|main| main.get_server_url().to_string())
            .unwrap_or_default();
        let main = main.clone();
        let application = Arc::clone(&connection_application);
        connection_handle.spawn(async move {
            let status = match application.test_connection(&origin).await {
                Ok(()) => "Server compatibility check succeeded.".to_owned(),
                Err(error) => format!("Server check failed: {error}"),
            };
            set_settings_error(&main, status);
        });
    });

    let main = ui.main_window().as_weak();
    let revoke_application = Arc::clone(&application);
    let revoke_handle = handle.clone();
    ui.main_window().on_revoke_device_requested(move |device_id| {
        let main = main.clone();
        let application = Arc::clone(&revoke_application);
        revoke_handle.spawn(async move {
            let is_current = device_id.as_str() == application.installation().device_id.to_string();
            if is_current {
                let confirmed = rfd::AsyncMessageDialog::new()
                    .set_title("Revoke this Device?")
                    .set_description("This immediately logs out this installation. Its Device identity remains revoked until you explicitly replace it.")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if confirmed != rfd::MessageDialogResult::Yes {
                    return;
                }
            }
            match application.revoke_device(device_id.as_str()).await {
                Ok(()) if is_current => {
                    let _ = application.logout().await;
                    apply_account_state(&main, &application, "Device revoked · Login required");
                }
                Ok(()) => {
                    refresh_devices(main, &tokio::runtime::Handle::current(), application);
                }
                Err(error) => set_settings_error(&main, format!("Device revoke failed: {error}")),
            }
        });
    });

    let main = ui.main_window().as_weak();
    let recovery_application = Arc::clone(&application);
    let recovery_handle = handle.clone();
    ui.main_window().on_regenerate_device_identity_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&recovery_application);
        recovery_handle.spawn(async move {
            let confirmed = rfd::AsyncMessageDialog::new()
                .set_title("Replace revoked Device identity?")
                .set_description("This creates a new permanent installation Device ID, removes old sessions, and preserves Profiles and pending Outbox data. Continue only after this installation was revoked.")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                .await;
            if confirmed != rfd::MessageDialogResult::Yes {
                return;
            }
            match application.recover_revoked_device().await {
                Ok(()) => apply_account_state(
                    &main,
                    &application,
                    "Device identity replaced · Login again",
                ),
                Err(error) => set_settings_error(&main, format!("Device recovery failed: {error}")),
            }
        });
    });

    let main = ui.main_window().as_weak();
    let export_application = Arc::clone(&application);
    let export_handle = handle.clone();
    ui.main_window().on_export_data_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&export_application);
        export_handle.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("LVOS Portable JSON", &["json"])
                .set_file_name("lvos-export.json")
                .save_file()
                .await
            else {
                return;
            };
            let bytes = match application.export_portable_json().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    set_settings_error(&main, format!("Export failed: {error}"));
                    return;
                }
            };
            let path = file.path().to_path_buf();
            match tokio::task::spawn_blocking(move || std::fs::write(path, bytes)).await {
                Ok(Ok(())) => set_settings_error(&main, "Export completed.".to_owned()),
                Ok(Err(error)) => set_settings_error(&main, format!("Export failed: {error}")),
                Err(error) => set_settings_error(&main, format!("Export task failed: {error}")),
            }
        });
    });

    let main = ui.main_window().as_weak();
    let import_application = Arc::clone(&application);
    let import_handle = handle.clone();
    ui.main_window().on_import_data_requested(move || {
        let main = main.clone();
        let application = Arc::clone(&import_application);
        import_handle.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("LVOS Portable JSON", &["json"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let bytes = match tokio::task::spawn_blocking(move || {
                let metadata = std::fs::metadata(&path)?;
                if metadata.len() > u64::try_from(lvos_core::MAX_PORTABLE_JSON_BYTES).unwrap_or(u64::MAX) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Portable JSON exceeds 16 MiB",
                    ));
                }
                std::fs::read(path)
            })
            .await
            {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) => {
                    set_settings_error(&main, format!("Import read failed: {error}"));
                    return;
                }
                Err(error) => {
                    set_settings_error(&main, format!("Import task failed: {error}"));
                    return;
                }
            };
            let plan = match application.preview_portable_import(bytes).await {
                Ok(plan) => plan,
                Err(error) => {
                    set_settings_error(&main, format!("Import validation failed: {error}"));
                    return;
                }
            };
            let preview = plan.preview();
            let confirmed = rfd::AsyncMessageDialog::new()
                .set_title("Import LVOS data?")
                .set_description(format!(
                    "History: {} add, {} update. Favorites: {} add, {} reactivate. QueryStats archive: {} records. No changes are made until you choose Yes.",
                    preview.history_add,
                    preview.history_update,
                    preview.favorite_add,
                    preview.favorite_reactivate,
                    preview.query_stats_archive,
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                .await;
            if confirmed != rfd::MessageDialogResult::Yes {
                set_settings_error(&main, "Import cancelled; no data changed.".to_owned());
                return;
            }
            match application.apply_portable_import(plan).await {
                Ok(result) => {
                    set_settings_error(
                        &main,
                        format!(
                            "Import completed: {} History added, {} Favorites added/reactivated.",
                            result.history_add,
                            result.favorite_add.saturating_add(result.favorite_reactivate),
                        ),
                    );
                    refresh_history(
                        main.clone(),
                        &tokio::runtime::Handle::current(),
                        Arc::clone(&application),
                        String::new(),
                    );
                    refresh_favorites(
                        main,
                        &tokio::runtime::Handle::current(),
                        application,
                        String::new(),
                    );
                }
                Err(error) => set_settings_error(&main, format!("Import failed: {error}")),
            }
        });
    });

    let popup = ui.popup().as_weak();
    let popup_application = Arc::clone(&application);
    let popup_handle = handle.clone();
    ui.popup().on_favorite_toggled(move || {
        let Some(popup) = popup.upgrade() else { return };
        let application = Arc::clone(&popup_application);
        let popup = popup.as_weak();
        let currently_active = popup.upgrade().is_some_and(|popup| popup.get_favorite());
        popup_handle.spawn(async move {
            if application
                .set_last_favorite(!currently_active)
                .await
                .is_ok()
                && let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(popup) = popup.upgrade() {
                        popup.set_favorite(!currently_active);
                    }
                })
            {
                tracing::warn!(%error, "failed to update Popup Favorite state");
            }
        });
    });

    let popup = ui.popup().as_weak();
    let refresh_application = application;
    ui.popup().on_refresh_requested(move || {
        let popup = popup.clone();
        let application = Arc::clone(&refresh_application);
        handle.spawn(async move {
            if let Some(state) = application.refresh_last().await
                && application.is_current(&state)
                && let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(popup) = popup.upgrade()
                        && let Err(error) = lvos::show_lookup_state(&popup, &state)
                    {
                        tracing::warn!(%error, "failed to show refreshed Lookup Card");
                    }
                })
            {
                tracing::warn!(%error, "failed to dispatch refreshed Lookup Card");
            }
        });
    });
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn refresh_history(
    main: slint::Weak<lvos::MainWindow>,
    handle: &tokio::runtime::Handle,
    application: Arc<DesktopApplication>,
    term: String,
) {
    handle.spawn(async move {
        match application.history(term).await {
            Ok(records) => {
                if let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(main) = main.upgrade() {
                        let records: Vec<lvos::UiRecord> = records
                            .iter()
                            .map(|record| {
                                lvos::ui_record(
                                    record.key,
                                    &record.source,
                                    &record.translation,
                                    record.count,
                                    record.favorite,
                                    &record.metadata,
                                )
                            })
                            .collect();
                        main.set_history_records(ModelRc::new(VecModel::from(records)));
                    }
                }) {
                    tracing::warn!(%error, "failed to dispatch History records");
                }
            }
            Err(error) => set_settings_error(&main, format!("History load failed: {error}")),
        }
    });
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn refresh_favorites(
    main: slint::Weak<lvos::MainWindow>,
    handle: &tokio::runtime::Handle,
    application: Arc<DesktopApplication>,
    term: String,
) {
    handle.spawn(async move {
        match application.favorites(term).await {
            Ok(records) => {
                if let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(main) = main.upgrade() {
                        let records: Vec<lvos::UiRecord> = records
                            .iter()
                            .map(|record| {
                                lvos::ui_record(
                                    record.key,
                                    &record.source,
                                    &record.translation,
                                    record.count,
                                    record.favorite,
                                    &record.metadata,
                                )
                            })
                            .collect();
                        main.set_favorite_records(ModelRc::new(VecModel::from(records)));
                    }
                }) {
                    tracing::warn!(%error, "failed to dispatch Favorite records");
                }
            }
            Err(error) => set_settings_error(&main, format!("Favorites load failed: {error}")),
        }
    });
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn refresh_devices(
    main: slint::Weak<lvos::MainWindow>,
    handle: &tokio::runtime::Handle,
    application: Arc<DesktopApplication>,
) {
    handle.spawn(async move {
        match application.devices().await {
            Ok(devices) => {
                let current = application.installation().device_id.to_string();
                if let Err(error) = slint::invoke_from_event_loop(move || {
                    if let Some(main) = main.upgrade() {
                        let records: Vec<lvos::DeviceRecord> = devices
                            .into_iter()
                            .map(|device| lvos::DeviceRecord {
                                id: device.device_id.clone().into(),
                                name: device
                                    .device_name
                                    .unwrap_or_else(|| device.device_id.clone())
                                    .into(),
                                platform: device.platform.into(),
                                last_seen: format!("Unix {}", device.last_seen_at).into(),
                                current: device.device_id == current,
                                revoked: device.revoked_at.is_some(),
                            })
                            .collect();
                        main.set_devices(ModelRc::new(VecModel::from(records)));
                    }
                }) {
                    tracing::warn!(%error, "failed to dispatch Devices");
                }
            }
            Err(error) => set_settings_error(&main, format!("Devices load failed: {error}")),
        }
    });
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn apply_account_state(
    main: &slint::Weak<lvos::MainWindow>,
    application: &DesktopApplication,
    status: &str,
) {
    let profile = application.profile();
    let preferences = application.provider_preferences();
    let configured = application.provider_configuration().unwrap_or_default();
    let main = main.clone();
    let status = status.to_owned();
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if let Some(main) = main.upgrade() {
            main.set_server_url(
                profile
                    .server_origin
                    .as_deref()
                    .unwrap_or(lvos_core::DEFAULT_SERVER_URL)
                    .into(),
            );
            main.set_username(profile.username.as_deref().unwrap_or_default().into());
            main.set_tokenhub_model(preferences.tokenhub_model.into());
            main.set_tokenhub_configured(configured);
            main.set_sync_status(status.into());
        }
    }) {
        tracing::warn!(%error, "failed to dispatch account state");
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn set_settings_error(main: &slint::Weak<lvos::MainWindow>, message: String) {
    let main = main.clone();
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if let Some(main) = main.upgrade() {
            main.set_settings_error(message.into());
        }
    }) {
        tracing::warn!(%error, "failed to dispatch Desktop status");
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "LVOS Device".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn show_captured_lookup(
    application: Arc<DesktopApplication>,
    popup: slint::Weak<lvos::QuickLookupPopup>,
    source: String,
) {
    let loading = application.begin_lookup(source.clone());
    let generation = loading.generation().unwrap_or(0);
    let loading_popup = popup.clone();
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if let Some(popup) = loading_popup.upgrade()
            && let Err(error) = lvos::show_lookup_state(&popup, &loading)
        {
            tracing::warn!(%error, "failed to show loading Lookup Card");
        }
    }) {
        tracing::warn!(%error, "failed to dispatch loading Lookup Card");
    }
    let state = application
        .complete_lookup(generation, source, LookupMode::UseCache)
        .await;
    if !application.is_current(&state) {
        return;
    }
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if let Some(popup) = popup.upgrade()
            && let Err(error) = lvos::show_lookup_state(&popup, &state)
        {
            tracing::warn!(%error, "failed to show Lookup Card result");
        }
    }) {
        tracing::warn!(%error, "failed to dispatch Lookup Card result");
    }
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
    application: Arc<DesktopApplication>,
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
        let application = Arc::clone(&application);
        async_runtime.spawn(async move {
            tracing::info!(timeout_ms = 800_u64, "Windows selection capture task started");
            match capture.capture_selected_text(std::time::Duration::from_millis(800)).await {
                Ok(source) => {
                    tracing::info!(selected_text_bytes = source.len(), "Windows selection capture completed");
                    show_captured_lookup(application, popup, source).await;
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
    application: Arc<DesktopApplication>,
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
        let application = Arc::clone(&application);
        async_runtime.spawn(async move {
            match capture
                .capture_selected_text(std::time::Duration::from_millis(800))
                .await
            {
                Ok(source) => {
                    show_captured_lookup(application, popup, source).await;
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
