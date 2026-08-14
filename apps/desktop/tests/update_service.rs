use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use lvos::{
    GitHubUpdateConfig, GitHubUpdateService, ReleasePageOpener, UpdateCheckOutcome,
    UpdateCoordinator, UpdateTarget, UpdateTransport,
};
use lvos_core::{
    GITHUB_LATEST_RELEASE_API, GITHUB_RELEASES_URL, MAX_GITHUB_RELEASE_BYTES,
    MAX_UPDATE_MANIFEST_BYTES, UPDATE_CHECK_INTERVAL_SECONDS, UnixTimestamp, UpdateArtifact,
    UpdateError, UpdateInfo, UpdateService,
};
use serde_json::{Value, json};
use tempfile::tempdir;

const VERSION: &str = "0.2.0";
const MAC_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WINDOWS_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn release_page() -> String {
    format!("{GITHUB_RELEASES_URL}/tag/v{VERSION}")
}

fn manifest_url() -> String {
    format!("{GITHUB_RELEASES_URL}/download/v{VERSION}/lvos-update-stable.json")
}

fn artifact_url(platform: &str, architecture: &str) -> String {
    format!(
        "{GITHUB_RELEASES_URL}/download/v{VERSION}/LVOS-{VERSION}-{platform}-{architecture}.zip"
    )
}

fn valid_manifest() -> Value {
    json!({
        "manifest_version": 1,
        "product": "LVOS",
        "channel": "stable",
        "version": VERSION,
        "release_page": release_page(),
        "artifacts": [
            {
                "platform": "macos",
                "architecture": "arm64",
                "name": format!("LVOS-{VERSION}-macos-arm64.zip"),
                "size_bytes": 120,
                "sha256": MAC_HASH,
                "download_url": artifact_url("macos", "arm64")
            },
            {
                "platform": "windows",
                "architecture": "x86_64",
                "name": format!("LVOS-{VERSION}-windows-x86_64.zip"),
                "size_bytes": 240,
                "sha256": WINDOWS_HASH,
                "download_url": artifact_url("windows", "x86_64")
            }
        ]
    })
}

fn valid_release() -> Value {
    json!({
        "tag_name": format!("v{VERSION}"),
        "html_url": release_page(),
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": "lvos-update-stable.json",
                "state": "uploaded",
                "size": 800,
                "digest": null,
                "browser_download_url": manifest_url()
            },
            {
                "name": format!("LVOS-{VERSION}-macos-arm64.zip"),
                "state": "uploaded",
                "size": 120,
                "digest": format!("sha256:{MAC_HASH}"),
                "browser_download_url": artifact_url("macos", "arm64")
            },
            {
                "name": format!("LVOS-{VERSION}-windows-x86_64.zip"),
                "state": "uploaded",
                "size": 240,
                "digest": format!("sha256:{WINDOWS_HASH}"),
                "browser_download_url": artifact_url("windows", "x86_64")
            }
        ]
    })
}

#[derive(Clone, Debug)]
struct StaticTransport {
    responses: HashMap<String, Vec<u8>>,
}

#[async_trait]
impl UpdateTransport for StaticTransport {
    async fn get(&self, url: &str, maximum_bytes: usize) -> Result<Vec<u8>, UpdateError> {
        let response = self.responses.get(url).ok_or(UpdateError::Network)?;
        if response.len() > maximum_bytes {
            return Err(UpdateError::Network);
        }
        Ok(response.clone())
    }
}

fn service(
    release: &Value,
    manifest: &Value,
    target: UpdateTarget,
) -> GitHubUpdateService<StaticTransport> {
    GitHubUpdateService::new(fixture_transport(release, manifest), fixture_config(target))
}

fn fixture_transport(release: &Value, manifest: &Value) -> StaticTransport {
    let responses = HashMap::from([
        (
            GITHUB_LATEST_RELEASE_API.to_owned(),
            serde_json::to_vec(&release)
                .unwrap_or_else(|error| unreachable!("release fixture: {error}")),
        ),
        (
            manifest_url(),
            serde_json::to_vec(&manifest)
                .unwrap_or_else(|error| unreachable!("manifest fixture: {error}")),
        ),
    ]);
    StaticTransport { responses }
}

