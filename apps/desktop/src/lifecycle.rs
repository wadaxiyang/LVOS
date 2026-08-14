use std::{error::Error, fmt, future::Future, sync::Arc, time::Duration};

use tokio::{runtime::Runtime, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{DatabaseWorker, DatabaseWorkerError, UiDispatcher};
use lvos_platform::{InstanceAcquisition, PlatformError, SingleInstanceService};
use lvos_storage::ProfileMetadata;

pub trait BackgroundProfileServices: Send + Sync {
    fn start(&self, profile_id: Uuid, cancellation: CancellationToken) -> Vec<JoinHandle<()>>;
}

#[derive(Debug)]
struct ActiveServices {
    cancellation: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl ActiveServices {
    async fn stop(self) {
        self.cancellation.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

pub struct ProfileLifecycle<S> {
    database: Arc<DatabaseWorker>,
    services: Arc<S>,
    active_services: Option<ActiveServices>,
}

impl<S> fmt::Debug for ProfileLifecycle<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileLifecycle")
            .field("database", &self.database)
            .field("services_active", &self.active_services.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: BackgroundProfileServices + 'static> ProfileLifecycle<S> {
    #[must_use]
    pub fn new(database: impl Into<Arc<DatabaseWorker>>, services: Arc<S>) -> Self {
        Self {
            database: database.into(),
            services,
            active_services: None,
        }
    }

    /// Stops old Profile services, flushes queued DB work, switches Profile, then starts new services.
    ///
    /// # Errors
    /// Returns an error if the old services cannot finish within the deadline or Profile open fails.
    pub async fn switch_profile(
        &mut self,
        metadata: ProfileMetadata,
        stop_timeout: Duration,
    ) -> Result<SwitchOutcome, RuntimeError> {
        if let Some(active) = self.active_services.take() {
            tokio::time::timeout(stop_timeout, active.stop())
                .await
                .map_err(|_| RuntimeError::ServiceStopTimeout)?;
        }
        let profile_id = metadata.profile_id;
        let database_path = self.database.switch_profile(metadata).await?;
        let cancellation = CancellationToken::new();
        let tasks = self.services.start(profile_id, cancellation.clone());
        self.active_services = Some(ActiveServices {
            cancellation,
            tasks,
        });
        Ok(SwitchOutcome {
            profile_id,
            database_path,
        })
    }

    /// Stops current Profile background services and closes the database worker when dropped.
    ///
    /// # Errors
    /// Returns an error if background services cannot finish before the deadline.
    pub async fn shutdown(&mut self, stop_timeout: Duration) -> Result<(), RuntimeError> {
        if let Some(active) = self.active_services.take() {
            tokio::time::timeout(stop_timeout, active.stop())
                .await
                .map_err(|_| RuntimeError::ServiceStopTimeout)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn database(&self) -> &DatabaseWorker {
        self.database.as_ref()
    }

    #[must_use]
    pub fn database_handle(&self) -> Arc<DatabaseWorker> {
        Arc::clone(&self.database)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwitchOutcome {
    pub profile_id: Uuid,
    pub database_path: std::path::PathBuf,
}

pub enum StartupDisposition {
    Primary(Box<dyn lvos_platform::SingleInstanceGuard>),
    SignaledExisting,
}

impl fmt::Debug for StartupDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Primary(_) => "StartupDisposition::Primary(..)",
            Self::SignaledExisting => "StartupDisposition::SignaledExisting",
        })
    }
}

/// Acquires the installation-wide instance lock before any runtime service starts.
///
/// # Errors
/// Returns an error when lock acquisition or signaling the existing process fails.
pub fn acquire_single_instance(
    service: &dyn SingleInstanceService,
) -> Result<StartupDisposition, RuntimeError> {
    match service.acquire().map_err(RuntimeError::Platform)? {
        InstanceAcquisition::Primary(guard) => Ok(StartupDisposition::Primary(guard)),
        InstanceAcquisition::Existing(guard) => {
            guard.signal_existing().map_err(RuntimeError::Platform)?;
            Ok(StartupDisposition::SignaledExisting)
        }
    }
}

#[derive(Debug)]
pub struct DesktopRuntime<U> {
    runtime: Runtime,
    ui: U,
    shutdown: CancellationToken,
}

impl<U: UiDispatcher> DesktopRuntime<U> {
    /// Starts the Desktop async runtime. The caller remains the Slint event-loop thread.
    ///
    /// # Errors
    /// Returns an error when the multithread Tokio runtime cannot be created.
    pub fn try_new(ui: U) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("lvos-async")
            .build()
            .map_err(RuntimeError::RuntimeStart)?;
        Ok(Self {
            runtime,
            ui,
            shutdown: CancellationToken::new(),
        })
    }

    #[must_use]
    pub fn new(ui: U) -> Self {
        Self::try_new(ui).unwrap_or_else(|error| {
            tracing::error!(%error, "fatal Desktop runtime startup failure");
            std::process::abort();
        })
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.shutdown.child_token()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Sends a completed background result to the UI event-loop thread.
    ///
    /// # Errors
    /// Returns an error if the UI event loop is unavailable.
    pub fn dispatch_ui(
        &self,
        callback: impl FnOnce() + Send + 'static,
    ) -> Result<(), RuntimeError> {
        self.ui.dispatch(callback).map_err(RuntimeError::Ui)
    }

    pub fn shutdown(self) {
        self.shutdown.cancel();
        self.runtime.shutdown_timeout(Duration::from_secs(5));
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    RuntimeStart(std::io::Error),
    Database(DatabaseWorkerError),
    Ui(crate::UiDispatchError),
    Platform(PlatformError),
    ServiceStopTimeout,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeStart(error) => {
                write!(formatter, "failed to start async runtime: {error}")
            }
            Self::Database(error) => write!(formatter, "database runtime failed: {error}"),
            Self::Ui(error) => write!(formatter, "UI dispatch failed: {error}"),
            Self::Platform(error) => write!(formatter, "platform startup failed: {error}"),
            Self::ServiceStopTimeout => {
                formatter.write_str("Profile services did not stop before timeout")
            }
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RuntimeStart(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Ui(error) => Some(error),
            Self::Platform(error) => Some(error),
            Self::ServiceStopTimeout => None,
        }
    }
}

impl From<DatabaseWorkerError> for RuntimeError {
    fn from(error: DatabaseWorkerError) -> Self {
        Self::Database(error)
    }
}
