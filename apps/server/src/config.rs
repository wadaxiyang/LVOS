use std::{fmt, net::SocketAddr, num::ParseIntError, path::PathBuf};

use lvos_auth::{DEFAULT_ACCESS_TOKEN_TTL_MINUTES, DEFAULT_REFRESH_SESSION_IDLE_TTL_DAYS};
use lvos_core::{DEFAULT_SERVER_PORT, DEFAULT_SERVER_URL};

const DEFAULT_DATABASE_URL: &str = "sqlite://data/lvos.sqlite3";
const DEFAULT_USERNAME: &str = "default";
const EXAMPLE_PASSWORD: &str = "change-me-dev";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppEnvironment {
    Development,
    Production,
}

#[derive(Clone)]
pub struct ServerConfig {
    pub environment: AppEnvironment,
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub public_server_url: String,
    pub bootstrap_default_user: bool,
    pub default_username: String,
    pub default_password: Option<String>,
    pub access_token_ttl_seconds: i64,
    pub refresh_idle_ttl_seconds: i64,
    pub login_rate_limit_enabled: bool,
    pub login_rate_limit_max_failures: u32,
    pub login_rate_limit_window_seconds: i64,
    pub max_request_body_bytes: usize,
    pub backup_enabled: bool,
    pub backup_dir: PathBuf,
    pub backup_retention_count: usize,
    pub backup_interval_seconds: u64,
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("environment", &self.environment)
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &self.database_url)
            .field("public_server_url", &self.public_server_url)
            .field("bootstrap_default_user", &self.bootstrap_default_user)
            .field("default_username", &self.default_username)
            .field(
                "default_password",
                &self.default_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("access_token_ttl_seconds", &self.access_token_ttl_seconds)
            .field("refresh_idle_ttl_seconds", &self.refresh_idle_ttl_seconds)
            .field("login_rate_limit_enabled", &self.login_rate_limit_enabled)
            .field(
                "login_rate_limit_max_failures",
                &self.login_rate_limit_max_failures,
            )
            .field(
                "login_rate_limit_window_seconds",
                &self.login_rate_limit_window_seconds,
            )
            .field("max_request_body_bytes", &self.max_request_body_bytes)
            .field("backup_enabled", &self.backup_enabled)
            .field("backup_dir", &self.backup_dir)
            .field("backup_retention_count", &self.backup_retention_count)
            .field("backup_interval_seconds", &self.backup_interval_seconds)
            .finish()
    }
}

impl ServerConfig {
    /// Loads LVOS-owned environment variables and validates fail-closed production requirements.
    ///
    /// # Errors
    /// Returns a typed configuration error for malformed or unsafe values.
    pub fn from_environment() -> Result<Self, ConfigError> {
        let environment = match env_string("LVOS_APP_ENV", "development").as_str() {
            "development" => AppEnvironment::Development,
            "production" => AppEnvironment::Production,
            _ => return Err(ConfigError::InvalidEnvironment),
        };
        let config = Self {
            environment,
            bind_addr: env_string("LVOS_BIND_ADDR", &format!("0.0.0.0:{DEFAULT_SERVER_PORT}"))
                .parse()
                .map_err(|_| ConfigError::InvalidBindAddress)?,
            database_url: env_string("LVOS_DATABASE_URL", DEFAULT_DATABASE_URL),
            public_server_url: env_string("LVOS_PUBLIC_SERVER_URL", DEFAULT_SERVER_URL),
            bootstrap_default_user: env_bool("LVOS_BOOTSTRAP_DEFAULT_USER", true)?,
            default_username: env_string("LVOS_DEFAULT_USERNAME", DEFAULT_USERNAME),
            default_password: std::env::var("LVOS_DEFAULT_PASSWORD")
                .ok()
                .or_else(|| Some(EXAMPLE_PASSWORD.to_owned())),
            access_token_ttl_seconds: minutes_to_seconds(env_u64(
                "LVOS_ACCESS_TOKEN_TTL_MINUTES",
                DEFAULT_ACCESS_TOKEN_TTL_MINUTES,
            )?)?,
            refresh_idle_ttl_seconds: days_to_seconds(env_u64(
                "LVOS_REFRESH_SESSION_IDLE_TTL_DAYS",
                DEFAULT_REFRESH_SESSION_IDLE_TTL_DAYS,
            )?)?,
            login_rate_limit_enabled: env_bool("LVOS_LOGIN_RATE_LIMIT_ENABLED", true)?,
            login_rate_limit_max_failures: u32::try_from(env_u64(
                "LVOS_LOGIN_RATE_LIMIT_MAX_FAILURES",
                5,
            )?)
            .map_err(|_| ConfigError::InvalidNumber)?,
            login_rate_limit_window_seconds: i64::try_from(env_u64(
                "LVOS_LOGIN_RATE_LIMIT_WINDOW_SECONDS",
                60,
            )?)
            .map_err(|_| ConfigError::InvalidNumber)?,
            max_request_body_bytes: usize::try_from(env_u64(
                "LVOS_MAX_REQUEST_BODY_BYTES",
                1_048_576,
            )?)
            .map_err(|_| ConfigError::InvalidNumber)?,
            backup_enabled: env_bool("LVOS_BACKUP_ENABLED", true)?,
            backup_dir: PathBuf::from(env_string("LVOS_BACKUP_DIR", "./backups")),
            backup_retention_count: usize::try_from(env_u64("LVOS_BACKUP_RETENTION_COUNT", 14)?)
                .map_err(|_| ConfigError::InvalidNumber)?,
            backup_interval_seconds: env_u64("LVOS_BACKUP_INTERVAL_HOURS", 24)?
                .checked_mul(60 * 60)
                .ok_or(ConfigError::InvalidNumber)?,
        };
        config.validate()?;
        Ok(config)
    }

