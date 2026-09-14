use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, str::FromStr};

use lvos::{
    ConfirmationBroker, ConfirmationRequest, DeviceRecord, FeedbackKind, LookupCardState,
    MainWindow, UiProcessCoordinator, UiRecord, popup_idle_timeout_from_preset_index,
    popup_idle_timeout_preset_index,
};
use lvos_core::ContentKey;
use lvos_ipc::{
    AgentToUi, DeviceSnapshot, FeedbackLevel, LookupErrorKind, LookupUiState, MainUiPatch,
    MainUiSnapshot, OperationOutput, OperationResult, SensitiveString, UiFeedback, UiOperation,
    UiOperationName, UiRecordSnapshot, UiSnapshot, UiToAgent,
};
use lvos_translation::LookupCardErrorKind;
use slint::ComponentHandle;
use uuid::Uuid;

use crate::ui_ipc::UiRequestSender;

#[derive(Default)]
struct UiCache {
    main: Option<MainUiSnapshot>,
    revision: u64,
    last_sequence: u64,
    resync_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DisplaySession {
    id: Uuid,
    query_id: u64,
}

pub(crate) struct UiSession {
    ui: UiProcessCoordinator,
    requests: UiRequestSender,
    cache: Rc<RefCell<UiCache>>,
    display: Rc<RefCell<Option<DisplaySession>>>,
    pending: Rc<RefCell<HashMap<Uuid, UiOperationName>>>,
}

impl UiSession {
    pub(crate) fn install(ui: UiProcessCoordinator, requests: UiRequestSender) -> Rc<Self> {
        let session = Rc::new(Self {
            ui,
            requests,
            cache: Rc::new(RefCell::new(UiCache::default())),
            display: Rc::new(RefCell::new(None)),
            pending: Rc::new(RefCell::new(HashMap::new())),
        });
        session.install_host_hooks();
        session
    }

    fn install_host_hooks(self: &Rc<Self>) {
        let session = Rc::clone(self);
        self.ui.on_main_created(move |main, confirmations| {
            session.install_main_callbacks(&main, &confirmations);
            session.apply_cached_main(&main);
        });

        let session = Rc::clone(self);
        self.ui.on_popup_created(move |popup| {
            let requests = session.requests.clone();
            let popup_weak = popup.as_weak();
            popup.on_favorite_toggled(move || {
                let Some(popup) = popup_weak.upgrade() else {
                    return;
                };
                let _ = requests.request(UiOperation::PopupFavoriteToggle {
                    active: !popup.get_favorite(),
                });
            });

            let requests = session.requests.clone();
            let display = Rc::clone(&session.display);
            popup.on_refresh_requested(move || {
                if let Some(display) = *display.borrow() {
                    let _ = requests.request(UiOperation::PopupRefresh {
                        display_session_id: display.id,
                    });
                }
            });
        });

        let requests = self.requests.clone();
        let display = Rc::clone(&self.display);
        self.ui.on_popup_dismissed(move || {
            if let Some(display) = display.borrow_mut().take() {
                let _ = requests.event(UiToAgent::LookupDismissed {
                    display_session_id: display.id,
                });
            }
        });

        let requests = self.requests.clone();
        self.ui.on_permission_created(move |permission| {
            let sender = requests.clone();
            permission.on_open_settings(move || {
                let _ = sender.request(UiOperation::OpenAccessibilitySettings);
            });
            let sender = requests.clone();
            permission.on_check_again(move || {
                let _ = sender.request(UiOperation::RequestAccessibilityPermission);
            });
            let sender = requests.clone();
            permission.on_restart_requested(move || {
                let _ = sender.request(UiOperation::RestartAgent);
            });
        });
    }

