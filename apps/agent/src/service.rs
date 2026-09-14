use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use lvos::{
    DesktopApplication, GitHubUpdateConfig, GitHubUpdateService, HttpUpdateTransport,
    LocalPreferenceStore, LookupCardState, NativeReleasePageOpener, NetworkPreferences,
    ProviderPreferences, ProxyKind, UiPreferences, UpdateCheckOutcome, UpdateCoordinator,
};
use lvos_ipc::{
    AgentToUi, DeviceSnapshot, FeedbackLevel, LookupErrorKind, LookupUiState, MainUiPatch,
    MainUiSnapshot, OperationOutput, OperationResult, UiFeedback, UiOperation,
    UiPreferenceSnapshot, UiRecordSnapshot, UiSnapshot, UiToAgent,
};
use lvos_storage::PortableImportPlan;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;
use winit::event_loop::EventLoopProxy;

use crate::{NativeAgentEvent, ui_process::UiProcessClient};

pub(crate) struct AgentService {
    application: Arc<DesktopApplication>,
    data_root: PathBuf,
    preferences: RwLock<UiPreferences>,
    global_hotkey: RwLock<String>,
    sync_status: RwLock<String>,
    update_status: RwLock<String>,
    revision: AtomicU64,
    ui_blocked: AtomicBool,
    state_order: Mutex<()>,
    imports: Mutex<HashMap<Uuid, PortableImportPlan>>,
    update: Arc<UpdateCoordinator>,
    update_transport: HttpUpdateTransport,
    update_channel: String,
    native_events: EventLoopProxy<NativeAgentEvent>,
}

