use std::{
    any::Any,
    error::Error,
    fmt,
    path::PathBuf,
    sync::mpsc,
    thread::{self, JoinHandle, ThreadId},
};

use lvos_storage::{ProfileDatabase, ProfileMetadata, ProfilePaths, StorageError};
use tokio::sync::oneshot;
use uuid::Uuid;

type BoxedValue = Box<dyn Any + Send>;
type DatabaseJob = Box<dyn FnOnce(&mut WorkerState) + Send>;

enum WorkerCommand {
    Job(DatabaseJob),
    Shutdown,
}

struct WorkerState {
    application_data_root: PathBuf,
    active: Option<ProfileDatabase>,
    thread_id: ThreadId,
}

impl fmt::Debug for WorkerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerState")
            .field("application_data_root", &self.application_data_root)
            .field("active", &self.active.as_ref().map(ProfileDatabase::paths))
            .field("thread_id", &self.thread_id)
            .finish()
    }
}

#[derive(Debug)]
pub struct DatabaseWorker {
    sender: mpsc::Sender<WorkerCommand>,
    join: Option<JoinHandle<()>>,
    thread_id: ThreadId,
}

impl DatabaseWorker {
    /// Starts one dedicated blocking thread that exclusively owns the active `SQLite` Profile.
    ///
    /// # Errors
    /// Returns an error when the operating system cannot start the worker thread.
    pub fn start(application_data_root: PathBuf) -> Result<Self, DatabaseWorkerError> {
        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("lvos-database".to_owned())
            .spawn(move || {
                let thread_id = thread::current().id();
                let _ = ready_sender.send(thread_id);
                let mut state = WorkerState {
                    application_data_root,
                    active: None,
                    thread_id,
                };
                while let Ok(command) = receiver.recv() {
                    match command {
                        WorkerCommand::Job(job) => job(&mut state),
                        WorkerCommand::Shutdown => break,
                    }
                }
                state.active = None;
            })
            .map_err(DatabaseWorkerError::Start)?;
        let thread_id = ready_receiver
            .recv()
            .map_err(|_| DatabaseWorkerError::Unavailable)?;
        Ok(Self {
            sender,
            join: Some(join),
            thread_id,
        })
    }

