#![forbid(unsafe_code)]

use std::{
    error::Error,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use lvos_core::{PRODUCT_NAME, SOFTWARE_VERSION};
use lvos_server::{BackupService, ServerConfig, ServerRepository, build_app};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = dotenvy::dotenv();
    init_tracing();
    let config = ServerConfig::from_environment()?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|value| value == "healthcheck")
    {
        if arguments.len() != 1 {
            return Err("usage: lvos-server healthcheck".into());
        }
        run_healthcheck(config.bind_addr)?;
        return Ok(());
    }
    if arguments.first().is_some_and(|value| value == "restore") {
        let source = arguments
            .get(1)
            .ok_or("usage: lvos-server restore <backup-path>")?;
        let recovery = ServerRepository::restore(
            &config.database_url,
            Path::new(source),
            &config.backup_dir,
            config.backup_retention_count,
            now,
        )?;
        tracing::info!(
            recovery_backup = %recovery.display(),
            "database restore completed; restart the server"
        );
        return Ok(());
    }
    if arguments.len() > 1 || arguments.first().is_some_and(|value| value != "backup") {
        return Err("usage: lvos-server [backup|healthcheck|restore <backup-path>]".into());
    }
    let repository = ServerRepository::open(
        &config.database_url,
        &config.backup_dir,
        config.backup_retention_count,
        now,
    )?;
    let backup_service = BackupService::new(
        repository.clone(),
        config.backup_dir.clone(),
        config.backup_retention_count,
        Duration::from_secs(config.backup_interval_seconds),
    );
    if arguments.first().is_some_and(|value| value == "backup") {
        let backup = backup_service.run("manual")?;
        tracing::info!(backup_file = %backup.display(), "manual database backup completed");
        return Ok(());
    }
    let bind_addr = config.bind_addr;
    let backup_enabled = config.backup_enabled;
    let app = build_app(config, repository).await?;
    let _backup_task = backup_enabled.then(|| backup_service.start_periodic());
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(
        version = SOFTWARE_VERSION,
        bind_addr = %bind_addr,
        "{PRODUCT_NAME} Server listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn run_healthcheck(bind_addr: SocketAddr) -> Result<(), Box<dyn Error>> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bind_addr.port());
    let timeout = Duration::from_secs(4);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(
        b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )?;
    let mut response = [0_u8; 128];
    let length = stream.read(&mut response)?;
    let status_line = std::str::from_utf8(&response[..length])?
        .lines()
        .next()
        .unwrap_or_default();
    if status_line.contains(" 200 ") {
        Ok(())
    } else {
        Err("LVOS Server health endpoint did not return HTTP 200".into())
    }
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