    pub(crate) fn apply(&self, sequence: u64, message: AgentToUi) {
        {
            let mut cache = self.cache.borrow_mut();
            if sequence <= cache.last_sequence {
                tracing::warn!(
                    sequence,
                    last = cache.last_sequence,
                    "ignored stale GUI message"
                );
                return;
            }
            cache.last_sequence = sequence;
        }
        match message {
            AgentToUi::Snapshot(snapshot) => self.apply_snapshot(snapshot),
            AgentToUi::Patch(patch) => self.apply_patch(patch),
            AgentToUi::BeginLookup {
                display_session_id,
                state,
            } => self.begin_lookup(display_session_id, state),
            AgentToUi::UpdateLookup {
                display_session_id,
                state,
            } => self.update_lookup(display_session_id, state),
            AgentToUi::HideLookup { display_session_id } => {
                if self
                    .display
                    .borrow()
                    .is_some_and(|display| display.id == display_session_id)
                {
                    let _ = self.ui.hide_lookup_card();
                }
            }
            AgentToUi::OpenMainWindow => {
                if let Err(error) = self.ui.show_main_window() {
                    tracing::error!(%error, "failed to show management UI");
                }
            }
            AgentToUi::ShowPermission { status } => {
                let permission = self.ui.permission_window();
                permission.set_status_text(status.into());
                if let Err(error) = self.ui.show_permission_window() {
                    tracing::error!(%error, "failed to show permission UI");
                }
            }
            AgentToUi::OperationResult(result) => self.apply_operation_result(&result),
            AgentToUi::Shutdown => {
                let _ = slint::quit_event_loop();
            }
            AgentToUi::HandshakeAccepted(_) => {
                tracing::warn!("ignored handshake message after GUI readiness");
            }
        }
    }

    fn apply_snapshot(&self, snapshot: UiSnapshot) {
        if snapshot.revision != snapshot.main.revision {
            tracing::warn!(
                envelope_revision = snapshot.revision,
                state_revision = snapshot.main.revision,
                "ignored inconsistent UI snapshot"
            );
            self.request_resync();
            return;
        }
        let timeout = snapshot.main.preferences.popup_idle_timeout_secs;
        if let Err(error) = self.ui.set_popup_idle_timeout_secs(timeout) {
            tracing::warn!(%error, "ignored invalid Popup retention from Agent");
        }
        {
            let mut cache = self.cache.borrow_mut();
            cache.revision = snapshot.revision;
            cache.main = Some(snapshot.main);
            cache.resync_pending = false;
        }
        if self.ui.has_main_host() {
            let main = self.ui.main_window();
            self.apply_cached_main(&main);
        }
    }

    fn apply_patch(&self, patch: MainUiPatch) {
        let timeout = patch
            .preferences
            .as_ref()
            .map(|preferences| preferences.popup_idle_timeout_secs);
        let applied = {
            let mut cache = self.cache.borrow_mut();
            if patch.base_revision != cache.revision || patch.revision <= cache.revision {
                false
            } else if let Some(main) = cache.main.as_mut() {
                apply_main_patch(main, patch);
                cache.revision = main.revision;
                true
            } else {
                false
            }
        };
        if !applied {
            tracing::warn!(
                "UI patch did not follow the applied snapshot; requesting a replacement"
            );
            self.request_resync();
            return;
        }
        if let Some(timeout) = timeout
            && let Err(error) = self.ui.set_popup_idle_timeout_secs(timeout)
        {
            tracing::warn!(%error, "ignored invalid Popup retention patch");
        }
        if self.ui.has_main_host() {
            let main = self.ui.main_window();
            self.apply_cached_main(&main);
        }
    }

    fn request_resync(&self) {
        let should_request = {
            let mut cache = self.cache.borrow_mut();
            if cache.resync_pending {
                false
            } else {
                cache.resync_pending = true;
                true
            }
        };
        if should_request {
            let _ = self.requests.request(UiOperation::RequestSnapshot);
        }
    }

    fn begin_lookup(&self, display_session_id: Uuid, state: LookupUiState) {
        let query_id = state.query_id();
        match lookup_state(state) {
            Ok(state) => {
                *self.display.borrow_mut() = Some(DisplaySession {
                    id: display_session_id,
                    query_id,
                });
                if let Err(error) = self.ui.show_lookup_card(&state) {
                    tracing::error!(%error, "failed to show Lookup Card");
                }
            }
            Err(error) => tracing::warn!(%error, "ignored invalid lookup state"),
        }
    }

