use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use lvos_core::SOFTWARE_VERSION;
use rusqlite::{Connection, OptionalExtension, backup::Backup};
use uuid::Uuid;

use crate::RepositoryError;

pub(crate) const SERVER_SCHEMA_V1: &str = r"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS users (
    user_id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    sync_revision INTEGER NOT NULL DEFAULT 0 CHECK(sync_revision >= 0),
    created_at INTEGER NOT NULL,
    disabled_at INTEGER NULL
);
CREATE TABLE IF NOT EXISTS devices (
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    platform TEXT NOT NULL CHECK(platform IN ('windows', 'macos')),
    device_name TEXT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    PRIMARY KEY (user_id, device_id),
    FOREIGN KEY (user_id) REFERENCES users(user_id)
);
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    access_token_hash TEXT NOT NULL UNIQUE,
    refresh_token_hash TEXT NOT NULL UNIQUE,
    access_expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_refreshed_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    FOREIGN KEY (user_id, device_id) REFERENCES devices(user_id, device_id)
);
CREATE INDEX IF NOT EXISTS idx_sessions_user_device
    ON sessions(user_id, device_id);
CREATE INDEX IF NOT EXISTS idx_sessions_refresh_hash
    ON sessions(refresh_token_hash);
";

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "server_identity_auth",
    sql: SERVER_SCHEMA_V1,
}];

pub(crate) fn sqlite_path(database_url: &str) -> Result<&Path, RepositoryError> {
    let value = database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url);
    if value.is_empty() {
        return Err(RepositoryError::Database);
    }
    Ok(Path::new(value))
}

pub(crate) fn initialize(
    connection: &mut Connection,
    backup_dir: Option<&Path>,
    retention_count: usize,
    now: i64,
) -> Result<(), RepositoryError> {
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
        .map_err(|_| RepositoryError::Database)?;

    let current = current_schema_version(connection)?;
    let pending = MIGRATIONS
        .iter()
        .copied()
        .filter(|migration| migration.version > current)
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }

    if current > 0 || database_has_application_tables(connection)? {
        let directory = backup_dir.ok_or(RepositoryError::Backup)?;
        create_backup(
            connection,
            directory,
            retention_count,
            current,
            "pre-migration",
            now,
        )?;
    }
    apply_migrations(connection, &pending, now)
}

fn current_schema_version(connection: &Connection) -> Result<i64, RepositoryError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|_| RepositoryError::Migration)?
        .is_some();
    if !exists {
        return Ok(0);
    }
    let mut statement = connection
        .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
        .map_err(|_| RepositoryError::Migration)?;
    let mut rows = statement
        .query([])
        .map_err(|_| RepositoryError::Migration)?;
    let mut current = 0;
    let mut index = 0;
    while let Some(row) = rows.next().map_err(|_| RepositoryError::Migration)? {
        let version: i64 = row.get(0).map_err(|_| RepositoryError::Migration)?;
        let name: String = row.get(1).map_err(|_| RepositoryError::Migration)?;
        let expected = MIGRATIONS.get(index).ok_or(RepositoryError::Migration)?;
        if version != expected.version || name != expected.name {
            return Err(RepositoryError::Migration);
        }
        current = version;
        index += 1;
    }
    Ok(current)
}

fn database_has_application_tables(connection: &Connection) -> Result<bool, RepositoryError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| RepositoryError::Migration)
}

fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
    now: i64,
) -> Result<(), RepositoryError> {
    let transaction = connection
        .transaction()
        .map_err(|_| RepositoryError::Migration)?;
    for migration in migrations {
        transaction
            .execute_batch(migration.sql)
            .map_err(|_| RepositoryError::Migration)?;
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, name, applied_at) VALUES(?1, ?2, ?3)",
                (migration.version, migration.name, now),
            )
            .map_err(|_| RepositoryError::Migration)?;
    }
    transaction.commit().map_err(|_| RepositoryError::Migration)
}

