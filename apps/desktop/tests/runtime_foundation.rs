use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use lvos::{
    BackgroundProfileServices, CaptureAdmission, CaptureGate, DatabaseWorker, DesktopRuntime,
    ProfileLifecycle, UiDispatchError, UiDispatcher,
};
use lvos_core::{LanguageCode, UnixTimestamp, ValidationPolicy, prepare_content};
use lvos_storage::{HistoryEntry, ProfileMetadata, StoredContent, TranslationSnapshot};
use tempfile::tempdir;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn metadata(profile_id: Uuid, device_id: Uuid) -> ProfileMetadata {
    ProfileMetadata {
        profile_id,
        user_id: None,
        username: None,
        device_id,
        platform: "macos".to_owned(),
        server_origin: None,
        last_server_revision: 0,
        created_at: UnixTimestamp::from_seconds(100),
        updated_at: UnixTimestamp::from_seconds(100),
    }
}

fn history(source: &str, timestamp: i64) -> HistoryEntry {
    let prepared = prepare_content(
        source,
        LanguageCode::parse("en").unwrap_or_else(|error| unreachable!("fixture: {error}")),
        ValidationPolicy::new(NonZeroUsize::new(100).unwrap_or_else(|| unreachable!("fixture"))),
    )
    .unwrap_or_else(|error| unreachable!("fixture: {error}"));
    HistoryEntry {
        content: StoredContent {
            content_key: prepared.content_key(),
            key_version: prepared.key_version(),
            kind: prepared.kind(),
            source_lang: prepared.source_lang().clone(),
            source_text: prepared.source_text().to_owned(),
            canonical_text: prepared.canonical_text().to_owned(),
        },
        translation: TranslationSnapshot {
            target_lang: LanguageCode::parse("zh-CN")
                .unwrap_or_else(|error| unreachable!("fixture: {error}")),
            translation: "fixture".to_owned(),
            provider: "fixture".to_owned(),
            updated_at: UnixTimestamp::from_seconds(timestamp),
        },
        last_queried_at: UnixTimestamp::from_seconds(timestamp),
    }
}

#[test]
fn capture_is_single_flight_and_debounced() {
    let gate = CaptureGate::new(Duration::from_millis(100));
    let start = Instant::now();
    let permit = match gate.admit_capture(start) {
        CaptureAdmission::Admitted(permit) => permit,
        other => unreachable!("unexpected admission: {other:?}"),
    };
    assert!(matches!(gate.admit_capture(start), CaptureAdmission::Busy));
    drop(permit);
    assert!(matches!(
        gate.admit_capture(start + Duration::from_millis(50)),
        CaptureAdmission::Debounced
    ));
    assert!(matches!(
        gate.admit_capture(start + Duration::from_millis(101)),
        CaptureAdmission::Admitted(_)
    ));
}

#[test]
fn latest_generation_wins_and_cancels_previous_ticket() {
    let gate = CaptureGate::default();
    let first = gate.begin_query();
    assert!(gate.is_current(first.generation()));
    let second = gate.begin_query();
    assert!(first.cancellation().is_cancelled());
    assert!(!second.cancellation().is_cancelled());
    assert!(!gate.is_current(first.generation()));
    assert!(gate.is_current(second.generation()));

    let key = history("Invariant", 100).content.content_key;
    let flight = gate
        .begin_content_flight(key)
        .unwrap_or_else(|| unreachable!("fixture"));
    assert!(gate.begin_content_flight(key).is_none());
    drop(flight);
    assert!(gate.begin_content_flight(key).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_operations_run_on_one_non_caller_thread_and_persist() {
    let root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let device_id = Uuid::new_v4();
    let profile_id = Uuid::new_v4();
    let worker = DatabaseWorker::start(root.path().to_path_buf())
        .unwrap_or_else(|error| unreachable!("worker: {error}"));
    assert_ne!(worker.thread_id(), thread::current().id());
    worker
        .switch_profile(metadata(profile_id, device_id))
        .await
        .unwrap_or_else(|error| unreachable!("switch: {error}"));
    let entry = history("Invariant", 100);
    let key = entry.content.content_key;
    let observed = worker
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(thread::current().id())
        })
        .await
        .unwrap_or_else(|error| unreachable!("execute: {error}"));
    assert_eq!(observed, worker.thread_id());
    drop(worker);

    let reopened = DatabaseWorker::start(root.path().to_path_buf())
        .unwrap_or_else(|error| unreachable!("worker: {error}"));
    reopened
        .switch_profile(metadata(profile_id, device_id))
        .await
        .unwrap_or_else(|error| unreachable!("switch: {error}"));
    let count = reopened
        .execute(move |database| {
            Ok(database
                .query_stats(key)?
                .map(|stats| stats.device_query_count))
        })
        .await
        .unwrap_or_else(|error| unreachable!("query: {error}"));
    assert_eq!(count, Some(1));
}

#[derive(Debug, Default)]
struct ServiceProbe {
    stopped: Arc<Mutex<HashMap<Uuid, bool>>>,
}

