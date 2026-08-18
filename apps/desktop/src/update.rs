use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use lvos_core::{
    DEFAULT_UPDATE_CHANNEL, GITHUB_LATEST_RELEASE_API, GITHUB_OWNER, GITHUB_RELEASES_URL,
    GITHUB_REPOSITORY, GITHUB_REST_API_VERSION, MAX_GITHUB_RELEASE_BYTES,
    MAX_UPDATE_ARTIFACT_BYTES, MAX_UPDATE_MANIFEST_BYTES, SOFTWARE_VERSION,
    UPDATE_CHECK_INTERVAL_SECONDS, UPDATE_MANIFEST_VERSION, UnixTimestamp, UpdateArtifact,
    UpdateError, UpdateInfo, UpdateService,
};
use reqwest::Url;
use semver::Version;
use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateTarget {
    pub platform: String,
    pub architecture: String,
}

impl UpdateTarget {
    /// Resolves the two V1 release targets.
    ///
    /// # Errors
    /// Returns an error outside macOS arm64 and Windows `x86_64`.
    pub fn current() -> Result<Self, UpdateError> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => Ok(Self {
                platform: "macos".to_owned(),
                architecture: "arm64".to_owned(),
            }),
            ("windows", "x86_64") => Ok(Self {
                platform: "windows".to_owned(),
                architecture: "x86_64".to_owned(),
            }),
            _ => Err(UpdateError::UnsupportedPlatform),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHubUpdateConfig {
    pub owner: String,
    pub repository: String,
    pub release_api_url: String,
    pub release_page_root: String,
    pub channel: String,
    pub current_version: String,
    pub target: UpdateTarget,
}

impl GitHubUpdateConfig {
    /// Builds the frozen LVOS public-release configuration.
    ///
    /// # Errors
    /// Returns an error for an unsupported channel or build target.
    pub fn lvos(channel: impl Into<String>) -> Result<Self, UpdateError> {
        let channel = channel.into();
        validate_channel(&channel)?;
        Ok(Self {
            owner: GITHUB_OWNER.to_owned(),
            repository: GITHUB_REPOSITORY.to_owned(),
            release_api_url: GITHUB_LATEST_RELEASE_API.to_owned(),
            release_page_root: GITHUB_RELEASES_URL.to_owned(),
            channel,
            current_version: SOFTWARE_VERSION.to_owned(),
            target: UpdateTarget::current()?,
        })
    }
}

#[async_trait]
pub trait UpdateTransport: Send + Sync {
    /// Fetches one bounded public metadata document.
    ///
    /// # Errors
    /// Returns a network error for transport, status, or response-size failure.
    async fn get(&self, url: &str, maximum_bytes: usize) -> Result<Vec<u8>, UpdateError>;
}

#[derive(Clone, Debug)]
pub struct HttpUpdateTransport {
    client: Arc<RwLock<reqwest::Client>>,
}

impl HttpUpdateTransport {
    /// Builds the bounded HTTPS client used only for update metadata.
    ///
    /// # Errors
    /// Returns an error when the HTTP client cannot be initialized.
    pub fn new(proxy_url: Option<&str>) -> Result<Self, UpdateError> {
        let client = build_update_client(proxy_url)?;
        Ok(Self {
            client: Arc::new(RwLock::new(client)),
        })
    }

    /// Replaces the client so the next update request uses the selected proxy.
    ///
    /// # Errors
    /// Returns a network error when the proxy client cannot be constructed.
    pub fn set_proxy_url(&self, proxy_url: Option<&str>) -> Result<(), UpdateError> {
        let client = build_update_client(proxy_url)?;
        *self
            .client
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = client;
        Ok(())
    }
}

fn build_update_client(proxy_url: Option<&str>) -> Result<reqwest::Client, UpdateError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(format!("LVOS/{SOFTWARE_VERSION}"));
    if let Some(proxy_url) = proxy_url {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url).map_err(|_| UpdateError::Network)?);
    }
    builder.build().map_err(|_| UpdateError::Network)
}

