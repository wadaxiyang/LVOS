use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{RepositoryError, ServerRepository};

/// Coordinates manual and periodic consistent Server backups.
#[derive(Clone, Debug)]
pub struct BackupService {
    repository: ServerRepository,
    backup_dir: PathBuf,
    retention_count: usize,
    interval: Duration,
}

impl BackupService {
    #[must_use]
    pub fn new(
        repository: ServerRepository,
        backup_dir: PathBuf,
        retention_count: usize,
        interval: Duration,
    ) -> Self {
        Self {
            repository,
            backup_dir,
            retention_count,
            interval,
        }
    }

    /// Runs one consistent backup immediately.
    ///
    /// # Errors
    /// Returns an error when the clock, `SQLite` backup, verification, or retention fails.
    pub fn run(&self, reason: &str) -> Result<PathBuf, RepositoryError> {
        let now = unix_timestamp()?;
        self.repository
            .backup(&self.backup_dir, self.retention_count, reason, now)
    }

    /// Starts the configured low-frequency backup loop on a blocking worker.
    #[must_use]
    pub fn start_periodic(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.interval);
            interval.tick().await;
            loop {
                interval.tick().await;
                let service = self.clone();
                match tokio::task::spawn_blocking(move || service.run("periodic")).await {
                    Ok(Ok(path)) => tracing::info!(
                        backup_file = %display_filename(&path),
                        "periodic database backup completed"
                    ),
                    Ok(Err(error)) => tracing::error!(%error, "periodic database backup failed"),
                    Err(_) => tracing::error!("periodic database backup worker failed"),
                }
            }
        })
    }
}

fn unix_timestamp() -> Result<i64, RepositoryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RepositoryError::Backup)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| RepositoryError::Backup)
}

fn display_filename(path: &Path) -> String {
    path.file_name().map_or_else(
        || "[unknown]".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}
