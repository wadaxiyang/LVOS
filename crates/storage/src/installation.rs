use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::StorageError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Windows,
    Macos,
}

impl Platform {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstallationMetadata {
    pub device_id: Uuid,
    pub platform: Platform,
    pub device_name: String,
}

impl InstallationMetadata {
    #[must_use]
    pub fn new(platform: Platform, device_name: String) -> Self {
        Self {
            device_id: Uuid::new_v4(),
            platform,
            device_name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstallationStore {
    path: PathBuf,
}

impl InstallationStore {
    #[must_use]
    pub fn new(application_data_root: &Path) -> Self {
        Self {
            path: application_data_root.join("installation.json"),
        }
    }

    /// Loads installation identity, creating it atomically on first use.
    ///
    /// # Errors
    /// Returns an error for invalid JSON, an empty device name, or filesystem failure.
    pub fn load_or_create(
        &self,
        platform: Platform,
        device_name: &str,
    ) -> Result<InstallationMetadata, StorageError> {
        if self.path.exists() {
            let metadata: InstallationMetadata = serde_json::from_slice(&fs::read(&self.path)?)?;
            validate_metadata(&metadata)?;
            return Ok(metadata);
        }
        if device_name.trim().is_empty() {
            return Err(StorageError::InvalidData("device name is empty"));
        }
        let metadata = InstallationMetadata::new(platform, device_name.trim().to_owned());
        self.persist(&metadata)?;
        Ok(metadata)
    }

    /// Permanently replaces a revoked installation identity after explicit user confirmation.
    ///
    /// The caller must first preserve Profile data and Outbox intent under the replacement ID.
    ///
    /// # Errors
    /// Returns an error if the current identity changed, the replacement is invalid, or atomic
    /// persistence fails.
    pub fn replace_revoked_identity(
        &self,
        expected_current: Uuid,
        replacement: Uuid,
    ) -> Result<InstallationMetadata, StorageError> {
        if replacement.is_nil() || replacement == expected_current {
            return Err(StorageError::InvalidData(
                "replacement device identity is invalid",
            ));
        }
        let mut metadata: InstallationMetadata = serde_json::from_slice(&fs::read(&self.path)?)?;
        validate_metadata(&metadata)?;
        if metadata.device_id != expected_current {
            return Err(StorageError::InvalidData(
                "installation device identity changed",
            ));
        }
        metadata.device_id = replacement;
        self.persist(&metadata)?;
        Ok(metadata)
    }

    fn persist(&self, metadata: &InstallationMetadata) -> Result<(), StorageError> {
        let parent = self
            .path
            .parent()
            .ok_or(StorageError::InvalidData("installation path has no parent"))?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&serde_json::to_vec_pretty(metadata)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.path)
            .map_err(|error| StorageError::Io(error.error))?;
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn validate_metadata(metadata: &InstallationMetadata) -> Result<(), StorageError> {
    if metadata.device_id.is_nil() {
        return Err(StorageError::InvalidData("device id is nil"));
    }
    if metadata.device_name.trim().is_empty() {
        return Err(StorageError::InvalidData("device name is empty"));
    }
    Ok(())
}
