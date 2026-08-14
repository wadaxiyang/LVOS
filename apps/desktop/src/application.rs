use std::{
    error::Error,
    fmt, fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use lvos_auth::{CredentialKey, CredentialScope, CredentialStore};
use lvos_core::{
    DEFAULT_SERVER_URL, LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content,
};
use lvos_storage::{
    InstallationMetadata, InstallationStore, Platform as StoragePlatform, PortableImportPlan,
    PortableImportResult, ProfileMetadata,
};
use lvos_translation::{
    CredentialReader, GoogleBasicV2Provider, LookupCardErrorKind, ProviderId, ProviderRegistry,
    ReqwestTransport, RouterSettings, TencentTokenHubProvider, TimeoutConfig, TranslationProvider,
    TranslationRequest, TranslationRouter, validate_tokenhub_model,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    AuthenticatedSession, DatabaseWorker, HttpSyncTransport, LoginCredentials, LookupCardState,
    LookupMode, LookupService, RemoteDevice, SessionError, SyncEngine, SyncWorker,
    SyncWorkerHandle, TransportError, UiDataError, UiDataService, UiRecordData,
};

const MAX_LOOKUP_BYTES: usize = 2_000;
const HISTORY_PAGE_LIMIT: u32 = 200;
const PROVIDER_SCOPE_ORIGIN: &str = "lvos://translation-provider";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPreferences {
    pub primary: String,
    pub fallback: Option<String>,
    #[serde(default = "default_tokenhub_model")]
    pub tokenhub_model: String,
}

impl Default for ProviderPreferences {
    fn default() -> Self {
        Self {
            primary: lvos_translation::DEFAULT_PRIMARY_PROVIDER.to_owned(),
            fallback: Some(lvos_translation::DEFAULT_FALLBACK_PROVIDER.to_owned()),
            tokenhub_model: default_tokenhub_model(),
        }
    }
}

/// Production composition root for local Desktop lookup and data behavior.
///
/// Native capture and Slint stay outside this type. This boundary is `Send + Sync`, keeps secrets
/// in the OS Credential Store, owns one dedicated database worker, and exposes only bounded async
/// operations for UI callbacks.
pub struct DesktopApplication {
    root: PathBuf,
    installation: RwLock<InstallationMetadata>,
    database: Arc<DatabaseWorker>,
    data: UiDataService,
    credentials: Arc<dyn CredentialStore>,
    profile: RwLock<ProfileMetadata>,
    provider_preferences: RwLock<ProviderPreferences>,
    lookup: RwLock<Arc<LookupService>>,
    generation: AtomicU64,
    last_source: Mutex<Option<String>>,
    sync_transport: Arc<HttpSyncTransport>,
    sync: tokio::sync::Mutex<ActiveSync>,
}

#[derive(Default)]
struct ActiveSync {
    session: Option<Arc<AuthenticatedSession<HttpSyncTransport>>>,
    handle: Option<SyncWorkerHandle>,
    cancellation: Option<CancellationToken>,
    tasks: Vec<JoinHandle<()>>,
}

impl fmt::Debug for DesktopApplication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopApplication")
            .field("root", &self.root)
            .field("installation", &self.installation)
            .field("database", &self.database)
            .field("profile", &self.profile)
            .field("provider_preferences", &self.provider_preferences)
            .finish_non_exhaustive()
    }
}