    /// Validates security and resource boundaries without exposing secret values.
    ///
    /// # Errors
    /// Returns a typed configuration error for unsafe or unusable settings.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database_url.trim().is_empty()
            || self.default_username.trim().is_empty()
            || self.default_username.len() > 128
            || self.access_token_ttl_seconds <= 0
            || self.refresh_idle_ttl_seconds <= 0
            || self.login_rate_limit_max_failures == 0
            || self.login_rate_limit_window_seconds <= 0
            || self.max_request_body_bytes == 0
            || self.backup_dir.as_os_str().is_empty()
            || self.backup_retention_count == 0
            || self.backup_interval_seconds == 0
        {
            return Err(ConfigError::InvalidBoundary);
        }
        if self.bootstrap_default_user
            && self
                .default_password
                .as_deref()
                .is_none_or(|password| password.is_empty() || password.len() > 1_024)
        {
            return Err(ConfigError::MissingBootstrapPassword);
        }
        if self.environment == AppEnvironment::Production {
            if self.bootstrap_default_user
                && self.default_password.as_deref().is_none_or(|password| {
                    password.trim().len() < 12
                        || matches!(password, EXAMPLE_PASSWORD | "password" | "admin")
                })
            {
                return Err(ConfigError::UnsafeProductionPassword);
            }
            if !self.public_server_url.starts_with("https://")
                || self.public_server_url.len() <= "https://".len()
                || self.public_server_url.chars().any(char::is_whitespace)
            {
                return Err(ConfigError::UnsafeProductionPublicUrl);
            }
            if self.database_url == ":memory:" || self.database_url.ends_with(":memory:") {
                return Err(ConfigError::UnsafeProductionDatabase);
            }
        }
        Ok(())
    }
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn env_bool(name: &str, default: bool) -> Result<bool, ConfigError> {
    match std::env::var(name) {
        Ok(value) => match value.as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(ConfigError::InvalidBoolean),
        },
        Err(_) => Ok(default),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64, ConfigError> {
    std::env::var(name).map_or(Ok(default), |value| {
        value.parse().map_err(ConfigError::from)
    })
}

fn minutes_to_seconds(minutes: u64) -> Result<i64, ConfigError> {
    i64::try_from(minutes.checked_mul(60).ok_or(ConfigError::InvalidNumber)?)
        .map_err(|_| ConfigError::InvalidNumber)
}

fn days_to_seconds(days: u64) -> Result<i64, ConfigError> {
    i64::try_from(
        days.checked_mul(24)
            .and_then(|value| value.checked_mul(60 * 60))
            .ok_or(ConfigError::InvalidNumber)?,
    )
    .map_err(|_| ConfigError::InvalidNumber)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidEnvironment,
    InvalidBindAddress,
    InvalidBoolean,
    InvalidNumber,
    InvalidBoundary,
    MissingBootstrapPassword,
    UnsafeProductionPassword,
    UnsafeProductionPublicUrl,
    UnsafeProductionDatabase,
}

impl From<ParseIntError> for ConfigError {
    fn from(_: ParseIntError) -> Self {
        Self::InvalidNumber
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEnvironment => "LVOS_APP_ENV must be development or production",
            Self::InvalidBindAddress => "LVOS_BIND_ADDR is invalid",
            Self::InvalidBoolean => "an LVOS boolean setting is invalid",
            Self::InvalidNumber => "an LVOS numeric setting is invalid",
            Self::InvalidBoundary => "an LVOS safety boundary must be greater than zero",
            Self::MissingBootstrapPassword => "bootstrap requires LVOS_DEFAULT_PASSWORD",
            Self::UnsafeProductionPassword => {
                "production rejects missing, example, or materially unsafe bootstrap passwords"
            }
            Self::UnsafeProductionPublicUrl => {
                "production requires an HTTPS LVOS_PUBLIC_SERVER_URL"
            }
            Self::UnsafeProductionDatabase => "production requires a persistent LVOS_DATABASE_URL",
        })
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ServerConfig {
        ServerConfig {
            environment: AppEnvironment::Development,
            bind_addr: "127.0.0.1:7770".parse().unwrap_or_else(|_| unreachable!()),
            database_url: ":memory:".to_owned(),
            public_server_url: DEFAULT_SERVER_URL.to_owned(),
            bootstrap_default_user: true,
            default_username: "default".to_owned(),
            default_password: Some(EXAMPLE_PASSWORD.to_owned()),
            access_token_ttl_seconds: 3_600,
            refresh_idle_ttl_seconds: 90 * 24 * 60 * 60,
            login_rate_limit_enabled: true,
            login_rate_limit_max_failures: 5,
            login_rate_limit_window_seconds: 60,
            max_request_body_bytes: 1_048_576,
            backup_enabled: true,
            backup_dir: PathBuf::from("./backups"),
            backup_retention_count: 14,
            backup_interval_seconds: 24 * 60 * 60,
        }
    }

    #[test]
    fn production_rejects_example_password() {
        let mut config = valid_config();
        config.environment = AppEnvironment::Production;
        config.database_url = "sqlite://data/lvos.sqlite3".to_owned();
        assert_eq!(
            config.validate(),
            Err(ConfigError::UnsafeProductionPassword)
        );
    }

    #[test]
    fn production_allows_disabled_bootstrap_without_a_password() {
        let mut config = valid_config();
        config.environment = AppEnvironment::Production;
        config.database_url = "sqlite://data/lvos.sqlite3".to_owned();
        config.bootstrap_default_user = false;
        config.default_password = None;
        assert_eq!(config.validate(), Ok(()));
    }
}
