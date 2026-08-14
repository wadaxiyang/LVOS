#![forbid(unsafe_code)]

use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use lvos_core::{PRODUCT_NAME, SOFTWARE_VERSION};
use lvos_server::{ServerConfig, ServerRepository, build_app};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = dotenvy::dotenv();
    init_tracing();
    let config = ServerConfig::from_environment()?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let repository = ServerRepository::open(&config.database_url, now)?;
    let bind_addr = config.bind_addr;
    let app = build_app(config, repository).await?;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(
        version = SOFTWARE_VERSION,
        bind_addr = %bind_addr,
        "{PRODUCT_NAME} Server listening"
    );
    axum::serve(listener, app).await?;
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