impl BackgroundProfileServices for ServiceProbe {
    fn start(&self, profile_id: Uuid, cancellation: CancellationToken) -> Vec<JoinHandle<()>> {
        let stopped = Arc::clone(&self.stopped);
        vec![tokio::spawn(async move {
            cancellation.cancelled().await;
            if let Ok(mut values) = stopped.lock() {
                values.insert(profile_id, true);
            }
        })]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_switch_stops_old_services_and_isolates_databases() {
    let root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let device_id = Uuid::new_v4();
    let profile_a = Uuid::new_v4();
    let profile_b = Uuid::new_v4();
    let services = Arc::new(ServiceProbe::default());
    let worker = DatabaseWorker::start(root.path().to_path_buf())
        .unwrap_or_else(|error| unreachable!("worker: {error}"));
    let mut lifecycle = ProfileLifecycle::new(worker, Arc::clone(&services));
    lifecycle
        .switch_profile(metadata(profile_a, device_id), Duration::from_secs(1))
        .await
        .unwrap_or_else(|error| unreachable!("switch A: {error}"));
    let entry = history("Invariant", 100);
    lifecycle
        .database()
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|error| unreachable!("write A: {error}"));
    let outcome = lifecycle
        .switch_profile(metadata(profile_b, device_id), Duration::from_secs(1))
        .await
        .unwrap_or_else(|error| unreachable!("switch B: {error}"));
    assert_eq!(outcome.profile_id, profile_b);
    assert!(
        services
            .stopped
            .lock()
            .is_ok_and(|values| values.get(&profile_a) == Some(&true))
    );
    let rows = lifecycle
        .database()
        .execute(|database| database.search_history("Invariant", 10))
        .await
        .unwrap_or_else(|error| unreachable!("read B: {error}"));
    assert!(rows.is_empty());
    lifecycle
        .shutdown(Duration::from_secs(1))
        .await
        .unwrap_or_else(|error| unreachable!("shutdown: {error}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_resolution_reuses_existing_user_profile_without_merging() {
    let root = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let device_id = Uuid::new_v4();
    let profile_a = Uuid::new_v4();
    let profile_b = Uuid::new_v4();
    let user_a = Uuid::new_v4();
    let worker = DatabaseWorker::start(root.path().to_path_buf())
        .unwrap_or_else(|error| unreachable!("worker: {error}"));
    worker
        .switch_profile(metadata(profile_a, device_id))
        .await
        .unwrap_or_else(|error| unreachable!("switch A: {error}"));
    worker
        .resolve_account_profile(
            user_a,
            "alice".to_owned(),
            "https://example.invalid".to_owned(),
            UnixTimestamp::from_seconds(200),
        )
        .await
        .unwrap_or_else(|error| unreachable!("bind A: {error}"));
    let entry = history("Invariant", 100);
    let key = entry.content.content_key;
    worker
        .execute(move |database| {
            database.record_successful_query(&entry)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|error| unreachable!("write A: {error}"));
    worker
        .switch_profile(metadata(profile_b, device_id))
        .await
        .unwrap_or_else(|error| unreachable!("switch B: {error}"));

    let resolved = worker
        .resolve_account_profile(
            user_a,
            "alice-new".to_owned(),
            "https://example.invalid".to_owned(),
            UnixTimestamp::from_seconds(300),
        )
        .await
        .unwrap_or_else(|error| unreachable!("resolve A: {error}"));
    assert_eq!(resolved.profile_id, profile_a);
    assert_eq!(resolved.username.as_deref(), Some("alice-new"));
    let count = worker
        .execute(move |database| {
            Ok(database
                .query_stats(key)?
                .map(|stats| stats.device_query_count))
        })
        .await
        .unwrap_or_else(|error| unreachable!("read A: {error}"));
    assert_eq!(count, Some(1));
}

#[derive(Clone, Debug, Default)]
struct TestUiDispatcher {
    callbacks: Arc<Mutex<Vec<thread::ThreadId>>>,
}

impl UiDispatcher for TestUiDispatcher {
    fn dispatch(&self, callback: impl FnOnce() + Send + 'static) -> Result<(), UiDispatchError> {
        callback();
        if let Ok(mut callbacks) = self.callbacks.lock() {
            callbacks.push(thread::current().id());
        }
        Ok(())
    }
}

#[test]
fn desktop_runtime_runs_background_work_and_uses_ui_dispatcher() {
    let dispatcher = TestUiDispatcher::default();
    let observations = Arc::clone(&dispatcher.callbacks);
    let runtime = DesktopRuntime::try_new(dispatcher)
        .unwrap_or_else(|error| unreachable!("runtime: {error}"));
    let (background_sender, background_receiver) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let _ = background_sender.send(42_u8);
    });
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    runtime
        .dispatch_ui(move || {
            let _ = sender.send(());
        })
        .unwrap_or_else(|error| unreachable!("dispatch: {error}"));
    assert!(receiver.recv_timeout(Duration::from_secs(1)).is_ok());
    assert_eq!(
        background_receiver.recv_timeout(Duration::from_secs(1)),
        Ok(42)
    );
    assert!(observations.lock().is_ok_and(|values| values.len() == 1));
    runtime.shutdown();
}

#[test]
fn profile_database_path_is_stable_for_account_identity() {
    let root = Path::new("fixture-root");
    let profile_id = Uuid::nil();
    let path = lvos_storage::ProfilePaths::new(root, profile_id);
    assert!(
        path.database()
            .ends_with(format!("profile-{profile_id}.sqlite3"))
    );
}