fn fixture_config(target: UpdateTarget) -> GitHubUpdateConfig {
    GitHubUpdateConfig {
        owner: "wadaxiyang".to_owned(),
        repository: "LVOS".to_owned(),
        release_api_url: GITHUB_LATEST_RELEASE_API.to_owned(),
        release_page_root: GITHUB_RELEASES_URL.to_owned(),
        channel: "stable".to_owned(),
        current_version: "0.1.0".to_owned(),
        target,
    }
}

fn macos_target() -> UpdateTarget {
    UpdateTarget {
        platform: "macos".to_owned(),
        architecture: "arm64".to_owned(),
    }
}

#[tokio::test]
async fn valid_release_selects_current_target_and_reports_a_newer_version() {
    let service = service(&valid_release(), &valid_manifest(), macos_target());
    let info = service
        .check()
        .await
        .unwrap_or_else(|error| unreachable!("check: {error}"));
    assert!(info.available);
    assert_eq!(info.current_version, "0.1.0");
    assert_eq!(info.version, VERSION);
    assert_eq!(info.channel, "stable");
    assert_eq!(info.release_page, release_page());
    assert_eq!(info.artifact.platform, "macos");
    assert_eq!(info.artifact.architecture, "arm64");
    assert_eq!(info.artifact.sha256, MAC_HASH);
    assert_eq!(info.artifact.size_bytes, 120);
    assert_eq!(
        service
            .download(VERSION)
            .await
            .unwrap_or_else(|error| unreachable!("descriptor: {error}")),
        info.artifact
    );
}

#[tokio::test]
async fn same_version_is_up_to_date_and_wrong_descriptor_version_is_rejected() {
    let mut config = fixture_config(macos_target());
    config.current_version = VERSION.to_owned();
    let service = GitHubUpdateService::new(
        fixture_transport(&valid_release(), &valid_manifest()),
        config,
    );
    let info = service
        .check()
        .await
        .unwrap_or_else(|error| unreachable!("check: {error}"));
    assert!(!info.available);
    assert!(matches!(
        service.download("9.9.9").await,
        Err(UpdateError::InvalidVersion)
    ));
}

#[tokio::test]
async fn release_source_manifest_and_integrity_tampering_fail_closed() {
    let mut source_tamper = valid_release();
    source_tamper["assets"][0]["browser_download_url"] = json!("https://example.com/update.json");
    assert!(matches!(
        service(&source_tamper, &valid_manifest(), macos_target())
            .check()
            .await,
        Err(UpdateError::InvalidReleaseSource)
    ));

    let mut channel_tamper = valid_manifest();
    channel_tamper["channel"] = json!("nightly");
    assert!(matches!(
        service(&valid_release(), &channel_tamper, macos_target())
            .check()
            .await,
        Err(UpdateError::InvalidManifest)
    ));

    let mut digest_tamper = valid_release();
    digest_tamper["assets"][1]["digest"] = json!(format!("sha256:{WINDOWS_HASH}"));
    assert!(
        service(&valid_release(), &valid_manifest(), macos_target())
            .download(VERSION)
            .await
            .is_ok()
    );
    assert!(matches!(
        service(&digest_tamper, &valid_manifest(), macos_target())
            .check()
            .await,
        Err(UpdateError::IntegrityMismatch)
    ));

    let mut unknown_field = valid_manifest();
    unknown_field["unexpected"] = json!(true);
    assert!(matches!(
        service(&valid_release(), &unknown_field, macos_target())
            .check()
            .await,
        Err(UpdateError::InvalidManifest)
    ));
}

#[tokio::test]
async fn unsupported_channel_platform_and_network_failure_are_typed() {
    let mut config = fixture_config(macos_target());
    config.channel = "nightly".to_owned();
    let unsupported_channel = GitHubUpdateService::new(
        fixture_transport(&valid_release(), &valid_manifest()),
        config,
    );
    assert!(matches!(
        unsupported_channel.check().await,
        Err(UpdateError::UnsupportedChannel)
    ));
    let unsupported_target = UpdateTarget {
        platform: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
    };
    assert!(matches!(
        service(&valid_release(), &valid_manifest(), unsupported_target)
            .check()
            .await,
        Err(UpdateError::UnsupportedPlatform)
    ));
    let transport = StaticTransport {
        responses: HashMap::new(),
    };
    let config = fixture_config(macos_target());
    assert!(matches!(
        GitHubUpdateService::new(transport, config).check().await,
        Err(UpdateError::Network)
    ));
}

