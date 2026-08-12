use lvos_core::{DEFAULT_SERVER_PORT, PRODUCT_NAME, SOFTWARE_VERSION};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ServerConfig {
    app_env: String,
    bind_addr: String,
    default_password: Option<String>,
}

impl ServerConfig {
    fn from_environment() -> Result<Self, ConfigError> {
        let app_env = std::env::var("LVOS_APP_ENV").unwrap_or_else(|_| "development".into());
        let bind_addr = std::env::var("LVOS_BIND_ADDR")
            .unwrap_or_else(|_| format!("0.0.0.0:{DEFAULT_SERVER_PORT}"));
        let default_password = std::env::var("LVOS_DEFAULT_PASSWORD").ok();
        let config = Self {
            app_env,
            bind_addr,
            default_password,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.app_env == "production"
            && matches!(
                self.default_password.as_deref(),
                None | Some("" | "change-me-dev")
            )
        {
            return Err(ConfigError::UnsafeProductionPassword);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigError {
    UnsafeProductionPassword,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("production requires a non-example bootstrap password")
    }
}

impl std::error::Error for ConfigError {}

fn main() -> Result<(), ConfigError> {
    init_tracing();
    let config = ServerConfig::from_environment()?;
    tracing::info!(
        version = SOFTWARE_VERSION,
        bind_addr = config.bind_addr,
        "{PRODUCT_NAME} Server configured"
    );
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_rejects_example_password() {
        let config = ServerConfig {
            app_env: "production".into(),
            bind_addr: "127.0.0.1:7770".into(),
            default_password: Some("change-me-dev".into()),
        };
        assert_eq!(
            config.validate(),
            Err(ConfigError::UnsafeProductionPassword)
        );
    }

    #[test]
    fn development_allows_example_password() {
        let config = ServerConfig {
            app_env: "development".into(),
            bind_addr: "127.0.0.1:7770".into(),
            default_password: Some("change-me-dev".into()),
        };
        assert_eq!(config.validate(), Ok(()));
    }
}