pub(crate) fn create_backup(
    source: &Connection,
    backup_dir: &Path,
    retention_count: usize,
    schema_version: i64,
    reason: &str,
    now: i64,
) -> Result<PathBuf, RepositoryError> {
    if retention_count == 0
        || reason.is_empty()
        || !reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(RepositoryError::Backup);
    }
    fs::create_dir_all(backup_dir).map_err(|_| RepositoryError::Backup)?;
    let filename = format!(
        "lvos-server-v{SOFTWARE_VERSION}-schema{schema_version}-{reason}-{now}-{}.sqlite3",
        Uuid::new_v4()
    );
    let path = backup_dir.join(filename);
    let temporary_path = path.with_extension("sqlite3.partial");
    let mut destination = Connection::open(&temporary_path).map_err(|_| RepositoryError::Backup)?;
    if let Err(error) =
        copy_database(source, &mut destination).and_then(|()| verify_integrity(&destination))
    {
        drop(destination);
        let _ = fs::remove_file(temporary_path);
        return Err(error);
    }
    drop(destination);
    fs::rename(temporary_path, &path).map_err(|_| RepositoryError::Backup)?;
    enforce_retention(backup_dir, retention_count)?;
    Ok(path)
}

fn copy_database(source: &Connection, destination: &mut Connection) -> Result<(), RepositoryError> {
    let backup = Backup::new(source, destination).map_err(|_| RepositoryError::Backup)?;
    backup
        .run_to_completion(128, Duration::from_millis(5), None)
        .map_err(|_| RepositoryError::Backup)
}

pub(crate) fn verify_integrity(connection: &Connection) -> Result<(), RepositoryError> {
    let result: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| RepositoryError::Integrity)?;
    if result == "ok" {
        Ok(())
    } else {
        Err(RepositoryError::Integrity)
    }
}

fn enforce_retention(backup_dir: &Path, retention_count: usize) -> Result<(), RepositoryError> {
    let mut backups = fs::read_dir(backup_dir)
        .map_err(|_| RepositoryError::Backup)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("lvos-server-v")
                && entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "sqlite3")
        })
        .collect::<Vec<_>>();
    backups.sort_by_key(|entry| {
        let filename = entry.file_name().to_string_lossy().into_owned();
        (backup_timestamp(&filename).unwrap_or(i64::MIN), filename)
    });
    let remove_count = backups.len().saturating_sub(retention_count);
    for entry in backups.into_iter().take(remove_count) {
        fs::remove_file(entry.path()).map_err(|_| RepositoryError::Backup)?;
    }
    Ok(())
}

fn backup_timestamp(filename: &str) -> Option<i64> {
    let stem = filename.strip_suffix(".sqlite3")?;
    let prefix = stem.get(..stem.len().checked_sub(37)?)?;
    prefix.rsplit_once('-')?.1.parse().ok()
}