    fn update_lookup(&self, display_session_id: Uuid, state: LookupUiState) {
        let expected = DisplaySession {
            id: display_session_id,
            query_id: state.query_id(),
        };
        if self.display.borrow().as_ref() != Some(&expected) {
            tracing::debug!("ignored result for an inactive lookup display session");
            return;
        }
        match lookup_state(state) {
            Ok(state) => {
                if !self.ui.update_lookup_card_if_active(&state) {
                    self.display.borrow_mut().take();
                    tracing::debug!("ignored lookup result after Popup dismissal");
                }
            }
            Err(error) => tracing::warn!(%error, "ignored invalid lookup state"),
        }
    }

    fn apply_operation_result(&self, result: &OperationResult) {
        let pending_operation = self.pending.borrow_mut().remove(&result.response_to);
        if let Some(expected) = pending_operation
            && expected != result.operation
        {
            self.show_feedback(UiFeedback {
                message: "The Agent returned a mismatched operation response.".to_owned(),
                level: FeedbackLevel::Error,
            });
        }
        self.update_pending_indicator();
        if !result.feedback.message.is_empty() {
            self.show_feedback(result.feedback.clone());
        }
        if result.success
            && let OperationOutput::ImportPreview {
                token,
                history_add,
                history_update,
                favorite_add,
                favorite_reactivate,
                query_stats_archive,
            } = &result.output
            && self.ui.has_main_host()
        {
            let token = *token;
            let confirmations = self.ui.confirmations();
            let epoch = confirmations.epoch();
            let session = self.clone_handles();
            let request = ConfirmationRequest {
                title: "Import LVOS data?".to_owned(),
                message: format!(
                    "History: {history_add} add, {history_update} update. Favorites: {favorite_add} add, {favorite_reactivate} reactivate. QueryStats archive: {query_stats_archive} records. Nothing changes until you choose Import."
                ),
                primary: "Import".to_owned(),
                focus_target: 3,
                device_id: String::new(),
            };
            if let Err(error) = slint::spawn_local(async move {
                if confirmations.confirm(epoch, request).await {
                    session.submit(UiOperation::ApplyImport { token });
                } else {
                    session.show_feedback(UiFeedback {
                        message: "Import cancelled; no data changed.".to_owned(),
                        level: FeedbackLevel::Info,
                    });
                }
            }) {
                tracing::warn!(%error, "failed to schedule import confirmation");
            }
        }
    }

