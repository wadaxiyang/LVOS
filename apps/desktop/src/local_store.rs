//! Small, shared persistence primitives for non-secret Desktop preferences.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use uuid::Uuid;

/// A directory-backed store for small, non-secret Desktop preferences.
#[derive(Clone, Debug)]
pub struct LocalPreferenceStore {
    root: PathBuf,
}

impl LocalPreferenceStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn load_text(&self, name: &str) -> Option<String> {
        self.path(name)
            .and_then(|path| fs::read_to_string(path).ok())
    }

    /// Atomically persists a small text preference.
    ///
    /// # Errors
    /// Returns an I/O error when the preference directory or file cannot be written.
    pub fn save_text(&self, name: &str, value: &str) -> io::Result<()> {
        let path = self.path(name).ok_or_else(invalid_preference_name)?;
        atomic_write(&path, value.as_bytes())
    }

    #[must_use]
    pub fn load_boolean(&self, name: &str) -> bool {
        self.load_text(name)
            .is_some_and(|value| value.trim() == "true")
    }

    /// Atomically persists a boolean preference.
    ///
    /// # Errors
    /// Returns an I/O error when the preference directory or file cannot be written.
    pub fn save_boolean(&self, name: &str, value: bool) -> io::Result<()> {
        self.save_text(name, if value { "true" } else { "false" })
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        (!name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        .then(|| self.root.join(format!("{name}.txt")))
    }
}

fn invalid_preference_name() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid preference name")
}

/// Writes bytes through a same-directory temporary file and replaces the destination.
///
/// The temporary file is flushed before replacement. Same-directory replacement keeps the
/// operation on one filesystem and makes successful writes atomic on supported platforms.
///
/// # Errors
/// Returns an I/O error when the parent directory, temporary file, flush, or replacement fails.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid preference path"))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        lvos_platform::replace_file(&temporary, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{LocalPreferenceStore, atomic_write};

    #[test]
    fn atomic_write_replaces_existing_content_without_leaving_temporary_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("settings.json");

        atomic_write(&path, b"first")?;
        atomic_write(&path, b"second")?;

        assert_eq!(std::fs::read(&path)?, b"second");
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn preference_store_round_trips_text_and_boolean_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let store = LocalPreferenceStore::new(root.path());

        store.save_text("shortcut", "Alt+D")?;
        store.save_boolean("launch-minimized", true)?;

        assert_eq!(store.load_text("shortcut").as_deref(), Some("Alt+D"));
        assert!(store.load_boolean("launch-minimized"));
        assert_eq!(store.root(), root.path());
        assert!(store.load_text("../outside").is_none());
        assert!(store.save_text("../outside", "blocked").is_err());
        Ok(())
    }
}