impl std::fmt::Debug for AgentService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentService")
            .field("data_root", &self.data_root)
            .field("revision", &self.revision.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl AgentService {
    /// Creates the authoritative background service state.
    ///
    /// # Errors
    /// Returns an error when the update transport or target configuration is unavailable.
    pub(crate) fn new(
        application: Arc<DesktopApplication>,
        data_root: PathBuf,
        preferences: UiPreferences,
        global_hotkey: String,
        native_events: EventLoopProxy<NativeAgentEvent>,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let update_channel = std::env::var("LVOS_UPDATE_CHANNEL")
            .unwrap_or_else(|_| lvos_core::DEFAULT_UPDATE_CHANNEL.to_owned());
        let config = GitHubUpdateConfig::lvos(update_channel.clone())?;
        let network = application.network_preferences();
        let proxy_url = if network.update_proxy_enabled {
            network.proxy_url()?
        } else {
            None
        };
        let update_transport = HttpUpdateTransport::new(proxy_url.as_deref())?;
        let update_service = Arc::new(GitHubUpdateService::new(update_transport.clone(), config));
        let update = Arc::new(UpdateCoordinator::new(
            update_service,
            Arc::new(NativeReleasePageOpener),
            &data_root,
        ));
        let signed_in = application.profile().user_id.is_some();
        Ok(Arc::new(Self {
            application,
            data_root,
            preferences: RwLock::new(preferences),
            global_hotkey: RwLock::new(global_hotkey),
            sync_status: RwLock::new(
                if signed_in {
                    "Restoring session…"
                } else {
                    "Login required"
                }
                .to_owned(),
            ),
            update_status: RwLock::new(format!(
                "Current {} · {} · Not checked",
                lvos_core::SOFTWARE_VERSION,
                update_channel
            )),
            revision: AtomicU64::new(0),
            ui_blocked: AtomicBool::new(false),
            state_order: Mutex::new(()),
            imports: Mutex::new(HashMap::new()),
            update,
            update_transport,
            update_channel,
            native_events,
        }))
    }

    pub(crate) async fn snapshot(&self) -> UiSnapshot {
        let _order = self.state_order.lock().await;
        let revision = self.revision.load(Ordering::Acquire);
        let history = self
            .application
            .history(String::new())
            .await
            .unwrap_or_default();
        let favorites = self
            .application
            .favorites(String::new())
            .await
            .unwrap_or_default();
        let devices = if self.application.is_authenticated().await {
            self.application.devices().await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let preferences = self.preference_snapshot();
        let provider = self.application.provider_preferences();
        let network = self.application.network_preferences();
        let profile = self.application.profile();
        let installation = self.application.installation();
        let main = MainUiSnapshot {
            revision,
            history: records(history),
            favorites: records(favorites),
            devices: devices
                .into_iter()
                .map(|device| DeviceSnapshot {
                    id: device.device_id.clone(),
                    name: device
                        .device_name
                        .unwrap_or_else(|| device.device_id.clone()),
                    platform: device.platform,
                    last_seen: format!("Unix {}", device.last_seen_at),
                    current: device.device_id == installation.device_id.to_string(),
                    revoked: device.revoked_at.is_some(),
                })
                .collect(),
            tokenhub_model: provider.tokenhub_model,
            tokenhub_configured: self.application.provider_configuration().unwrap_or(false),
            proxy_kind: match network.proxy_kind {
                ProxyKind::Http => 0,
                ProxyKind::Socks5 => 1,
            },
            proxy_address: network.proxy_address,
            provider_proxy_enabled: network.provider_proxy_enabled,
            update_proxy_enabled: network.update_proxy_enabled,
            server_url: profile
                .server_origin
                .unwrap_or_else(|| lvos::default_server_url().to_owned()),
            username: profile.username.unwrap_or_default(),
            current_device: installation.device_name,
            sync_status: read_lock(&self.sync_status),
            update_status: read_lock(&self.update_status),
            preferences,
        };
        UiSnapshot { revision, main }
    }

    pub(crate) async fn resume_session(self: Arc<Self>, ui: UiProcessClient) {
        if self.application.profile().user_id.is_none() {
            return;
        }
        let status = match self.application.resume_session().await {
            Ok(()) if self.application.is_authenticated().await => "Connected".to_owned(),
            Ok(()) => "Login required".to_owned(),
            Err(error) => {
                tracing::warn!(%error, "session restore failed");
                format!("Session restore failed: {error}")
            }
        };
        set_lock(&self.sync_status, status.clone());
        let patch = self.patch(|patch| patch.sync_status = Some(status)).await;
        let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
    }

    pub(crate) async fn startup_update_check(self: Arc<Self>, ui: UiProcessClient) {
        let result = self.update.startup_check(current_timestamp()).await;
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
        if let Some(status) = status {
            set_lock(&self.update_status, status.clone());
            let patch = self.patch(|patch| patch.update_status = Some(status)).await;
            let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
        }
    }

    pub(crate) async fn run_requests(
        self: Arc<Self>,
        mut inbound: mpsc::UnboundedReceiver<UiToAgent>,
        ui: UiProcessClient,
    ) {
        while let Some(message) = inbound.recv().await {
            match message {
                UiToAgent::Request {
                    request_id,
                    operation,
                } => {
                    self.handle_request(&ui, request_id, operation).await;
                }
                UiToAgent::LookupDismissed { .. }
                | UiToAgent::LookupPresentationApplied { .. }
                | UiToAgent::Ready(_)
                | UiToAgent::Hello(_)
                | UiToAgent::Exiting { .. } => {}
                UiToAgent::UiBlockingChanged { blocked } => {
                    self.ui_blocked.store(blocked, Ordering::Release);
                }
                UiToAgent::RequestIdleExit {
                    request_id,
                    process_generation,
                } => {
                    ui.approve_idle_exit(request_id, process_generation).await;
                }
                UiToAgent::CancelIdleExit {
                    request_id,
                    process_generation,
                    ..
                } => {
                    ui.cancel_idle_exit(request_id, process_generation).await;
                }
            }
        }
    }

    #[must_use]
    pub(crate) fn ui_is_blocking(&self) -> bool {
        self.ui_blocked.load(Ordering::Acquire)
    }

    async fn handle_request(&self, ui: &UiProcessClient, request_id: Uuid, operation: UiOperation) {
        let operation_name = operation.name();
        let outcome = self.execute(ui, operation).await;
        let (success, message, output) = match outcome {
            Ok((message, output)) => (true, message, output),
            Err(message) => {
                tracing::warn!(operation = ?operation_name, error = %message, "UI operation failed");
                (false, message, OperationOutput::None)
            }
        };
        let result = OperationResult {
            response_to: request_id,
            operation: operation_name,
            success,
            feedback: UiFeedback {
                message,
                level: if success {
                    FeedbackLevel::Success
                } else {
                    FeedbackLevel::Error
                },
            },
            output,
        };
        let _ = ui.send_if_ready(AgentToUi::OperationResult(result)).await;
    }

    #[allow(clippy::too_many_lines)]
    async fn execute(
        &self,
        ui: &UiProcessClient,
        operation: UiOperation,
    ) -> Result<(String, OperationOutput), String> {
        match operation {
            UiOperation::HistorySearch { term } => {
                let values = self.application.history(term).await.map_err(err)?;
                let patch = self
                    .patch(|patch| patch.history = Some(records(values)))
                    .await;
                let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                ok("")
            }
            UiOperation::FavoritesSearch { term } => {
                let values = self.application.favorites(term).await.map_err(err)?;
                let patch = self
                    .patch(|patch| patch.favorites = Some(records(values)))
                    .await;
                let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                ok("")
            }
            UiOperation::SetFavorite { key, active } => {
                self.application
                    .set_favorite(key, active)
                    .await
                    .map_err(err)?;
                self.refresh_collections(ui).await?;
                ok("Favorite updated.")
            }
            UiOperation::ClearHistory => {
                self.application.clear_history().await.map_err(err)?;
                self.refresh_collections(ui).await?;
                ok("History cleared.")
            }
            UiOperation::SaveProviderSettings {
                tokenhub_model,
                tokenhub_key,
                provider_proxy_enabled,
            } => {
                self.application
                    .save_provider_settings(
                        ProviderPreferences { tokenhub_model },
                        tokenhub_key.expose(),
                    )
                    .map_err(err)?;
                let network = self.application.network_preferences();
                self.application
                    .save_network_preferences(NetworkPreferences {
                        provider_proxy_enabled,
                        ..network
                    })
                    .map_err(err)?;
                let provider = self.application.provider_preferences();
                let configured = self.application.provider_configuration().map_err(err)?;
                let patch = self
                    .patch(|patch| {
                        patch.tokenhub_model = Some(provider.tokenhub_model);
                        patch.tokenhub_configured = Some(configured);
                        patch.provider_proxy_enabled = Some(provider_proxy_enabled);
                    })
                    .await;
                let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                ok("Provider settings saved.")
            }
            UiOperation::SaveNetworkSettings {
                proxy_address,
                proxy_kind,
                provider_proxy_enabled,
                update_proxy_enabled,
            } => {
                let preferences = NetworkPreferences {
                    proxy_kind: if proxy_kind == 1 {
                        ProxyKind::Socks5
                    } else {
                        ProxyKind::Http
                    },
                    proxy_address,
                    provider_proxy_enabled,
                    update_proxy_enabled,
                };
                self.application
                    .save_network_preferences(preferences.clone())
                    .map_err(err)?;
                self.apply_update_proxy(&preferences)?;
                let patch = self
                    .patch(|patch| {
                        patch.proxy_kind = Some(proxy_kind);
                        patch.proxy_address = Some(preferences.proxy_address);
                        patch.provider_proxy_enabled = Some(provider_proxy_enabled);
                        patch.update_proxy_enabled = Some(update_proxy_enabled);
                    })
                    .await;
                let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                ok("Network settings saved.")
            }
            UiOperation::TestProvider {
                tokenhub_model,
                tokenhub_key,
            } => {
                self.application
                    .test_provider(&tokenhub_model, tokenhub_key.expose())
                    .await
                    .map_err(|error| format!("Provider test failed: {error}"))?;
                ok("Provider test succeeded.")
            }
            UiOperation::Login {
                server,
                username,
                password,
            } => {
                set_lock(&self.sync_status, "Signing in…".to_owned());
                self.application
                    .login(server, username, password.expose().to_owned())
                    .await
                    .map_err(|error| format!("Login failed: {error}"))?;
                set_lock(&self.sync_status, "Connected".to_owned());
                self.refresh_account(ui).await?;
                self.refresh_collections(ui).await?;
                ok("Signed in.")
            }
            UiOperation::Logout => {
                let result = self.application.logout().await;
                set_lock(&self.sync_status, "Login required".to_owned());
                self.refresh_account(ui).await?;
                match result {
                    Ok(()) => ok("Signed out."),
                    Err(error) => ok(&format!("Signed out locally; the Server returned: {error}")),
                }
            }
            UiOperation::ManualSync => {
                if self.application.manual_sync().await {
                    set_lock(&self.sync_status, "Sync requested".to_owned());
                    let patch = self
                        .patch(|patch| patch.sync_status = Some("Sync requested".to_owned()))
                        .await;
                    let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                    ok("Sync requested.")
                } else {
                    Err("Login required.".to_owned())
                }
            }
            UiOperation::TestConnection { server } => {
                self.application
                    .test_connection(&server)
                    .await
                    .map_err(|error| format!("Server check failed: {error}"))?;
                ok("Server compatibility check succeeded.")
            }
            UiOperation::RevokeDevice { device_id } => {
                let current = device_id == self.application.installation().device_id.to_string();
                self.application
                    .revoke_device(&device_id)
                    .await
                    .map_err(|error| format!("Device revoke failed: {error}"))?;
                if current {
                    let _ = self.application.logout().await;
                    set_lock(
                        &self.sync_status,
                        "Device revoked · Login required".to_owned(),
                    );
                    self.refresh_account(ui).await?;
                } else {
                    self.refresh_devices(ui).await?;
                }
                ok("Device revoked.")
            }
            UiOperation::RegenerateDeviceIdentity => {
                self.application
                    .recover_revoked_device()
                    .await
                    .map_err(|error| format!("Device recovery failed: {error}"))?;
                set_lock(
                    &self.sync_status,
                    "Device identity replaced · Login again".to_owned(),
                );
                self.refresh_account(ui).await?;
                ok("Device identity replaced.")
            }
            UiOperation::ExportData { path } => {
                let bytes = self
                    .application
                    .export_portable_json()
                    .await
                    .map_err(|error| format!("Export failed: {error}"))?;
                write_file(path, bytes)
                    .await
                    .map_err(|error| format!("Export failed: {error}"))?;
                ok("Export completed.")
            }
            UiOperation::PreviewImport { path } => {
                let bytes = read_portable_file(path)
                    .await
                    .map_err(|error| format!("Import read failed: {error}"))?;
                let plan = self
                    .application
                    .preview_portable_import(bytes)
                    .await
                    .map_err(|error| format!("Import validation failed: {error}"))?;
                let preview = plan.preview();
                let token = Uuid::new_v4();
                let mut imports = self.imports.lock().await;
                imports.clear();
                imports.insert(token, plan);
                Ok((
                    "Import preview ready.".to_owned(),
                    OperationOutput::ImportPreview {
                        token,
                        history_add: preview.history_add,
                        history_update: preview.history_update,
                        favorite_add: preview.favorite_add,
                        favorite_reactivate: preview.favorite_reactivate,
                        query_stats_archive: preview.query_stats_archive,
                    },
                ))
            }
            UiOperation::ApplyImport { token } => {
                let plan =
                    self.imports.lock().await.remove(&token).ok_or_else(|| {
                        "Import preview expired; choose the file again.".to_owned()
                    })?;
                let result = self
                    .application
                    .apply_portable_import(plan)
                    .await
                    .map_err(|error| format!("Import failed: {error}"))?;
                self.refresh_collections(ui).await?;
                ok(&format!(
                    "Import completed: {} History added, {} Favorites added/reactivated.",
                    result.history_add,
                    result
                        .favorite_add
                        .saturating_add(result.favorite_reactivate)
                ))
            }
            UiOperation::CheckUpdate => {
                let status = self.manual_update_check().await;
                let success = !status.starts_with("Update check failed");
                set_lock(&self.update_status, status.clone());
                let patch = self
                    .patch(|patch| patch.update_status = Some(status.clone()))
                    .await;
                let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
                if success { ok(&status) } else { Err(status) }
            }
            UiOperation::UpdateGlobalHotkey { shortcut } => {
                self.native_request(|response| NativeAgentEvent::UpdateGlobalHotkey {
                    shortcut: shortcut.clone(),
                    response,
                })
                .await?;
                self.preference_store()
                    .save_text("global-hotkey", shortcut.trim())
                    .map_err(err)?;
                set_lock(&self.global_hotkey, shortcut);
                self.push_preferences(ui).await;
                ok("Shortcut saved.")
            }
            UiOperation::UpdateStartAtLogin { enabled } => {
                self.native_request(|response| NativeAgentEvent::UpdateStartAtLogin {
                    enabled,
                    response,
                })
                .await?;
                self.push_preferences(ui).await;
                ok("Startup preference saved.")
            }
            UiOperation::UpdateLaunchMinimized { enabled } => {
                self.preference_store()
                    .save_boolean(lvos::LAUNCH_MINIMIZED_KEY, enabled)
                    .map_err(err)?;
                write_lock(&self.preferences).launch_minimized = enabled;
                self.push_preferences(ui).await;
                ok("Launch preference saved.")
            }
            UiOperation::UpdatePopupIdleTimeout { seconds } => {
                UiPreferences::save_popup_idle_timeout(&self.preference_store(), seconds)
                    .map_err(err)?;
                write_lock(&self.preferences).popup_idle_timeout_secs = seconds;
                self.push_preferences(ui).await;
                ok("Lookup popup retention saved.")
            }
            UiOperation::PopupFavoriteToggle { active } => {
                self.application
                    .set_last_favorite(active)
                    .await
                    .map_err(err)?;
                ok("Favorite updated.")
            }
            UiOperation::PopupRefresh { display_session_id } => {
                if let Some(state) = self.application.refresh_last().await
                    && self.application.is_current(&state)
                {
                    let _ = ui
                        .send_if_ready(AgentToUi::UpdateLookup {
                            display_session_id,
                            state: lookup_state(state)?,
                        })
                        .await;
                }
                ok("")
            }
            UiOperation::RequestSnapshot => {
                let snapshot = self.snapshot().await;
                if !ui.send_if_ready(AgentToUi::Snapshot(snapshot)).await {
                    return Err("The GUI disconnected before snapshot recovery.".to_owned());
                }
                ok("")
            }
            UiOperation::OpenAccessibilitySettings => {
                self.native_request(NativeAgentEvent::OpenAccessibilitySettings)
                    .await?;
                ok("Accessibility settings opened.")
            }
            UiOperation::RequestAccessibilityPermission => {
                self.native_request(NativeAgentEvent::RequestAccessibilityPermission)
                    .await?;
                ok("Accessibility permission is available.")
            }
            UiOperation::RestartAgent => {
                self.native_request(NativeAgentEvent::RestartAgent).await?;
                ok("LVOS Agent is restarting.")
            }
        }
    }

    async fn refresh_collections(&self, ui: &UiProcessClient) -> Result<(), String> {
        let history = self.application.history(String::new()).await.map_err(err)?;
        let favorites = self
            .application
            .favorites(String::new())
            .await
            .map_err(err)?;
        let patch = self
            .patch(|patch| {
                patch.history = Some(records(history));
                patch.favorites = Some(records(favorites));
            })
            .await;
        let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
        Ok(())
    }

    async fn refresh_account(&self, ui: &UiProcessClient) -> Result<(), String> {
        let profile = self.application.profile();
        let provider = self.application.provider_preferences();
        let configured = self.application.provider_configuration().map_err(err)?;
        let status = read_lock(&self.sync_status);
        let patch = self
            .patch(|patch| {
                patch.server_url = Some(
                    profile
                        .server_origin
                        .unwrap_or_else(|| lvos::default_server_url().to_owned()),
                );
                patch.username = Some(profile.username.unwrap_or_default());
                patch.tokenhub_model = Some(provider.tokenhub_model);
                patch.tokenhub_configured = Some(configured);
                patch.sync_status = Some(status);
            })
            .await;
        let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
        self.refresh_devices(ui).await
    }

    async fn refresh_devices(&self, ui: &UiProcessClient) -> Result<(), String> {
        let installation = self.application.installation();
        let devices = if self.application.is_authenticated().await {
            self.application.devices().await.map_err(err)?
        } else {
            Vec::new()
        };
        let devices = devices
            .into_iter()
            .map(|device| DeviceSnapshot {
                id: device.device_id.clone(),
                name: device
                    .device_name
                    .unwrap_or_else(|| device.device_id.clone()),
                platform: device.platform,
                last_seen: format!("Unix {}", device.last_seen_at),
                current: device.device_id == installation.device_id.to_string(),
                revoked: device.revoked_at.is_some(),
            })
            .collect();
        let patch = self.patch(|patch| patch.devices = Some(devices)).await;
        let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
        Ok(())
    }

    async fn push_preferences(&self, ui: &UiProcessClient) {
        let preferences = self.preference_snapshot();
        let patch = self
            .patch(|patch| patch.preferences = Some(preferences))
            .await;
        let _ = ui.send_if_ready(AgentToUi::Patch(patch)).await;
    }

    async fn patch(&self, build: impl FnOnce(&mut MainUiPatch)) -> MainUiPatch {
        let _order = self.state_order.lock().await;
        let base_revision = self.revision.load(Ordering::Acquire);
        let revision = base_revision.wrapping_add(1);
        let mut patch = MainUiPatch {
            base_revision,
            revision,
            ..MainUiPatch::default()
        };
        build(&mut patch);
        self.revision.store(revision, Ordering::Release);
        patch
    }

    fn preference_snapshot(&self) -> UiPreferenceSnapshot {
        let preferences = *read_guard(&self.preferences);
        UiPreferenceSnapshot {
            popup_idle_timeout_secs: preferences.popup_idle_timeout_secs,
            launch_minimized: preferences.launch_minimized,
            global_hotkey: read_lock(&self.global_hotkey),
            start_at_login: start_at_login_enabled(),
        }
    }

    fn preference_store(&self) -> LocalPreferenceStore {
        LocalPreferenceStore::new(&self.data_root)
    }

    fn apply_update_proxy(&self, network: &NetworkPreferences) -> Result<(), String> {
        let proxy = if network.update_proxy_enabled {
            network.proxy_url().map_err(err)?
        } else {
            None
        };
        self.update_transport
            .set_proxy_url(proxy.as_deref())
            .map_err(err)
    }

    async fn manual_update_check(&self) -> String {
        match self.update.manual_check(current_timestamp()).await {
            Ok(UpdateCheckOutcome::Available(info)) => {
                if self.update.open_available(&info).is_ok() {
                    format!(
                        "Version {} is available. GitHub Releases opened for manual download.",
                        info.version
                    )
                } else {
                    format!(
                        "Version {} is available, but the Release page could not be opened.",
                        info.version
                    )
                }
            }
            Ok(UpdateCheckOutcome::UpToDate(info)) => format!(
                "Current {} · {} · Up to date",
                info.current_version, info.channel
            ),
            Ok(UpdateCheckOutcome::Skipped) => format!(
                "Current {} · {} · Check skipped",
                lvos_core::SOFTWARE_VERSION,
                self.update_channel
            ),
            Err(error) => {
                tracing::warn!(%error, "manual update check failed");
                "Update check failed. Try again later.".to_owned()
            }
        }
    }

    async fn native_request(
        &self,
        event: impl FnOnce(oneshot::Sender<Result<(), String>>) -> NativeAgentEvent,
    ) -> Result<(), String> {
        let (response, receiver) = oneshot::channel();
        self.native_events
            .send_event(event(response))
            .map_err(|_| "The Agent event loop is unavailable.".to_owned())?;
        tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .map_err(|_| "The Agent did not apply the platform setting in time.".to_owned())?
            .map_err(|_| "The Agent stopped while applying the platform setting.".to_owned())?
    }
}

pub(crate) fn lookup_state(state: LookupCardState) -> Result<LookupUiState, String> {
    match state {
        LookupCardState::Hidden => Err("A hidden lookup state cannot be displayed.".to_owned()),
        LookupCardState::Loading { generation, source } => Ok(LookupUiState::Loading {
            query_id: generation,
            source,
        }),
        LookupCardState::Ready {
            generation,
            content_key,
            source,
            translation,
            favorite,
            effective_query_count,
        } => Ok(LookupUiState::Ready {
            query_id: generation,
            content_key: content_key.to_string(),
            source,
            translation,
            favorite,
            effective_query_count,
        }),
        LookupCardState::Error {
            generation,
            source,
            kind,
        } => Ok(LookupUiState::Error {
            query_id: generation,
            source,
            kind: match kind {
                lvos_translation::LookupCardErrorKind::ProviderConfigurationRequired => {
                    LookupErrorKind::ProviderConfigurationRequired
                }
                lvos_translation::LookupCardErrorKind::ProviderUnauthorized => {
                    LookupErrorKind::ProviderUnauthorized
                }
                lvos_translation::LookupCardErrorKind::TranslationUnavailable => {
                    LookupErrorKind::TranslationUnavailable
                }
                lvos_translation::LookupCardErrorKind::UnsupportedInput => {
                    LookupErrorKind::UnsupportedInput
                }
            },
        }),
    }
}

fn records(values: Vec<lvos::UiRecordData>) -> Vec<UiRecordSnapshot> {
    values
        .into_iter()
        .map(|record| UiRecordSnapshot {
            key: record.key.to_string(),
            source: record.source,
            translation: record.translation,
            count: record.count,
            favorite: record.favorite,
            metadata: record.metadata,
        })
        .collect()
}

async fn read_portable_file(path: PathBuf) -> std::io::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
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
    .map_err(std::io::Error::other)?
}

async fn write_file(path: PathBuf, bytes: Vec<u8>) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || std::fs::write(path, bytes))
        .await
        .map_err(std::io::Error::other)?
}

#[allow(clippy::unnecessary_wraps)]
fn ok(message: &str) -> Result<(String, OperationOutput), String> {
    Ok((message.to_owned(), OperationOutput::None))
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn current_timestamp() -> lvos_core::UnixTimestamp {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    lvos_core::UnixTimestamp::from_seconds(seconds)
}

fn read_guard<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_lock(lock: &RwLock<String>) -> String {
    read_guard(lock).clone()
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn set_lock<T>(lock: &RwLock<T>, value: T) {
    *write_lock(lock) = value;
}

fn start_at_login_enabled() -> bool {
    #[cfg(target_os = "windows")]
    return lvos_platform::windows::start_at_login_enabled();
    #[cfg(target_os = "macos")]
    return lvos_platform::macos::start_at_login_enabled();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    false
}