#[async_trait]
impl UpdateTransport for HttpUpdateTransport {
    async fn get(&self, url: &str, maximum_bytes: usize) -> Result<Vec<u8>, UpdateError> {
        let url = Url::parse(url).map_err(|_| UpdateError::InvalidReleaseSource)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(UpdateError::InvalidReleaseSource);
        }
        let client = self
            .client
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let response = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_REST_API_VERSION)
            .send()
            .await
            .map_err(|_| UpdateError::Network)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > maximum_bytes as u64)
        {
            return Err(UpdateError::Network);
        }
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| UpdateError::Network)?;
            if body.len().saturating_add(chunk.len()) > maximum_bytes {
                return Err(UpdateError::Network);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[derive(Clone, Debug)]
pub struct GitHubUpdateService<T> {
    transport: T,
    config: GitHubUpdateConfig,
}

impl<T> GitHubUpdateService<T> {
    #[must_use]
    pub const fn new(transport: T, config: GitHubUpdateConfig) -> Self {
        Self { transport, config }
    }

    #[must_use]
    pub const fn config(&self) -> &GitHubUpdateConfig {
        &self.config
    }
}

impl<T: UpdateTransport> GitHubUpdateService<T> {
    async fn validated_release(&self) -> Result<ValidatedRelease, UpdateError> {
        validate_config(&self.config)?;
        let release_bytes = self
            .transport
            .get(&self.config.release_api_url, MAX_GITHUB_RELEASE_BYTES)
            .await?;
        let release: GitHubRelease =
            serde_json::from_slice(&release_bytes).map_err(|_| UpdateError::InvalidManifest)?;
        if release.draft || (self.config.channel == DEFAULT_UPDATE_CHANNEL && release.prerelease) {
            return Err(UpdateError::InvalidManifest);
        }
        let manifest_name = format!("lvos-update-{}.json", self.config.channel);
        let manifest_asset = unique_asset(&release.assets, &manifest_name)?;
        validate_asset_source(&self.config, &release.tag_name, manifest_asset)?;
        let manifest_bytes = self
            .transport
            .get(
                &manifest_asset.browser_download_url,
                MAX_UPDATE_MANIFEST_BYTES,
            )
            .await?;
        let manifest: UpdateManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|_| UpdateError::InvalidManifest)?;
        validate_manifest_header(&self.config, &release, &manifest)?;
        let mut assets_by_name = HashMap::with_capacity(release.assets.len());
        for asset in &release.assets {
            if assets_by_name.insert(asset.name.as_str(), asset).is_some() {
                return Err(UpdateError::InvalidManifest);
            }
        }
        let mut targets = HashSet::new();
        let mut selected = None;
        for artifact in &manifest.artifacts {
            let target = (artifact.platform.as_str(), artifact.architecture.as_str());
            if !targets.insert(target) {
                return Err(UpdateError::InvalidManifest);
            }
            validate_manifest_artifact(&self.config, &manifest.version, artifact)?;
            let release_asset = assets_by_name
                .get(artifact.name.as_str())
                .copied()
                .ok_or(UpdateError::InvalidManifest)?;
            validate_release_artifact(&self.config, &release.tag_name, artifact, release_asset)?;
            if artifact.platform == self.config.target.platform
                && artifact.architecture == self.config.target.architecture
            {
                if selected.is_some() {
                    return Err(UpdateError::InvalidManifest);
                }
                selected = Some(UpdateArtifact {
                    version: manifest.version.clone(),
                    name: artifact.name.clone(),
                    platform: artifact.platform.clone(),
                    architecture: artifact.architecture.clone(),
                    download_url: artifact.download_url.clone(),
                    sha256: artifact.sha256.clone(),
                    size_bytes: artifact.size_bytes,
                });
            }
        }
        let expected_targets = HashSet::from([("macos", "arm64"), ("windows", "x86_64")]);
        if targets != expected_targets {
            return Err(UpdateError::InvalidManifest);
        }
        Ok(ValidatedRelease {
            version: manifest.version,
            release_page: manifest.release_page,
            artifact: selected.ok_or(UpdateError::UnsupportedPlatform)?,
        })
    }
}