impl DesktopApplication {
    /// Opens or creates the installation and one active local Profile, then restores local
    /// Provider configuration without reading secrets into persistence or diagnostic structures.
    /// Server session restoration is intentionally separate so startup never waits on a network.
    ///
    /// # Errors
    /// Returns a startup error for filesystem, installation, Profile, or Provider-state failures.
    pub async fn open(
        root: PathBuf,
        platform: StoragePlatform,
        device_name: &str,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Arc<Self>, ApplicationError> {
        fs::create_dir_all(&root)?;
        let installation = InstallationStore::new(&root).load_or_create(platform, device_name)?;
        let database = Arc::new(DatabaseWorker::start(root.clone())?);
        let profiles = database.profile_metadata().await?;
        let profile = profiles
            .into_iter()
            .find(|profile| profile.device_id == installation.device_id)
            .unwrap_or_else(|| new_unbound_profile(&installation));
        database.switch_profile(profile.clone()).await?;
        let preferences = read_provider_preferences(&provider_path(&root, profile.profile_id))?;
        let sync_transport = Arc::new(HttpSyncTransport::new()?);
        let application = Arc::new(Self {
            root,
            installation: RwLock::new(installation),
            data: UiDataService::new(Arc::clone(&database)),
            database: Arc::clone(&database),
            credentials,
            profile: RwLock::new(profile),
            provider_preferences: RwLock::new(preferences),
            lookup: RwLock::new(Arc::new(LookupService::new_without_provider(Arc::clone(
                &database,
            )))),
            generation: AtomicU64::new(0),
            last_source: Mutex::new(None),
            sync_transport,
            sync: tokio::sync::Mutex::new(ActiveSync::default()),
        });
        application.rebuild_lookup_service()?;
        Ok(application)
    }

    #[must_use]
    pub fn database(&self) -> Arc<DatabaseWorker> {
        Arc::clone(&self.database)
    }

    #[must_use]
    pub fn installation(&self) -> InstallationMetadata {
        self.installation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn profile(&self) -> ProfileMetadata {
        self.profile
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn provider_preferences(&self) -> ProviderPreferences {
        self.provider_preferences
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Reports credential presence without returning either secret.
    ///
    /// # Errors
    /// Returns an error if the native Credential Store cannot be queried.
    pub fn provider_configuration(&self) -> Result<(bool, bool), ApplicationError> {
        let scope = self.provider_scope(CredentialKey::TencentTokenHubApiKey);
        let tokenhub = self.credentials.contains(&scope)?;
        let scope = self.provider_scope(CredentialKey::GoogleApiKey);
        let google = self.credentials.contains(&scope)?;
        Ok((tokenhub, google))
    }

    /// Persists non-secret Provider order and optional replacement API keys.
    ///
    /// Empty key fields retain existing credentials. Selected Providers must be configured and
    /// distinct before preferences are committed.
    ///
    /// # Errors
    /// Returns an error for invalid selections, Credential Store failure, or atomic file failure.
    pub fn save_provider_settings(
        &self,
        mut preferences: ProviderPreferences,
        tokenhub_key: &str,
        google_key: &str,
    ) -> Result<(), ApplicationError> {
        preferences.tokenhub_model = validated_tokenhub_model(&preferences.tokenhub_model)?;
        validate_provider_id(&preferences.primary)?;
        if let Some(fallback) = &preferences.fallback {
            validate_provider_id(fallback)?;
            if fallback == &preferences.primary {
                return Err(ApplicationError::ProviderSettings(
                    "Primary and Fallback Providers must be different",
                ));
            }
        }
        let tokenhub_key = tokenhub_key.trim();
        let google_key = google_key.trim();
        let (stored_tokenhub, stored_google) = self.provider_configuration()?;
        ensure_selected_configured(
            &preferences,
            stored_tokenhub || !tokenhub_key.is_empty(),
            stored_google || !google_key.is_empty(),
        )?;
        if !tokenhub_key.is_empty() {
            self.credentials.set(
                &self.provider_scope(CredentialKey::TencentTokenHubApiKey),
                tokenhub_key.as_bytes(),
            )?;
        }
        if !google_key.is_empty() {
            self.credentials.set(
                &self.provider_scope(CredentialKey::GoogleApiKey),
                google_key.as_bytes(),
            )?;
        }
        write_provider_preferences(
            &provider_path(&self.root, self.profile().profile_id),
            &preferences,
        )?;
        *self
            .provider_preferences
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = preferences;
        self.rebuild_lookup_service()
    }

    /// Starts a latest-query-wins generation and returns its immediate loading state.
    pub fn begin_lookup(&self, source: String) -> LookupCardState {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut last) = self.last_source.lock() {
            *last = Some(source.clone());
        }
        LookupCardState::Loading { generation, source }
    }

    /// Completes a generation started by [`Self::begin_lookup`].
    pub async fn complete_lookup(
        &self,
        generation: u64,
        source: String,
        mode: LookupMode,
    ) -> LookupCardState {
        let policy = ValidationPolicy::new(
            NonZeroUsize::new(MAX_LOOKUP_BYTES)
                .unwrap_or_else(|| unreachable!("lookup limit is nonzero")),
        );
        let Ok(content) = prepare_content(&source, language("en"), policy) else {
            return LookupCardState::Error {
                generation,
                source,
                kind: LookupCardErrorKind::UnsupportedInput,
            };
        };
        let service = self
            .lookup
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let outcome = match service
            .lookup(content, language("zh-CN"), mode, now())
            .await
        {
            Ok(outcome) => outcome,
            Err(crate::LookupError::Translation(error)) => {
                return LookupCardState::Error {
                    generation,
                    source,
                    kind: error.lookup_card_kind(),
                };
            }
            Err(crate::LookupError::Database(error)) => {
                tracing::warn!(%error, "lookup persistence failed");
                return LookupCardState::Error {
                    generation,
                    source,
                    kind: LookupCardErrorKind::TranslationUnavailable,
                };
            }
        };
        let key = outcome.history.content.content_key;
        let favorite = self
            .database
            .execute(move |database| database.favorite_by_key(key))
            .await
            .ok()
            .flatten()
            .is_some_and(|favorite| favorite.deleted_at.is_none());
        LookupCardState::Ready {
            generation,
            content_key: key,
            source: outcome.history.content.source_text,
            translation: outcome.history.translation.translation,
            favorite,
            effective_query_count: outcome.query_stats.effective_total(),
        }
    }

    /// Runs a bounded lookup and returns a typed Lookup Card completion.
    pub async fn lookup(&self, source: String, mode: LookupMode) -> LookupCardState {
        let loading = self.begin_lookup(source.clone());
        self.complete_lookup(loading.generation().unwrap_or(0), source, mode)
            .await
    }

    #[must_use]
    pub fn is_current(&self, state: &LookupCardState) -> bool {
        state
            .generation()
            .is_some_and(|generation| generation == self.generation.load(Ordering::SeqCst))
    }

    pub async fn refresh_last(&self) -> Option<LookupCardState> {
        let source = self.last_source.lock().ok()?.clone()?;
        Some(self.lookup(source, LookupMode::Refresh).await)
    }

    /// Searches the active Profile's local History.
    ///
    /// # Errors
    /// Returns a database-worker or query error.
    pub async fn history(&self, term: String) -> Result<Vec<UiRecordData>, UiDataError> {
        self.data.history(term, HISTORY_PAGE_LIMIT).await
    }

    /// Searches the active Profile's local Favorites.
    ///
    /// # Errors
    /// Returns a database-worker or query error.
    pub async fn favorites(&self, term: String) -> Result<Vec<UiRecordData>, UiDataError> {
        self.data.favorites(term, HISTORY_PAGE_LIMIT).await
    }

    /// Applies a Favorite intent and wakes event-driven sync.
    ///
    /// # Errors
    /// Returns an invalid-key or database-worker error.
    pub async fn set_favorite(&self, key: String, active: bool) -> Result<bool, UiDataError> {
        let result = self.data.set_favorite(key, active, now()).await?;
        self.wake_sync().await;
        Ok(result)
    }

    /// Applies a Favorite intent to the most recent lookup.
    ///
    /// # Errors
    /// Returns an invalid-content or database-worker error.
    pub async fn set_last_favorite(&self, active: bool) -> Result<bool, UiDataError> {
        let source = self
            .last_source
            .lock()
            .ok()
            .and_then(|source| source.clone())
            .ok_or(UiDataError::InvalidContentKey)?;
        let content = prepare_content(
            &source,
            language("en"),
            ValidationPolicy::new(
                NonZeroUsize::new(MAX_LOOKUP_BYTES)
                    .unwrap_or_else(|| unreachable!("lookup limit is nonzero")),
            ),
        )
        .map_err(|_| UiDataError::InvalidContentKey)?;
        self.set_favorite(content.content_key().to_string(), active)
            .await
    }

    /// Clears non-Favorite History in the active Profile.
    ///
    /// # Errors
    /// Returns a database-worker error.
    pub async fn clear_history(&self) -> Result<(), UiDataError> {
        self.data.clear_history().await
    }

    /// Sends one direct request through the selected configured Provider.
    ///
    /// # Errors
    /// Returns a Provider-selection, credential, transport, or response error.
    pub async fn test_provider(
        &self,
        provider: &str,
        tokenhub_model: &str,
    ) -> Result<(), ApplicationError> {
        validate_provider_id(provider)?;
        let profile = self.profile();
        let profile_scope = profile.profile_id.to_string();
        let device_scope = profile.device_id.to_string();
        let reader = CredentialReader::new(
            self.credentials.as_ref(),
            PROVIDER_SCOPE_ORIGIN,
            &profile_scope,
            &device_scope,
        );
        let timeout = TimeoutConfig::default();
        let transport = Arc::new(
            ReqwestTransport::new(timeout)
                .map_err(|_| ApplicationError::ProviderSettings("HTTP transport unavailable"))?,
        );
        let request = TranslationRequest {
            text: "hello".to_owned(),
            source_language: language("en"),
            target_language: language("zh-CN"),
        };
        match provider {
            lvos_translation::DEFAULT_PRIMARY_PROVIDER => {
                let tokenhub_model = validated_tokenhub_model(tokenhub_model)?;
                TencentTokenHubProvider::new(transport, reader.tokenhub_api_key()?, timeout)
                    .with_model(&tokenhub_model)?
                    .translate(&request)
                    .await?;
            }
            lvos_translation::DEFAULT_FALLBACK_PROVIDER => {
                GoogleBasicV2Provider::new(transport, reader.google_api_key()?, timeout)
                    .translate(&request)
                    .await?;
            }
            _ => {
                return Err(ApplicationError::ProviderSettings(
                    "unknown translation Provider",
                ));
            }
        }
        Ok(())
    }

    /// Exports the active Profile's portable data as bounded JSON bytes.
    ///
    /// # Errors
    /// Returns a database-worker or serialization error.
    pub async fn export_portable_json(&self) -> Result<Vec<u8>, UiDataError> {
        self.data.export_portable_json().await
    }

    /// Validates portable JSON and constructs an immutable import preview.
    ///
    /// # Errors
    /// Returns a size, format, validation, or database-worker error.
    pub async fn preview_portable_import(
        &self,
        bytes: Vec<u8>,
    ) -> Result<PortableImportPlan, UiDataError> {
        self.data.preview_portable_import(bytes).await
    }

    /// Applies a previously validated portable import and wakes sync.
    ///
    /// # Errors
    /// Returns a stale-plan, transaction, or database-worker error.
    pub async fn apply_portable_import(
        &self,
        plan: PortableImportPlan,
    ) -> Result<PortableImportResult, UiDataError> {
        let result = self.data.apply_portable_import(plan, now()).await?;
        self.wake_sync().await;
        Ok(result)
    }

    /// Authenticates, resolves the User's isolated Profile, and starts event-driven sync.
    ///
    /// # Errors
    /// Returns an incompatibility, login, storage, or lifecycle error. Password bytes are passed
    /// directly to the request and are never persisted.
    pub async fn login(
        &self,
        server_origin: String,
        username: String,
        password: String,
    ) -> Result<(), ApplicationError> {
        self.sync_transport.compatibility(&server_origin).await?;
        self.stop_sync().await;
        let installation = self.installation();
        let credentials = LoginCredentials {
            username,
            password,
            device_id: installation.device_id.to_string(),
            platform: installation.platform.as_str().to_owned(),
            device_name: Some(installation.device_name.clone()),
        };
        let (session, identity) = AuthenticatedSession::login(
            Arc::clone(&self.sync_transport),
            Arc::clone(&self.credentials),
            server_origin.clone(),
            &credentials,
        )
        .await?;
        let user_id = Uuid::parse_str(&identity.user_id).map_err(|_| {
            ApplicationError::ProviderSettings("Server returned an invalid User identity")
        })?;
        let metadata = if let Some(mut existing) =
            self.database.find_profile_for_user(user_id).await?
        {
            existing.username = Some(identity.username.clone());
            existing.server_origin = Some(server_origin);
            existing.updated_at = now();
            self.database.switch_profile(existing.clone()).await?;
            existing
        } else if self.profile().user_id.is_none() {
            self.database
                .resolve_account_profile(user_id, identity.username.clone(), server_origin, now())
                .await?
        } else {
            let timestamp = now();
            let installation = self.installation();
            let metadata = ProfileMetadata {
                profile_id: Uuid::now_v7(),
                user_id: Some(user_id),
                username: Some(identity.username.clone()),
                device_id: installation.device_id,
                platform: installation.platform.as_str().to_owned(),
                server_origin: Some(server_origin),
                // A new local Profile must catch up from the beginning. The login response's
                // latest revision is a target hint, never a safe persisted cursor.
                last_server_revision: 0,
                created_at: timestamp,
                updated_at: timestamp,
            };
            self.database.switch_profile(metadata.clone()).await?;
            metadata
        };
        self.activate_profile(metadata)?;
        self.start_sync(Arc::new(session)).await;
        Ok(())
    }

    /// Resumes a bound Profile by rotating its persisted Refresh Token.
    ///
    /// # Errors
    /// Returns an incompatibility, credential, session, transport, or lifecycle error.
    pub async fn resume_session(&self) -> Result<(), ApplicationError> {
        let profile = self.profile();
        let user_id = profile.user_id.ok_or(ApplicationError::ProviderSettings(
            "Profile is not bound to an account",
        ))?;
        let server_origin =
            profile
                .server_origin
                .clone()
                .ok_or(ApplicationError::ProviderSettings(
                    "Profile has no Server origin",
                ))?;
        self.sync_transport.compatibility(&server_origin).await?;
        let session = AuthenticatedSession::resume(
            Arc::clone(&self.sync_transport),
            Arc::clone(&self.credentials),
            server_origin,
            user_id.to_string(),
            profile.username.unwrap_or_default(),
            profile.device_id.to_string(),
            profile.platform,
        )
        .await?;
        self.start_sync(Arc::new(session)).await;
        Ok(())
    }

    /// Stops sync, revokes the current Refresh Session, and removes local session credentials.
    ///
    /// # Errors
    /// Returns a Server or Credential Store error after local sync has stopped.
    pub async fn logout(&self) -> Result<(), ApplicationError> {
        let session = self.sync.lock().await.session.clone();
        self.stop_sync().await;
        if let Some(session) = session {
            session.logout().await?;
        }
        Ok(())
    }

    /// Performs the bounded Server compatibility handshake without authenticating.
    ///
    /// # Errors
    /// Returns a URL, transport, response, or compatibility error.
    pub async fn test_connection(&self, origin: &str) -> Result<(), ApplicationError> {
        self.sync_transport.compatibility(origin).await?;
        Ok(())
    }

    pub async fn manual_sync(&self) -> bool {
        let handle = self.sync.lock().await.handle.clone();
        handle.is_some_and(|handle| {
            handle.wake();
            true
        })
    }

    pub async fn is_authenticated(&self) -> bool {
        self.sync.lock().await.session.is_some()
    }

    /// Lists the authenticated User's registered devices.
    ///
    /// # Errors
    /// Returns a login-required, session, transport, or response error.
    pub async fn devices(&self) -> Result<Vec<RemoteDevice>, ApplicationError> {
        let session = self
            .sync
            .lock()
            .await
            .session
            .clone()
            .ok_or(ApplicationError::ProviderSettings("Login required"))?;
        Ok(session.devices().await?)
    }

    /// Permanently revokes one device belonging to the authenticated User.
    ///
    /// # Errors
    /// Returns a login-required, invalid-identity, session, transport, or response error.
    pub async fn revoke_device(&self, device_id: &str) -> Result<(), ApplicationError> {
        let session = self
            .sync
            .lock()
            .await
            .session
            .clone()
            .ok_or(ApplicationError::ProviderSettings("Login required"))?;
        session.revoke_device(device_id).await?;
        Ok(())
    }

    /// Replaces a permanently revoked installation identity after the UI has confirmed it.
    ///
    /// # Errors
    /// Returns a storage, credential deletion, Profile migration, or lifecycle error.
    pub async fn recover_revoked_device(&self) -> Result<(), ApplicationError> {
        self.stop_sync().await;
        let installation = self.installation();
        let manager = crate::DeviceIdentityManager::new(
            InstallationStore::new(&self.root),
            Arc::clone(&self.database),
            Arc::clone(&self.credentials),
        );
        let replacement = manager
            .regenerate_after_revocation(
                true,
                installation.platform,
                &installation.device_name,
                now(),
            )
            .await?;
        *self
            .installation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = replacement.clone();
        let mut profile = self.profile();
        profile.device_id = replacement.device_id;
        profile.updated_at = now();
        self.activate_profile(profile)?;
        Ok(())
    }

    /// Reads non-secret sync diagnostics for the active Profile.
    ///
    /// # Errors
    /// Returns a database-worker or query error.
    pub async fn sync_diagnostics(
        &self,
    ) -> Result<lvos_storage::SyncDiagnostics, ApplicationError> {
        Ok(self
            .database
            .execute(|database| database.sync_diagnostics())
            .await?)
    }

    fn activate_profile(&self, profile: ProfileMetadata) -> Result<(), ApplicationError> {
        let preferences =
            read_provider_preferences(&provider_path(&self.root, profile.profile_id))?;
        *self
            .profile
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = profile;
        *self
            .provider_preferences
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = preferences;
        self.rebuild_lookup_service()
    }

    async fn start_sync(&self, session: Arc<AuthenticatedSession<HttpSyncTransport>>) {
        self.stop_sync().await;
        let engine = Arc::new(SyncEngine::new(
            Arc::clone(&self.database),
            Arc::clone(&session),
        ));
        let (worker, handle) = SyncWorker::new(engine);
        let cancellation = CancellationToken::new();
        let tasks = worker.start(&cancellation);
        *self.sync.lock().await = ActiveSync {
            session: Some(session),
            handle: Some(handle),
            cancellation: Some(cancellation),
            tasks,
        };
    }

    async fn stop_sync(&self) {
        let mut active = self.sync.lock().await;
        if let Some(cancellation) = active.cancellation.take() {
            cancellation.cancel();
        }
        let tasks = std::mem::take(&mut active.tasks);
        active.handle = None;
        active.session = None;
        drop(active);
        for task in tasks {
            let _ = task.await;
        }
    }

    async fn wake_sync(&self) {
        if let Some(handle) = self.sync.lock().await.handle.clone() {
            handle.wake();
        }
    }

    fn rebuild_lookup_service(&self) -> Result<(), ApplicationError> {
        let preferences = self.provider_preferences();
        let profile = self.profile();
        let user_scope = profile.profile_id.to_string();
        let device_scope = profile.device_id.to_string();
        let reader = CredentialReader::new(
            self.credentials.as_ref(),
            PROVIDER_SCOPE_ORIGIN,
            &user_scope,
            &device_scope,
        );
        let timeout = TimeoutConfig::default();
        let transport = Arc::new(
            ReqwestTransport::new(timeout)
                .map_err(|_| ApplicationError::ProviderSettings("HTTP transport unavailable"))?,
        );
        let mut registry = ProviderRegistry::default();
        if let Ok(key) = reader.tokenhub_api_key() {
            registry.register(Arc::new(
                TencentTokenHubProvider::new(transport.clone(), key, timeout)
                    .with_model(&preferences.tokenhub_model)?,
            ));
        }
        if let Ok(key) = reader.google_api_key() {
            registry.register(Arc::new(GoogleBasicV2Provider::new(
                transport, key, timeout,
            )));
        }
        let settings = RouterSettings {
            primary: ProviderId::new(preferences.primary),
            fallback: preferences.fallback.map(ProviderId::new),
        };
        let lookup = TranslationRouter::new(&registry, &settings).map_or_else(
            |_| {
                Arc::new(LookupService::new_without_provider(Arc::clone(
                    &self.database,
                )))
            },
            |router| Arc::new(LookupService::new(Arc::clone(&self.database), router)),
        );
        *self
            .lookup
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = lookup;
        Ok(())
    }

    fn provider_scope(&self, key: CredentialKey) -> CredentialScope {
        let profile = self.profile();
        CredentialScope {
            server_origin: PROVIDER_SCOPE_ORIGIN.to_owned(),
            user_id: profile.profile_id.to_string(),
            device_id: profile.device_id.to_string(),
            key,
        }
    }
}

fn new_unbound_profile(installation: &InstallationMetadata) -> ProfileMetadata {
    let timestamp = now();
    ProfileMetadata {
        profile_id: Uuid::new_v4(),
        user_id: None,
        username: None,
        device_id: installation.device_id,
        platform: installation.platform.as_str().to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: timestamp,
        updated_at: timestamp,
    }
}

fn language(value: &str) -> LanguageCode {
    LanguageCode::parse(value).unwrap_or_else(|_| unreachable!("frozen language is valid"))
}

fn now() -> UnixTimestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    UnixTimestamp::from_seconds(seconds)
}

fn provider_path(root: &Path, profile_id: Uuid) -> PathBuf {
    root.join(format!("provider-settings-{profile_id}.json"))
}

fn read_provider_preferences(path: &Path) -> Result<ProviderPreferences, ApplicationError> {
    if !path.exists() {
        return Ok(ProviderPreferences::default());
    }
    let mut preferences: ProviderPreferences = serde_json::from_slice(&fs::read(path)?)?;
    preferences.tokenhub_model = validated_tokenhub_model(&preferences.tokenhub_model)?;
    validate_provider_id(&preferences.primary)?;
    if let Some(fallback) = &preferences.fallback {
        validate_provider_id(fallback)?;
        if fallback == &preferences.primary {
            return Err(ApplicationError::ProviderSettings(
                "Primary and Fallback Providers must be different",
            ));
        }
    }
    Ok(preferences)
}

fn default_tokenhub_model() -> String {
    lvos_translation::DEFAULT_TOKENHUB_MODEL.to_owned()
}

fn validated_tokenhub_model(model: &str) -> Result<String, ApplicationError> {
    validate_tokenhub_model(model)
        .map(str::to_owned)
        .map_err(|_| {
            ApplicationError::ProviderSettings(
                "Tencent TokenHub model must be 1-128 characters without whitespace or control characters",
            )
        })
}

fn write_provider_preferences(
    path: &Path,
    preferences: &ProviderPreferences,
) -> Result<(), ApplicationError> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(preferences)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn validate_provider_id(value: &str) -> Result<(), ApplicationError> {
    if matches!(
        value,
        lvos_translation::DEFAULT_PRIMARY_PROVIDER | lvos_translation::DEFAULT_FALLBACK_PROVIDER
    ) {
        Ok(())
    } else {
        Err(ApplicationError::ProviderSettings(
            "unknown translation Provider",
        ))
    }
}

fn ensure_selected_configured(
    preferences: &ProviderPreferences,
    tokenhub: bool,
    google: bool,
) -> Result<(), ApplicationError> {
    let configured = |provider: &str| match provider {
        lvos_translation::DEFAULT_PRIMARY_PROVIDER => tokenhub,
        lvos_translation::DEFAULT_FALLBACK_PROVIDER => google,
        _ => false,
    };
    if !configured(&preferences.primary)
        || preferences
            .fallback
            .as_deref()
            .is_some_and(|provider| !configured(provider))
    {
        return Err(ApplicationError::ProviderSettings(
            "selected Provider is not configured",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum ApplicationError {
    Io(std::io::Error),
    Storage(lvos_storage::StorageError),
    Database(crate::DatabaseWorkerError),
    Credential(lvos_auth::AuthError),
    Transport(TransportError),
    Session(SessionError),
    ProviderCredential(lvos_translation::ProviderCredentialError),
    Translation(lvos_translation::TranslationError),
    DeviceIdentity(crate::DeviceIdentityError),
    Json(serde_json::Error),
    ProviderSettings(&'static str),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Desktop filesystem failed: {error}"),
            Self::Storage(error) => write!(formatter, "installation storage failed: {error}"),
            Self::Database(error) => write!(formatter, "Profile database failed: {error}"),
            Self::Credential(error) => write!(formatter, "Credential Store failed: {error}"),
            Self::Transport(error) => write!(formatter, "Server compatibility failed: {error}"),
            Self::Session(error) => write!(formatter, "Server session failed: {error}"),
            Self::ProviderCredential(error) => {
                write!(formatter, "Provider credential failed: {error}")
            }
            Self::Translation(error) => write!(formatter, "Provider test failed: {error}"),
            Self::DeviceIdentity(error) => write!(formatter, "Device recovery failed: {error}"),
            Self::Json(error) => write!(formatter, "Provider settings are invalid: {error}"),
            Self::ProviderSettings(message) => formatter.write_str(message),
        }
    }
}

impl Error for ApplicationError {}

impl From<std::io::Error> for ApplicationError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<lvos_storage::StorageError> for ApplicationError {
    fn from(value: lvos_storage::StorageError) -> Self {
        Self::Storage(value)
    }
}

impl From<crate::DatabaseWorkerError> for ApplicationError {
    fn from(value: crate::DatabaseWorkerError) -> Self {
        Self::Database(value)
    }
}

impl From<lvos_auth::AuthError> for ApplicationError {
    fn from(value: lvos_auth::AuthError) -> Self {
        Self::Credential(value)
    }
}

impl From<TransportError> for ApplicationError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<SessionError> for ApplicationError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

impl From<lvos_translation::ProviderCredentialError> for ApplicationError {
    fn from(value: lvos_translation::ProviderCredentialError) -> Self {
        Self::ProviderCredential(value)
    }
}

impl From<lvos_translation::TranslationError> for ApplicationError {
    fn from(value: lvos_translation::TranslationError) -> Self {
        Self::Translation(value)
    }
}

impl From<crate::DeviceIdentityError> for ApplicationError {
    fn from(value: crate::DeviceIdentityError) -> Self {
        Self::DeviceIdentity(value)
    }
}

impl From<serde_json::Error> for ApplicationError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[must_use]
pub const fn default_server_url() -> &'static str {
    DEFAULT_SERVER_URL
}