    fn install_main_callbacks(&self, main: &Rc<MainWindow>, confirmations: &ConfirmationBroker) {
        let sender = self.requests.clone();
        main.on_history_search(move |term| {
            let _ = sender.request(UiOperation::HistorySearch {
                term: term.to_string(),
            });
        });
        let sender = self.requests.clone();
        main.on_favorites_search(move |term| {
            let _ = sender.request(UiOperation::FavoritesSearch {
                term: term.to_string(),
            });
        });

        let session = self.clone_handles();
        main.on_favorite_toggled(move |key, currently_active| {
            session.submit(UiOperation::SetFavorite {
                key: key.to_string(),
                active: !currently_active,
            });
        });
        let session = self.clone_handles();
        main.on_clear_history_requested(move || session.submit(UiOperation::ClearHistory));
        let session = self.clone_handles();
        main.on_persist_provider_settings(move |model, key, proxy| {
            session.submit(UiOperation::SaveProviderSettings {
                tokenhub_model: model.to_string(),
                tokenhub_key: SensitiveString::new(key.to_string()),
                provider_proxy_enabled: proxy,
            });
        });
        let session = self.clone_handles();
        main.on_persist_network_settings(move |address, kind, provider, update| {
            session.submit(UiOperation::SaveNetworkSettings {
                proxy_address: address.to_string(),
                proxy_kind: u8::try_from(kind).unwrap_or_default(),
                provider_proxy_enabled: provider,
                update_proxy_enabled: update,
            });
        });
        let session = self.clone_handles();
        main.on_test_provider(move |model, key| {
            session.submit(UiOperation::TestProvider {
                tokenhub_model: model.to_string(),
                tokenhub_key: SensitiveString::new(key.to_string()),
            });
        });
        let session = self.clone_handles();
        main.on_login_requested(move |server, username, password| {
            session.submit(UiOperation::Login {
                server: server.to_string(),
                username: username.to_string(),
                password: SensitiveString::new(password.to_string()),
            });
        });
        let session = self.clone_handles();
        main.on_logout_requested(move || session.submit(UiOperation::Logout));
        let session = self.clone_handles();
        main.on_manual_sync_requested(move || session.submit(UiOperation::ManualSync));
        let session = self.clone_handles();
        let weak = main.as_weak();
        main.on_test_connection_requested(move || {
            if let Some(main) = weak.upgrade() {
                session.submit(UiOperation::TestConnection {
                    server: main.get_server_url().to_string(),
                });
            }
        });

        self.install_confirmation_callbacks(main, confirmations);
        self.install_file_callbacks(main);

        let session = self.clone_handles();
        main.on_check_update_requested(move || session.submit(UiOperation::CheckUpdate));
        let session = self.clone_handles();
        main.on_update_global_hotkey(move |shortcut| {
            session.submit(UiOperation::UpdateGlobalHotkey {
                shortcut: shortcut.to_string(),
            });
        });
        let session = self.clone_handles();
        main.on_update_start_at_login(move |enabled| {
            session.submit(UiOperation::UpdateStartAtLogin { enabled });
        });
        let session = self.clone_handles();
        main.on_update_launch_minimized(move |enabled| {
            session.submit(UiOperation::UpdateLaunchMinimized { enabled });
        });
        let session = self.clone_handles();
        main.on_update_popup_idle_timeout(move |index| {
            if let Some(seconds) = popup_idle_timeout_from_preset_index(index) {
                session.submit(UiOperation::UpdatePopupIdleTimeout { seconds });
            } else {
                session.show_feedback(UiFeedback {
                    message: "Choose a supported Popup retention value.".to_owned(),
                    level: FeedbackLevel::Error,
                });
            }
        });

        let sender = self.requests.clone();
        confirmations.on_blocking_changed(move |blocked| {
            let _ = sender.event(UiToAgent::UiBlockingChanged { blocked });
        });
    }

    fn install_confirmation_callbacks(
        &self,
        main: &Rc<MainWindow>,
        confirmations: &ConfirmationBroker,
    ) {
        let session = self.clone_handles();
        let revoke_confirmations = confirmations.clone();
        let cache = Rc::clone(&self.cache);
        main.on_revoke_device_requested(move |device_id| {
            let device_id = device_id.to_string();
            let is_current = cache.borrow().main.as_ref().is_some_and(|main| {
                main.devices
                    .iter()
                    .any(|device| device.id == device_id && device.current)
            });
            if !is_current {
                session.submit(UiOperation::RevokeDevice { device_id });
                return;
            }
            let confirmations = revoke_confirmations.clone();
            let session = session.clone();
            let epoch = confirmations.epoch();
            let request = ConfirmationRequest {
                title: "Revoke this device?".to_owned(),
                message: "This logs out this installation. Its device identity remains revoked until you explicitly replace it.".to_owned(),
                primary: "Revoke".to_owned(),
                focus_target: 1,
                device_id: device_id.clone(),
            };
            let _ = slint::spawn_local(async move {
                if confirmations.confirm(epoch, request).await {
                    session.submit(UiOperation::RevokeDevice { device_id });
                }
            });
        });

        let session = self.clone_handles();
        let confirmations = confirmations.clone();
        main.on_regenerate_device_identity_requested(move || {
            let confirmations = confirmations.clone();
            let session = session.clone();
            let epoch = confirmations.epoch();
            let request = ConfirmationRequest {
                title: "Replace device identity?".to_owned(),
                message: "Creates a new installation ID and removes old sessions. Profiles and pending Outbox data are preserved. Continue only after this installation was revoked.".to_owned(),
                primary: "Replace".to_owned(),
                focus_target: 2,
                device_id: String::new(),
            };
            let _ = slint::spawn_local(async move {
                if confirmations.confirm(epoch, request).await {
                    session.submit(UiOperation::RegenerateDeviceIdentity);
                }
            });
        });
    }