impl<T: UpdateTransport> UpdateService for GitHubUpdateService<T> {
    fn check(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<UpdateInfo, UpdateError>> + Send + '_>,
    > {
        Box::pin(async move {
            let release = self.validated_release().await?;
            let current = Version::parse(&self.config.current_version)
                .map_err(|_| UpdateError::InvalidVersion)?;
            let available = Version::parse(&release.version)
                .map_err(|_| UpdateError::InvalidVersion)?
                > current;
            Ok(UpdateInfo {
                current_version: self.config.current_version.clone(),
                version: release.version,
                channel: self.config.channel.clone(),
                release_page: release.release_page,
                available,
                artifact: release.artifact,
            })
        })
    }

    fn download(
        &self,
        version: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<UpdateArtifact, UpdateError>> + Send + '_>,
    > {
        let version = version.to_owned();
        Box::pin(async move {
            let release = self.validated_release().await?;
            if release.version != version {
                return Err(UpdateError::InvalidVersion);
            }
            Ok(release.artifact)
        })
    }
}

pub trait ReleasePageOpener: Send + Sync {
    /// Opens a validated public Release page in the user's browser.
    ///
    /// # Errors
    /// Returns an error when the native platform rejects the URL.
    fn open(&self, release_page: &str) -> Result<(), UpdateError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeReleasePageOpener;

impl ReleasePageOpener for NativeReleasePageOpener {
    fn open(&self, release_page: &str) -> Result<(), UpdateError> {
        validate_release_page_root(release_page)?;
        #[cfg(target_os = "macos")]
        return lvos_platform::macos::open_web_url(release_page)
            .map_err(|_| UpdateError::InvalidReleaseSource);
        #[cfg(target_os = "windows")]
        return lvos_platform::windows::open_web_url(release_page)
            .map_err(|_| UpdateError::InvalidReleaseSource);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Err(UpdateError::UnsupportedPlatform)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateCheckOutcome {
    Skipped,
    UpToDate(UpdateInfo),
    Available(UpdateInfo),
}

pub struct UpdateCoordinator {
    service: Arc<dyn UpdateService>,
    opener: Arc<dyn ReleasePageOpener>,
    attempt_path: PathBuf,
}

impl fmt::Debug for UpdateCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateCoordinator")
            .field("attempt_path", &self.attempt_path)
            .finish_non_exhaustive()
    }
}

impl UpdateCoordinator {
    #[must_use]
    pub fn new(
        service: Arc<dyn UpdateService>,
        opener: Arc<dyn ReleasePageOpener>,
        application_data_root: &Path,
    ) -> Self {
        Self {
            service,
            opener,
            attempt_path: application_data_root.join("last-update-check.txt"),
        }
    }

    /// Performs an explicit check regardless of the startup interval.
    ///
    /// # Errors
    /// Returns a typed update-source or validation error.
    pub async fn manual_check(
        &self,
        now: UnixTimestamp,
    ) -> Result<UpdateCheckOutcome, UpdateError> {
        self.record_attempt(now);
        self.check().await
    }

    /// Performs at most one startup attempt per frozen low-frequency interval.
    ///
    /// # Errors
    /// Returns a typed update-source or validation error when a due check fails.
    pub async fn startup_check(
        &self,
        now: UnixTimestamp,
    ) -> Result<UpdateCheckOutcome, UpdateError> {
        if !self.startup_check_due(now) {
            return Ok(UpdateCheckOutcome::Skipped);
        }
        self.record_attempt(now);
        self.check().await
    }

    /// Opens the validated Release page only for a newer version.
    ///
    /// # Errors
    /// Returns an error when no update exists or the native browser launch fails.
    pub fn open_available(&self, info: &UpdateInfo) -> Result<(), UpdateError> {
        if !info.available {
            return Err(UpdateError::InvalidVersion);
        }
        let expected_page = format!("{GITHUB_RELEASES_URL}/tag/v{}", info.version);
        if info.release_page != expected_page {
            return Err(UpdateError::InvalidReleaseSource);
        }
        self.opener.open(&info.release_page)
    }

    #[must_use]
    pub fn startup_check_due(&self, now: UnixTimestamp) -> bool {
        let Ok(value) = std::fs::read_to_string(&self.attempt_path) else {
            return true;
        };
        let Ok(previous) = value.trim().parse::<i64>() else {
            return true;
        };
        now.as_seconds().saturating_sub(previous) >= UPDATE_CHECK_INTERVAL_SECONDS
    }

    async fn check(&self) -> Result<UpdateCheckOutcome, UpdateError> {
        let info = self.service.check().await?;
        if info.available {
            Ok(UpdateCheckOutcome::Available(info))
        } else {
            Ok(UpdateCheckOutcome::UpToDate(info))
        }
    }

    fn record_attempt(&self, now: UnixTimestamp) {
        let result = self
            .attempt_path
            .parent()
            .ok_or(())
            .and_then(|parent| std::fs::create_dir_all(parent).map_err(|_| ()))
            .and_then(|()| {
                std::fs::write(&self.attempt_path, now.as_seconds().to_string()).map_err(|_| ())
            });
        if result.is_err() {
            tracing::warn!("failed to persist the low-frequency update-check attempt");
        }
    }
}

#[derive(Debug)]
struct ValidatedRelease {
    version: String,
    release_page: String,
    artifact: UpdateArtifact,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateManifest {
    manifest_version: u32,
    product: String,
    channel: String,
    version: String,
    release_page: String,
    artifacts: Vec<ManifestArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestArtifact {
    platform: String,
    architecture: String,
    name: String,
    size_bytes: u64,
    sha256: String,
    download_url: String,
}

fn validate_channel(channel: &str) -> Result<(), UpdateError> {
    if channel == DEFAULT_UPDATE_CHANNEL {
        Ok(())
    } else {
        Err(UpdateError::UnsupportedChannel)
    }
}

fn validate_config(config: &GitHubUpdateConfig) -> Result<(), UpdateError> {
    validate_channel(&config.channel)?;
    if config.owner != GITHUB_OWNER
        || config.repository != GITHUB_REPOSITORY
        || config.release_api_url != GITHUB_LATEST_RELEASE_API
        || config.release_page_root != GITHUB_RELEASES_URL
    {
        return Err(UpdateError::InvalidReleaseSource);
    }
    Version::parse(&config.current_version).map_err(|_| UpdateError::InvalidVersion)?;
    Ok(())
}

fn validate_manifest_header(
    config: &GitHubUpdateConfig,
    release: &GitHubRelease,
    manifest: &UpdateManifest,
) -> Result<(), UpdateError> {
    Version::parse(&manifest.version).map_err(|_| UpdateError::InvalidVersion)?;
    let expected_tag = format!("v{}", manifest.version);
    let expected_page = format!("{}/tag/{expected_tag}", config.release_page_root);
    if manifest.manifest_version != UPDATE_MANIFEST_VERSION
        || manifest.product != "LVOS"
        || manifest.channel != config.channel
        || release.tag_name != expected_tag
        || release.html_url != expected_page
        || manifest.release_page != expected_page
    {
        return Err(UpdateError::InvalidManifest);
    }
    Ok(())
}

fn unique_asset<'a>(assets: &'a [GitHubAsset], name: &str) -> Result<&'a GitHubAsset, UpdateError> {
    let mut matching = assets.iter().filter(|asset| asset.name == name);
    let asset = matching.next().ok_or(UpdateError::InvalidManifest)?;
    if matching.next().is_some() {
        return Err(UpdateError::InvalidManifest);
    }
    Ok(asset)
}

fn validate_manifest_artifact(
    config: &GitHubUpdateConfig,
    version: &str,
    artifact: &ManifestArtifact,
) -> Result<(), UpdateError> {
    let expected_name = match (artifact.platform.as_str(), artifact.architecture.as_str()) {
        ("macos", "arm64") => format!("LVOS-{version}-macos-arm64.zip"),
        ("windows", "x86_64") => format!("LVOS-{version}-windows-x86_64.zip"),
        _ => return Err(UpdateError::UnsupportedPlatform),
    };
    let expected_url = format!(
        "{}/download/v{version}/{expected_name}",
        config.release_page_root
    );
    if artifact.name != expected_name
        || artifact.download_url != expected_url
        || artifact.size_bytes == 0
        || artifact.size_bytes > MAX_UPDATE_ARTIFACT_BYTES
        || !valid_sha256(&artifact.sha256)
    {
        return Err(UpdateError::InvalidManifest);
    }
    Ok(())
}

fn validate_asset_source(
    config: &GitHubUpdateConfig,
    tag: &str,
    asset: &GitHubAsset,
) -> Result<(), UpdateError> {
    let expected = format!("{}/download/{tag}/{}", config.release_page_root, asset.name);
    if asset.state != "uploaded" || asset.browser_download_url != expected {
        return Err(UpdateError::InvalidReleaseSource);
    }
    Ok(())
}

fn validate_release_artifact(
    config: &GitHubUpdateConfig,
    tag: &str,
    manifest: &ManifestArtifact,
    release: &GitHubAsset,
) -> Result<(), UpdateError> {
    validate_asset_source(config, tag, release)?;
    let expected_digest = format!("sha256:{}", manifest.sha256);
    if release.size != manifest.size_bytes
        || release.digest.as_deref() != Some(expected_digest.as_str())
        || release.browser_download_url != manifest.download_url
    {
        return Err(UpdateError::IntegrityMismatch);
    }
    Ok(())
}

fn validate_release_page_root(url: &str) -> Result<(), UpdateError> {
    let parsed = Url::parse(url).map_err(|_| UpdateError::InvalidReleaseSource)?;
    let expected_prefix = format!("{GITHUB_RELEASES_URL}/tag/v");
    let version = url
        .strip_prefix(&expected_prefix)
        .ok_or(UpdateError::InvalidReleaseSource)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || Version::parse(version).is_err()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(UpdateError::InvalidReleaseSource);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
