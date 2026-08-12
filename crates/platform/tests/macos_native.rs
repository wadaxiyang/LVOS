#![cfg(target_os = "macos")]

use std::{
    sync::{Arc, mpsc},
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use lvos_platform::{
    InstanceAcquisition, SingleInstanceService,
    macos::{MacOsCredentialStore, MacOsSingleInstanceService},
};
use tempfile::tempdir;

#[test]
fn second_instance_signals_primary_without_polling() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let service = MacOsSingleInstanceService::new(directory.path());
    let primary = match service
        .acquire()
        .unwrap_or_else(|error| unreachable!("primary: {error}"))
    {
        InstanceAcquisition::Primary(guard) => guard,
        InstanceAcquisition::Existing(_) => unreachable!("fixture acquired existing instance"),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    primary
        .set_open_handler(Arc::new(move || {
            let _ = sender.send(());
        }))
        .unwrap_or_else(|error| unreachable!("handler: {error}"));
    let secondary = match service
        .acquire()
        .unwrap_or_else(|error| unreachable!("secondary: {error}"))
    {
        InstanceAcquisition::Existing(guard) => guard,
        InstanceAcquisition::Primary(_) => unreachable!("second instance became primary"),
    };
    secondary
        .signal_existing()
        .unwrap_or_else(|error| unreachable!("signal: {error}"));
    receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|error| unreachable!("activation: {error}"));
}

#[test]
#[ignore = "requires access to the user's macOS login Keychain"]
fn keychain_store_round_trips_binary_secret_without_logging_it() {
    use lvos_auth::{CredentialKey, CredentialScope, CredentialStore};

    let scope = CredentialScope {
        server_origin: "https://stage06.invalid".to_owned(),
        user_id: format!(
            "test-user-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ),
        device_id: "test-device".to_owned(),
        key: CredentialKey::TencentTokenHubApiKey,
    };
    let store = MacOsCredentialStore;
    let secret = b"stage06-temporary-secret\0binary";
    store
        .set(&scope, secret)
        .unwrap_or_else(|error| unreachable!("set: {error}"));
    assert!(store.contains(&scope).unwrap_or(false));
    assert_eq!(
        store.get(&scope).unwrap_or_default().as_deref(),
        Some(secret.as_slice())
    );
    store
        .delete(&scope)
        .unwrap_or_else(|error| unreachable!("delete: {error}"));
    assert!(!store.contains(&scope).unwrap_or(true));
}