    fn install_file_callbacks(&self, main: &Rc<MainWindow>) {
        let session = self.clone_handles();
        main.on_export_data_requested(move || {
            let session = session.clone();
            let _ = slint::spawn_local(async move {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .add_filter("LVOS Portable JSON", &["json"])
                    .set_file_name("lvos-export.json")
                    .save_file()
                    .await
                else {
                    return;
                };
                session.submit(UiOperation::ExportData {
                    path: PathBuf::from(file.path()),
                });
            });
        });

        let session = self.clone_handles();
        main.on_import_data_requested(move || {
            let session = session.clone();
            let _ = slint::spawn_local(async move {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .add_filter("LVOS Portable JSON", &["json"])
                    .pick_file()
                    .await
                else {
                    return;
                };
                session.submit(UiOperation::PreviewImport {
                    path: PathBuf::from(file.path()),
                });
            });
        });
    }

    fn clone_handles(&self) -> UiSessionHandles {
        UiSessionHandles {
            ui: self.ui.clone(),
            requests: self.requests.clone(),
            pending: Rc::clone(&self.pending),
        }
    }

    fn update_pending_indicator(&self) {
        if self.ui.has_main_host() {
            self.ui
                .main_window()
                .set_operation_pending(!self.pending.borrow().is_empty());
        }
    }

    fn show_feedback(&self, feedback: UiFeedback) {
        if self.ui.has_main_host() {
            set_feedback(&self.ui.main_window(), feedback);
        }
    }

    fn apply_cached_main(&self, main: &MainWindow) {
        if let Some(snapshot) = self.cache.borrow().main.as_ref() {
            apply_main_snapshot(main, snapshot);
        }
        main.set_operation_pending(!self.pending.borrow().is_empty());
    }
}

#[derive(Clone)]
struct UiSessionHandles {
    ui: UiProcessCoordinator,
    requests: UiRequestSender,
    pending: Rc<RefCell<HashMap<Uuid, UiOperationName>>>,
}

impl UiSessionHandles {
    fn submit(&self, operation: UiOperation) {
        let name = operation.name();
        match self.requests.request(operation) {
            Ok(request_id) => {
                self.pending.borrow_mut().insert(request_id, name);
                if self.ui.has_main_host() {
                    self.ui.main_window().set_operation_pending(true);
                }
            }
            Err(error) => self.show_feedback(UiFeedback {
                message: format!("The Agent connection is unavailable: {error}"),
                level: FeedbackLevel::Error,
            }),
        }
    }