#[tokio::test]
async fn oversized_release_and_manifest_documents_are_rejected_before_parsing() {
    let oversized_release = StaticTransport {
        responses: HashMap::from([(
            GITHUB_LATEST_RELEASE_API.to_owned(),
            vec![b' '; MAX_GITHUB_RELEASE_BYTES + 1],
        )]),
    };
    assert!(matches!(
        GitHubUpdateService::new(oversized_release, fixture_config(macos_target()))
            .check()
            .await,
        Err(UpdateError::Network)
    ));

    let oversized_manifest = StaticTransport {
        responses: HashMap::from([
            (
                GITHUB_LATEST_RELEASE_API.to_owned(),
                serde_json::to_vec(&valid_release())
                    .unwrap_or_else(|error| unreachable!("release fixture: {error}")),
            ),
            (manifest_url(), vec![b' '; MAX_UPDATE_MANIFEST_BYTES + 1]),
        ]),
    };
    assert!(matches!(
        GitHubUpdateService::new(oversized_manifest, fixture_config(macos_target()))
            .check()
            .await,
        Err(UpdateError::Network)
    ));
}

#[derive(Clone, Debug)]
struct StaticService {
    result: Result<UpdateInfo, UpdateError>,
}

impl UpdateService for StaticService {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<UpdateInfo, UpdateError>> + Send + '_>> {
        Box::pin(std::future::ready(self.result.clone()))
    }

    fn download(
        &self,
        _version: &str,
    ) -> Pin<Box<dyn Future<Output = Result<UpdateArtifact, UpdateError>> + Send + '_>> {
        Box::pin(std::future::ready(Err(UpdateError::UnsupportedPlatform)))
    }
}

#[derive(Debug, Default)]
struct RecordingOpener {
    opened: Mutex<Vec<String>>,
}

impl ReleasePageOpener for RecordingOpener {
    fn open(&self, release_page: &str) -> Result<(), UpdateError> {
        self.opened
            .lock()
            .map_err(|_| UpdateError::InvalidReleaseSource)?
            .push(release_page.to_owned());
        Ok(())
    }
}

fn available_info() -> UpdateInfo {
    UpdateInfo {
        current_version: "0.1.0".to_owned(),
        version: VERSION.to_owned(),
        channel: "stable".to_owned(),
        release_page: release_page(),
        available: true,
        artifact: UpdateArtifact {
            version: VERSION.to_owned(),
            name: format!("LVOS-{VERSION}-macos-arm64.zip"),
            platform: "macos".to_owned(),
            architecture: "arm64".to_owned(),
            download_url: artifact_url("macos", "arm64"),
            sha256: MAC_HASH.to_owned(),
            size_bytes: 120,
        },
    }
}

#[tokio::test]
async fn startup_is_persistently_low_frequency_while_manual_check_and_open_are_explicit() {
    let directory = tempdir().unwrap_or_else(|error| unreachable!("fixture: {error}"));
    let opener = Arc::new(RecordingOpener::default());
    let coordinator = UpdateCoordinator::new(
        Arc::new(StaticService {
            result: Ok(available_info()),
        }),
        opener.clone(),
        directory.path(),
    );
    let first = coordinator
        .startup_check(UnixTimestamp::from_seconds(100))
        .await
        .unwrap_or_else(|error| unreachable!("startup: {error}"));
    let UpdateCheckOutcome::Available(info) = first else {
        unreachable!("available fixture")
    };
    assert!(matches!(
        coordinator
            .startup_check(UnixTimestamp::from_seconds(101))
            .await,
        Ok(UpdateCheckOutcome::Skipped)
    ));
    assert!(matches!(
        coordinator
            .startup_check(UnixTimestamp::from_seconds(
                100 + UPDATE_CHECK_INTERVAL_SECONDS
            ))
            .await,
        Ok(UpdateCheckOutcome::Available(_))
    ));
    assert!(matches!(
        coordinator
            .manual_check(UnixTimestamp::from_seconds(102))
            .await,
        Ok(UpdateCheckOutcome::Available(_))
    ));
    assert!(
        opener
            .opened
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .is_empty()
    );
    coordinator
        .open_available(&info)
        .unwrap_or_else(|error| unreachable!("open: {error}"));
    assert_eq!(
        opener
            .opened
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .as_slice(),
        &[release_page()]
    );
    let mut tampered = info;
    tampered.release_page = "https://example.com/release".to_owned();
    assert!(matches!(
        coordinator.open_available(&tampered),
        Err(UpdateError::InvalidReleaseSource)
    ));
}