pub(crate) fn restore_database(
    database_url: &str,
    source_path: &Path,
    backup_dir: &Path,
    retention_count: usize,
    now: i64,
) -> Result<PathBuf, RepositoryError> {
    let source = Connection::open(source_path).map_err(|_| RepositoryError::Restore)?;
    verify_integrity(&source).map_err(|_| RepositoryError::Restore)?;
    let database_path = sqlite_path(database_url)?;
    if database_path == Path::new(":memory:") {
        return Err(RepositoryError::Restore);
    }
    if let Some(parent) = database_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|_| RepositoryError::Restore)?;
    }
    let mut destination = Connection::open(database_path).map_err(|_| RepositoryError::Restore)?;
    destination
        .execute_batch("PRAGMA journal_mode = WAL;")
        .map_err(|_| RepositoryError::Restore)?;
    let recovery = create_backup(
        &destination,
        backup_dir,
        retention_count,
        current_schema_version(&destination).map_err(|_| RepositoryError::Restore)?,
        "pre-restore",
        now,
    )
    .map_err(|_| RepositoryError::Restore)?;
    copy_database(&source, &mut destination).map_err(|_| RepositoryError::Restore)?;
    verify_integrity(&destination).map_err(|_| RepositoryError::Restore)?;
    Ok(recovery)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lvos-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn failed_migration_rolls_back_schema_and_version() {
        let mut connection = Connection::open_in_memory().unwrap_or_else(|_| unreachable!());
        let migrations = [
            Migration {
                version: 1,
                name: "base",
                sql: "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL); CREATE TABLE stable(value TEXT);",
            },
            Migration {
                version: 2,
                name: "broken",
                sql: "CREATE TABLE partial(value TEXT); THIS IS NOT SQL;",
            },
        ];
        assert_eq!(
            apply_migrations(&mut connection, &migrations, 1),
            Err(RepositoryError::Migration)
        );
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('stable', 'partial', 'schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(tables, 0);
    }

    #[test]
    fn unknown_or_renamed_migration_refuses_startup() {
        let mut connection = Connection::open_in_memory().unwrap_or_else(|_| unreachable!());
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL); INSERT INTO schema_migrations VALUES(1, 'rewritten_identity', 1);",
            )
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            initialize(&mut connection, None, 1, 2),
            Err(RepositoryError::Migration)
        );
    }

    #[test]
    fn opening_a_legacy_database_backs_it_up_before_migration() {
        let root = temporary_directory("pre-migration-test");
        let backup_dir = root.join("backups");
        fs::create_dir_all(&root).unwrap_or_else(|_| unreachable!());
        let database_path = root.join("legacy.sqlite3");
        let mut connection = Connection::open(&database_path).unwrap_or_else(|_| unreachable!());
        connection
            .execute_batch("CREATE TABLE legacy_marker(value TEXT); INSERT INTO legacy_marker VALUES('preserved');")
            .unwrap_or_else(|_| unreachable!());
        initialize(&mut connection, Some(&backup_dir), 2, 1).unwrap_or_else(|_| unreachable!());
        let backup_path = fs::read_dir(&backup_dir)
            .unwrap_or_else(|_| unreachable!())
            .next()
            .and_then(Result::ok)
            .map_or_else(|| unreachable!(), |entry| entry.path());
        let backup = Connection::open(backup_path).unwrap_or_else(|_| unreachable!());
        let marker: String = backup
            .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(marker, "preserved");
        let migrated: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(migrated, 1);
        drop(backup);
        drop(connection);
        fs::remove_dir_all(root).unwrap_or_else(|_| unreachable!());
    }

    #[test]
    fn backup_restore_and_retention_are_consistent() {
        let root = temporary_directory("backup-test");
        let backup_dir = root.join("backups");
        fs::create_dir_all(&root).unwrap_or_else(|_| unreachable!());
        let database_path = root.join("server.sqlite3");
        let database_url = format!("sqlite://{}", database_path.display());
        let mut connection = Connection::open(&database_path).unwrap_or_else(|_| unreachable!());
        initialize(&mut connection, Some(&backup_dir), 2, 1).unwrap_or_else(|_| unreachable!());
        connection
            .execute(
                "INSERT INTO users(user_id, username, password_hash, created_at) VALUES('u1', 'alice', 'hash-only', 1)",
                [],
            )
            .unwrap_or_else(|_| unreachable!());
        let first = create_backup(&connection, &backup_dir, 2, 1, "manual", 10)
            .unwrap_or_else(|_| unreachable!());
        connection
            .execute(
                "UPDATE users SET username = 'changed' WHERE user_id = 'u1'",
                [],
            )
            .unwrap_or_else(|_| unreachable!());
        create_backup(&connection, &backup_dir, 2, 1, "manual", 11)
            .unwrap_or_else(|_| unreachable!());
        create_backup(&connection, &backup_dir, 2, 1, "manual", 12)
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            fs::read_dir(&backup_dir)
                .unwrap_or_else(|_| unreachable!())
                .count(),
            2
        );
        drop(connection);

        // Retention removed the oldest manual backup, so use a
        // dedicated source to verify recovery of the original snapshot.
        let source = Connection::open_in_memory().unwrap_or_else(|_| unreachable!());
        source
            .execute_batch(SERVER_SCHEMA_V1)
            .unwrap_or_else(|_| unreachable!());
        source
            .execute(
                "INSERT INTO users(user_id, username, password_hash, created_at) VALUES('u2', 'restored', 'hash-only', 1)",
                [],
            )
            .unwrap_or_else(|_| unreachable!());
        let restore_source =
            create_backup(&source, &root.join("restore-source"), 1, 1, "manual", 20)
                .unwrap_or_else(|_| unreachable!());
        let recovery = restore_database(&database_url, &restore_source, &backup_dir, 2, 21)
            .unwrap_or_else(|_| unreachable!());
        assert!(recovery.exists());
        let restored = Connection::open(database_path).unwrap_or_else(|_| unreachable!());
        let username: String = restored
            .query_row("SELECT username FROM users", [], |row| row.get(0))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(username, "restored");
        assert!(first.starts_with(&backup_dir));
        fs::remove_dir_all(root).unwrap_or_else(|_| unreachable!());
    }
}