    fn show_feedback(&self, feedback: UiFeedback) {
        if self.ui.has_main_host() {
            set_feedback(&self.ui.main_window(), feedback);
        }
    }
}

fn apply_main_snapshot(main: &MainWindow, snapshot: &MainUiSnapshot) {
    main.set_history_records(slint::ModelRc::new(slint::VecModel::from(
        snapshot.history.iter().map(ui_record).collect::<Vec<_>>(),
    )));
    main.set_favorite_records(slint::ModelRc::new(slint::VecModel::from(
        snapshot.favorites.iter().map(ui_record).collect::<Vec<_>>(),
    )));
    main.set_devices(slint::ModelRc::new(slint::VecModel::from(
        snapshot
            .devices
            .iter()
            .map(device_record)
            .collect::<Vec<_>>(),
    )));
    main.set_tokenhub_model(snapshot.tokenhub_model.as_str().into());
    main.set_tokenhub_configured(snapshot.tokenhub_configured);
    main.set_proxy_kind(i32::from(snapshot.proxy_kind));
    main.set_proxy_address(snapshot.proxy_address.as_str().into());
    main.set_provider_proxy_enabled(snapshot.provider_proxy_enabled);
    main.set_update_proxy_enabled(snapshot.update_proxy_enabled);
    main.set_server_url(snapshot.server_url.as_str().into());
    main.set_username(snapshot.username.as_str().into());
    main.set_current_device(snapshot.current_device.as_str().into());
    main.set_sync_status(snapshot.sync_status.as_str().into());
    main.set_update_status(snapshot.update_status.as_str().into());
    main.set_global_hotkey(snapshot.preferences.global_hotkey.as_str().into());
    main.set_start_at_login(snapshot.preferences.start_at_login);
    main.set_launch_minimized(snapshot.preferences.launch_minimized);
    main.set_popup_idle_timeout_index(popup_idle_timeout_preset_index(
        snapshot.preferences.popup_idle_timeout_secs,
    ));
}

fn apply_main_patch(main: &mut MainUiSnapshot, patch: MainUiPatch) {
    main.revision = patch.revision;
    if let Some(value) = patch.history {
        main.history = value;
    }
    if let Some(value) = patch.favorites {
        main.favorites = value;
    }
    if let Some(value) = patch.devices {
        main.devices = value;
    }
    if let Some(value) = patch.tokenhub_model {
        main.tokenhub_model = value;
    }
    if let Some(value) = patch.tokenhub_configured {
        main.tokenhub_configured = value;
    }
    if let Some(value) = patch.proxy_kind {
        main.proxy_kind = value;
    }
    if let Some(value) = patch.proxy_address {
        main.proxy_address = value;
    }
    if let Some(value) = patch.provider_proxy_enabled {
        main.provider_proxy_enabled = value;
    }
    if let Some(value) = patch.update_proxy_enabled {
        main.update_proxy_enabled = value;
    }
    if let Some(value) = patch.server_url {
        main.server_url = value;
    }
    if let Some(value) = patch.username {
        main.username = value;
    }
    if let Some(value) = patch.current_device {
        main.current_device = value;
    }
    if let Some(value) = patch.sync_status {
        main.sync_status = value;
    }
    if let Some(value) = patch.update_status {
        main.update_status = value;
    }
    if let Some(value) = patch.preferences {
        main.preferences = value;
    }
    if let Some(value) = patch.feedback {
        tracing::debug!(message = %value.message, "Agent state patch included UI feedback");
    }
}

fn ui_record(record: &UiRecordSnapshot) -> UiRecord {
    UiRecord {
        key: record.key.as_str().into(),
        source: record.source.as_str().into(),
        translation: record.translation.as_str().into(),
        count: i32::try_from(record.count).unwrap_or(i32::MAX),
        favorite: record.favorite,
        metadata: record.metadata.as_str().into(),
    }
}

fn device_record(device: &DeviceSnapshot) -> DeviceRecord {
    DeviceRecord {
        id: device.id.as_str().into(),
        name: device.name.as_str().into(),
        platform: device.platform.as_str().into(),
        last_seen: device.last_seen.as_str().into(),
        current: device.current,
        revoked: device.revoked,
    }
}

fn lookup_state(state: LookupUiState) -> Result<LookupCardState, &'static str> {
    Ok(match state {
        LookupUiState::Loading { query_id, source } => LookupCardState::Loading {
            generation: query_id,
            source,
        },
        LookupUiState::Ready {
            query_id,
            content_key,
            source,
            translation,
            favorite,
            effective_query_count,
        } => LookupCardState::Ready {
            generation: query_id,
            content_key: ContentKey::from_str(&content_key).map_err(|_| "invalid content key")?,
            source,
            translation,
            favorite,
            effective_query_count,
        },
        LookupUiState::Error {
            query_id,
            source,
            kind,
        } => LookupCardState::Error {
            generation: query_id,
            source,
            kind: match kind {
                LookupErrorKind::ProviderConfigurationRequired => {
                    LookupCardErrorKind::ProviderConfigurationRequired
                }
                LookupErrorKind::ProviderUnauthorized => LookupCardErrorKind::ProviderUnauthorized,
                LookupErrorKind::TranslationUnavailable => {
                    LookupCardErrorKind::TranslationUnavailable
                }
                LookupErrorKind::UnsupportedInput => LookupCardErrorKind::UnsupportedInput,
            },
        },
    })
}

fn set_feedback(main: &MainWindow, feedback: UiFeedback) {
    main.set_settings_feedback_kind(match feedback.level {
        FeedbackLevel::Info => FeedbackKind::Info,
        FeedbackLevel::Success => FeedbackKind::Success,
        FeedbackLevel::Error => FeedbackKind::Error,
    });
    main.set_settings_error(feedback.message.into());
}
