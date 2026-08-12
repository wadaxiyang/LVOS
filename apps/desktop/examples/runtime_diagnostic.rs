use std::{sync::Arc, thread, time::Duration};

use lvos::{BackgroundProfileServices, DatabaseWorker, ProfileLifecycle};
use lvos_core::UnixTimestamp;
use lvos_storage::ProfileMetadata;
use tempfile::tempdir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug)]
struct DiagnosticServices;

impl BackgroundProfileServices for DiagnosticServices {
    fn start(&self, profile_id: Uuid, cancellation: CancellationToken) -> Vec<JoinHandle<()>> {
        vec![tokio::spawn(async move {
            println!("background_started={profile_id}");
            cancellation.cancelled().await;
            println!("background_stopped={profile_id}");
        })]
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let profile_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let worker = DatabaseWorker::start(root.path().to_path_buf())?;
    let database_thread = worker.thread_id();
    let mut lifecycle = ProfileLifecycle::new(worker, Arc::new(DiagnosticServices));
    lifecycle
        .switch_profile(
            ProfileMetadata {
                profile_id,
                user_id: None,
                username: None,
                device_id,
                platform: "macos".to_owned(),
                server_origin: None,
                last_server_revision: 0,
                created_at: UnixTimestamp::from_seconds(1_780_000_000),
                updated_at: UnixTimestamp::from_seconds(1_780_000_000),
            },
            Duration::from_secs(1),
        )
        .await?;
    println!("main_thread={:?}", thread::current().id());
    println!("database_thread={database_thread:?}");
    println!(
        "active_profile={}",
        lifecycle.database().active_profile_id().await?
    );
    lifecycle.shutdown(Duration::from_secs(1)).await?;
    println!("shutdown=clean");
    Ok(())
}