    #[must_use]
    pub const fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Executes a typed closure on the dedicated database thread.
    ///
    /// # Errors
    /// Returns an error when the worker stopped, the operation failed, or its result type is invalid.
    pub async fn execute<T, F>(&self, operation: F) -> Result<T, DatabaseWorkerError>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProfileDatabase) -> Result<T, StorageError> + Send + 'static,
    {
        self.schedule(move |state| {
            let database = state
                .active
                .as_mut()
                .ok_or(DatabaseWorkerError::NoActiveProfile)?;
            operation(database).map_err(DatabaseWorkerError::Storage)
        })
        .await
    }

    /// Flushes by queue ordering, closes the active Profile, then opens the target Profile.
    ///
    /// # Errors
    /// Returns an error without replacing the active Profile when opening the target fails.
    pub async fn switch_profile(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<PathBuf, DatabaseWorkerError> {
        self.schedule(move |state| {
            let paths = ProfilePaths::new(&state.application_data_root, metadata.profile_id);
            let replacement =
                ProfileDatabase::open(paths, &metadata).map_err(DatabaseWorkerError::Storage)?;
            let path = replacement.paths().database().to_path_buf();
            state.active = Some(replacement);
            Ok(path)
        })
        .await
    }

    /// Returns the active Profile identity.
    ///
    /// # Errors
    /// Returns an error when no Profile is active or the worker is unavailable.
    pub async fn active_profile_id(&self) -> Result<Uuid, DatabaseWorkerError> {
        self.schedule(|state| {
            let database = state
                .active
                .as_ref()
                .ok_or(DatabaseWorkerError::NoActiveProfile)?;
            database
                .metadata()
                .map(|metadata| metadata.profile_id)
                .map_err(DatabaseWorkerError::Storage)
        })
        .await
    }

    /// Resolves an existing local Profile by stable Server User identity.
    ///
    /// # Errors
    /// Returns an error when the Profile directory cannot be read or a candidate Profile is corrupt.
    pub async fn find_profile_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Option<ProfileMetadata>, DatabaseWorkerError> {
        self.schedule(move |state| {
            let entries = std::fs::read_dir(&state.application_data_root)
                .map_err(|error| DatabaseWorkerError::Storage(StorageError::Io(error)))?;
            for entry in entries {
                let path = entry
                    .map_err(|error| DatabaseWorkerError::Storage(StorageError::Io(error)))?
                    .path();
                let is_profile = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("profile-") && name.ends_with(".sqlite3"));
                if !is_profile {
                    continue;
                }
                let metadata = ProfileDatabase::inspect_metadata(&path)
                    .map_err(DatabaseWorkerError::Storage)?;
                if metadata.user_id == Some(user_id) {
                    return Ok(Some(metadata));
                }
            }
            Ok(None)
        })
        .await
    }

    /// Binds the current unbound Profile or switches to an existing account Profile.
    ///
    /// # Errors
    /// Returns an error for an invalid lifecycle state, database failure, or worker shutdown.
    pub async fn resolve_account_profile(
        &self,
        user_id: Uuid,
        username: String,
        server_origin: String,
        now: lvos_core::UnixTimestamp,
    ) -> Result<ProfileMetadata, DatabaseWorkerError> {
        if let Some(mut existing) = self.find_profile_for_user(user_id).await? {
            existing.username = Some(username);
            existing.server_origin = Some(server_origin);
            existing.updated_at = now;
            self.switch_profile(existing.clone()).await?;
            self.execute(move |database| {
                database.update_account_identity(
                    user_id,
                    existing.username.as_deref().unwrap_or_default(),
                    existing.server_origin.as_deref().unwrap_or_default(),
                    now,
                )?;
                database.metadata()
            })
            .await
        } else {
            self.execute(move |database| {
                if !database.is_unbound()? {
                    return Err(StorageError::InvalidData(
                        "bound Profile cannot be rebound to another User",
                    ));
                }
                database.bind_user(user_id, &username, &server_origin, now)?;
                database.metadata()
            })
            .await
        }
    }

    async fn schedule<T, F>(&self, operation: F) -> Result<T, DatabaseWorkerError>
    where
        T: Send + 'static,
        F: FnOnce(&mut WorkerState) -> Result<T, DatabaseWorkerError> + Send + 'static,
    {
        let (result_sender, result_receiver) = oneshot::channel();
        let job = Box::new(move |state: &mut WorkerState| {
            let result = operation(state).map(|value| Box::new(value) as BoxedValue);
            let _ = result_sender.send(result);
        });
        self.sender
            .send(WorkerCommand::Job(job))
            .map_err(|_| DatabaseWorkerError::Unavailable)?;
        let value = result_receiver
            .await
            .map_err(|_| DatabaseWorkerError::Unavailable)??;
        value
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| DatabaseWorkerError::InvalidResultType)
    }
}

impl Drop for DatabaseWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Debug)]
pub enum DatabaseWorkerError {
    Start(std::io::Error),
    Unavailable,
    NoActiveProfile,
    InvalidResultType,
    Storage(StorageError),
}

impl fmt::Display for DatabaseWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(error) => write!(formatter, "failed to start database worker: {error}"),
            Self::Unavailable => formatter.write_str("the database worker is unavailable"),
            Self::NoActiveProfile => formatter.write_str("no Profile is active"),
            Self::InvalidResultType => {
                formatter.write_str("database worker returned an invalid type")
            }
            Self::Storage(error) => write!(formatter, "Profile storage failed: {error}"),
        }
    }
}

impl Error for DatabaseWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Start(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Unavailable | Self::NoActiveProfile | Self::InvalidResultType => None,
        }
    }
}
