//! Storage boundaries shared by Desktop and Server implementations.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MigrationVersion(u32);

impl MigrationVersion {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    pub version: MigrationVersion,
    pub name: &'static str,
}

pub trait MigrationRunner: Send + Sync {
    /// Returns the most recently applied migration version.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when schema metadata cannot be read.
    fn current_version(&self) -> Result<Option<MigrationVersion>, StorageError>;

    /// Creates a coordinated, consistent backup before migration begins.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Backup`] when a recoverable backup cannot be completed.
    fn create_consistent_backup(&self) -> Result<(), StorageError>;

    /// Applies pending migrations strictly in version order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when migration validation or application fails.
    fn apply_pending(&self, migrations: &[Migration]) -> Result<MigrationVersion, StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageError {
    Open,
    Backup,
    Migration,
    Invariant(&'static str),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => formatter.write_str("failed to open storage"),
            Self::Backup => formatter.write_str("failed to create a consistent backup"),
            Self::Migration => formatter.write_str("failed to apply storage migration"),
            Self::Invariant(message) => write!(formatter, "storage invariant failed: {message}"),
        }
    }
}

impl Error for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_versions_are_strictly_orderable() {
        assert!(MigrationVersion::new(2) > MigrationVersion::new(1));
    }
}
