use anyhow::{Context, Result};
use chrono::Utc;
use futures::{StreamExt, stream};
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::cache::{CacheManager, PlatformMetadata, VersionMetadata};
use crate::config::{CodexConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;
use crate::retry::{RetryPolicy, send_with_retry, sync_concurrency};
use crate::s3;

const PROVIDER_NAME: &str = "codex";
const GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

struct DownloadResult {
    size: u64,
    sha256: String,
}

pub struct CodexProvider {
    config: CodexConfig,
    client: Client,
    cache: Arc<CacheManager>,
    github_token: Option<String>,
    storage: StorageConfig,
}

impl CodexProvider {
    pub fn new(config: CodexConfig, cache: Arc<CacheManager>, storage: StorageConfig) -> Self {
        let client = Client::builder()
            .user_agent("duckcoding-cli-mirror/0.1.0")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        let github_token = std::env::var("GITHUB_TOKEN").ok();

        Self {
            config,
            client,
            cache,
            github_token,
            storage,
        }
    }

    fn api_request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github+json");
        if let Some(token) = &self.github_token {
            req = req.bearer_auth(token);
        }
        req
    }

    async fn fetch_releases(&self) -> Result<Vec<Release>> {
        let url = format!("{}/repos/{}/releases", GITHUB_API_BASE, self.config.repo);
        let response = send_with_retry(|| self.api_request(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch releases from {}", url))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound("releases".to_string()).into());
        }
        if !status.is_success() {
            return Err(
                MirrorError::Provider(format!("Failed to fetch releases: {}", status)).into(),
            );
        }

        let releases = response.json::<Vec<Release>>().await?;
        Ok(releases)
    }

    async fn fetch_release_by_tag(&self, tag: &str) -> Result<Release> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            GITHUB_API_BASE, self.config.repo, tag
        );
        let response = send_with_retry(|| self.api_request(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch release tag {}", tag))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(tag.to_string()).into());
        }
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to fetch release {}: {}",
                tag, status
            ))
            .into());
        }

        Ok(response.json::<Release>().await?)
    }

    fn select_release<'a>(&self, releases: &'a [Release], tag: &str) -> Option<&'a Release> {
        let allow_prerelease = tag == "latest" && self.config.include_prerelease;
        releases
            .iter()
            .find(|release| !release.draft && (allow_prerelease || !release.prerelease))
    }

    fn platform_target(platform: &str) -> Option<&'static str> {
        match platform {
            "darwin-x64" => Some("x86_64-apple-darwin"),
            "darwin-arm64" => Some("aarch64-apple-darwin"),
            "linux-x64" => Some("x86_64-unknown-linux-gnu"),
            "linux-arm64" => Some("aarch64-unknown-linux-gnu"),
            "linux-x64-musl" => Some("x86_64-unknown-linux-musl"),
            "linux-arm64-musl" => Some("aarch64-unknown-linux-musl"),
            "win32-x64" => Some("x86_64-pc-windows-msvc"),
            "win32-arm64" => Some("aarch64-pc-windows-msvc"),
            _ => None,
        }
    }

    fn asset_name_for_platform(platform: &str) -> Option<String> {
        let target = Self::platform_target(platform)?;
        if platform.starts_with("win32") {
            Some(format!("codex-{}.exe", target))
        } else {
            Some(format!("codex-{}.tar.gz", target))
        }
    }

    fn asset_digest_sha256(asset: &Asset) -> Option<&str> {
        asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
    }

    async fn download_asset_to_path(&self, url: &str, path: &Path) -> Result<DownloadResult> {
        let url = url.to_string();
        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download asset {}: {}",
                url, status
            ))
            .into());
        }

        let mut file = tokio::fs::File::create(path).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }

        file.flush().await?;

        Ok(DownloadResult {
            size,
            sha256: hex::encode(hasher.finalize()),
        })
    }

    async fn download_asset_to_remote(
        &self,
        url: &str,
        object_key: &str,
    ) -> Result<DownloadResult> {
        let url = url.to_string();
        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download asset {}: {}",
                url, status
            ))
            .into());
        }

        let total_size = response.content_length();
        let (size, sha256) = if matches!(self.storage.mode, StorageMode::Oss) {
            let upload = oss::upload_stream(
                &self.storage.oss,
                object_key,
                "application/octet-stream",
                total_size,
                response.bytes_stream(),
            )
            .await?;
            (upload.size, upload.sha256)
        } else if matches!(self.storage.mode, StorageMode::S3) {
            let upload = s3::upload_stream(
                &self.storage.s3,
                object_key,
                "application/octet-stream",
                total_size,
                response.bytes_stream(),
            )
            .await?;
            (upload.size, upload.sha256)
        } else {
            return Err(MirrorError::Provider(
                "download_asset_to_remote called in local mode".to_string(),
            )
            .into());
        };

        Ok(DownloadResult { size, sha256 })
    }

    async fn try_use_existing_s3_object(
        &self,
        object_key: &str,
        expected_sha256: Option<&str>,
    ) -> Result<Option<DownloadResult>> {
        let info = s3::head_object_info(&self.storage.s3, object_key).await?;
        let Some(info) = info else {
            return Ok(None);
        };
        let Some(actual_sha256) = info.sha256 else {
            return Ok(None);
        };

        if let Some(expected) = expected_sha256 {
            if !actual_sha256.eq_ignore_ascii_case(expected) {
                return Ok(None);
            }
        }

        Ok(Some(DownloadResult {
            size: info.size.unwrap_or(0),
            sha256: actual_sha256,
        }))
    }

    async fn verify_file_checksum(path: &Path, expected: &str) -> Result<u64> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buffer = [0u8; 8192];

        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            size += read as u64;
            hasher.update(&buffer[..read]);
        }

        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            return Err(MirrorError::ChecksumMismatch {
                expected: expected.to_string(),
                actual,
            }
            .into());
        }

        Ok(size)
    }

    /// Get version for a tag from upstream
    pub async fn fetch_upstream_tag(&self, tag: &str) -> Result<String> {
        let releases = self.fetch_releases().await?;
        let release = self
            .select_release(&releases, tag)
            .ok_or_else(|| MirrorError::VersionNotFound(tag.to_string()))?;
        Ok(release.tag_name.clone())
    }

    pub async fn sync_tag(&self, tag: &str) -> Result<Option<String>> {
        if !self.config.enabled {
            return Ok(None);
        }

        let cached_version = self.cache.read_tag(PROVIDER_NAME, tag).await;
        let upstream_version = self.fetch_upstream_tag(tag).await?;

        if cached_version.as_ref() == Some(&upstream_version) {
            info!("Tag {} is up to date: {}", tag, upstream_version);
            return Ok(None);
        }

        info!(
            "New version available for tag {}: {} -> {}",
            tag,
            cached_version.as_deref().unwrap_or("none"),
            upstream_version
        );

        self.sync_version(&upstream_version).await?;

        self.cache
            .write_tag(PROVIDER_NAME, tag, &upstream_version)
            .await?;

        self.cache
            .update_provider_metadata(PROVIDER_NAME, |m| {
                m.tags.insert(tag.to_string(), upstream_version.clone());
                m.updated_at = Some(Utc::now());
            })
            .await?;

        let deleted = self.cache.cleanup_old_versions(PROVIDER_NAME).await?;
        if !deleted.is_empty() {
            info!("Cleaned up {} old versions", deleted.len());
            if matches!(self.storage.mode, StorageMode::Oss | StorageMode::S3) {
                self.delete_oss_versions(&deleted).await;
            }
        }

        Ok(Some(upstream_version))
    }

    pub async fn sync_version(&self, version: &str) -> Result<()> {
        if self.is_version_complete(version).await {
            info!("Version {} already cached", version);
            return Ok(());
        }

        info!("Syncing version: {}", version);

        let release = self.fetch_release_by_tag(version).await?;
        let metadata = self.cache.get_metadata().await;
        let existing_version = metadata.codex.versions.get(version);

        #[derive(Clone)]
        struct PlatformTask {
            platform: String,
            asset_name: String,
            asset_url: String,
            asset_digest: Option<String>,
            expected_existing: Option<String>,
        }

        let mut tasks = Vec::new();
        let mut failures = Vec::new();

        for platform in &self.config.platforms {
            let asset_name = match Self::asset_name_for_platform(platform) {
                Some(name) => name,
                None => {
                    failures.push(format!("Unsupported platform {}", platform));
                    continue;
                }
            };

            let asset = match release.assets.iter().find(|a| a.name == asset_name) {
                Some(asset) => asset,
                None => {
                    failures.push(format!("Asset not found for {}", platform));
                    continue;
                }
            };

            let asset_digest = Self::asset_digest_sha256(asset).map(str::to_string);
            let expected_existing = existing_version
                .and_then(|version_meta| version_meta.platforms.get(platform))
                .map(|meta| meta.sha256.clone())
                .or_else(|| asset_digest.clone());

            tasks.push(PlatformTask {
                platform: platform.clone(),
                asset_name: asset.name.clone(),
                asset_url: asset.browser_download_url.clone(),
                asset_digest,
                expected_existing,
            });
        }

        let concurrency = sync_concurrency();
        let version_label = version.to_string();
        let provider = self;
        let mut platforms_metadata = HashMap::new();

        let storage_mode = provider.storage.mode.clone();
        let remote_mode = matches!(storage_mode, StorageMode::Oss | StorageMode::S3);
        let mut stream = stream::iter(tasks)
            .map(|task| {
                let version = version_label.clone();
                let storage_mode = storage_mode.clone();
                async move {
                    if remote_mode {
                        let key = provider
                            .cache
                            .build_object_key(
                                PROVIDER_NAME,
                                &["versions", &version, &task.platform, &task.asset_name],
                            )
                            .ok_or_else(|| "Invalid storage key".to_string())?;

                        let expected_sha = task
                            .asset_digest
                            .as_deref()
                            .or(task.expected_existing.as_deref());
                        let existing = if matches!(storage_mode, StorageMode::S3) {
                            provider
                                .try_use_existing_s3_object(&key, expected_sha)
                                .await
                                .map_err(|e| e.to_string())?
                        } else {
                            None
                        };

                        let result = if let Some(result) = existing {
                            info!(
                                "S3 object already present for {}/{} (sha256 match), skip download",
                                version, task.platform
                            );
                            result
                        } else {
                            match provider.download_asset_to_remote(&task.asset_url, &key).await {
                                Ok(result) => result,
                                Err(e) => {
                                    warn!(
                                        "Failed to download {}/{}: {:?}",
                                        version, task.platform, e
                                    );
                                    return Err(format!("Download failed for {}", task.platform));
                                }
                            }
                        };

                        if let Some(expected) = task.asset_digest.as_deref() {
                            if result.sha256 != expected {
                                warn!(
                                    "Checksum verification failed for {}/{}: expected {}, got {}",
                                    version, task.platform, expected, result.sha256
                                );
                                match storage_mode {
                                    StorageMode::Oss => {
                                        let _ =
                                            oss::delete_object(&provider.storage.oss, &key).await;
                                    }
                                    StorageMode::S3 => {
                                        let _ =
                                            s3::delete_object(&provider.storage.s3, &key).await;
                                    }
                                    StorageMode::Local => {}
                                }
                                return Err(format!("Checksum mismatch for {}", task.platform));
                            }
                        }

                        info!(
                            "Uploaded asset: {}/{}/{} ({} bytes)",
                            version, task.platform, task.asset_name, result.size
                        );
                        Ok((
                            task.platform,
                            PlatformMetadata {
                                sha256: result.sha256,
                                size: result.size,
                                filename: task.asset_name,
                                files: HashMap::new(),
                            },
                        ))
                    } else {
                        let path = provider.cache.binary_path(
                            PROVIDER_NAME,
                            &version,
                            &task.platform,
                            &task.asset_name,
                        );

                        if path.exists() {
                            if let Some(expected) = &task.expected_existing {
                                match Self::verify_file_checksum(&path, expected).await {
                                    Ok(size) => {
                                        info!(
                                            "Asset verified: {}/{}/{} ({} bytes)",
                                            version, task.platform, task.asset_name, size
                                        );
                                        return Ok((
                                            task.platform,
                                            PlatformMetadata {
                                                sha256: expected.clone(),
                                                size,
                                                filename: task.asset_name,
                                                files: HashMap::new(),
                                            },
                                        ));
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Existing asset checksum failed for {}/{}: {:?}",
                                            version, task.platform, e
                                        );
                                        let _ = tokio::fs::remove_file(&path).await;
                                    }
                                }
                            } else {
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                        }

                        if let Some(parent) = path.parent() {
                            tokio::fs::create_dir_all(parent)
                                .await
                                .map_err(|e| e.to_string())?;
                        }

                        match provider.download_asset_to_path(&task.asset_url, &path).await {
                            Ok(result) => {
                                if let Some(expected) = task.asset_digest.as_deref() {
                                    if result.sha256 != expected {
                                        warn!(
                                            "Checksum verification failed for {}/{}: expected {}, got {}",
                                            version, task.platform, expected, result.sha256
                                        );
                                        let _ = tokio::fs::remove_file(&path).await;
                                        return Err(format!("Checksum mismatch for {}", task.platform));
                                    }
                                }

                                info!(
                                    "Saved asset: {}/{}/{} ({} bytes)",
                                    version, task.platform, task.asset_name, result.size
                                );
                                Ok((
                                    task.platform,
                                    PlatformMetadata {
                                        sha256: result.sha256,
                                        size: result.size,
                                        filename: task.asset_name,
                                        files: HashMap::new(),
                                    },
                                ))
                            }
                            Err(e) => {
                                warn!("Failed to download {}/{}: {:?}", version, task.platform, e);
                                let _ = tokio::fs::remove_file(&path).await;
                                Err(format!("Download failed for {}", task.platform))
                            }
                        }
                    }
                }
            })
            .buffer_unordered(concurrency);

        while let Some(result) = stream.next().await {
            match result {
                Ok((platform, meta)) => {
                    platforms_metadata.insert(platform, meta);
                }
                Err(err) => failures.push(err),
            }
        }

        if !failures.is_empty() {
            return Err(MirrorError::Provider(format!(
                "Sync incomplete for {}: {}",
                version,
                failures.join(", ")
            ))
            .into());
        }

        self.cache
            .update_provider_metadata(PROVIDER_NAME, |m| {
                m.versions.insert(
                    version.to_string(),
                    VersionMetadata {
                        version: version.to_string(),
                        downloaded_at: Utc::now(),
                        platforms: platforms_metadata,
                    },
                );
            })
            .await?;

        Ok(())
    }

    async fn is_version_complete(&self, version: &str) -> bool {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.codex;
        let Some(version_meta) = provider.versions.get(version) else {
            return false;
        };

        for platform in &self.config.platforms {
            let Some(platform_meta) = version_meta.platforms.get(platform) else {
                return false;
            };

            if matches!(self.storage.mode, StorageMode::Local)
                && !self
                    .cache
                    .binary_exists(PROVIDER_NAME, version, platform, &platform_meta.filename)
                    .await
            {
                return false;
            }
        }

        true
    }

    async fn delete_oss_versions(&self, versions: &[VersionMetadata]) {
        for version_meta in versions {
            let version = &version_meta.version;
            for (platform, meta) in &version_meta.platforms {
                if let Some(key) = self.cache.build_object_key(
                    PROVIDER_NAME,
                    &["versions", version, platform, &meta.filename],
                ) {
                    match self.storage.mode {
                        StorageMode::Oss => {
                            if let Err(e) = oss::delete_object(&self.storage.oss, &key).await {
                                warn!("Failed to delete OSS object {}: {:?}", key, e);
                            }
                        }
                        StorageMode::S3 => {
                            if let Err(e) = s3::delete_object(&self.storage.s3, &key).await {
                                warn!("Failed to delete S3 object {}: {:?}", key, e);
                            }
                        }
                        StorageMode::Local => {}
                    }
                }
            }
        }
    }

    pub async fn sync_all(&self) -> Result<Vec<String>> {
        let mut updated = Vec::new();

        for tag in &self.config.tags {
            match self.sync_tag(tag).await {
                Ok(Some(version)) => updated.push(format!("{}: {}", tag, version)),
                Ok(None) => {}
                Err(e) => warn!("Failed to sync tag {}: {:?}", tag, e),
            }
        }

        Ok(updated)
    }

    pub async fn get_tag_version(&self, tag: &str) -> Option<String> {
        if tag == "stable" {
            if let Some(version) = self.cache.read_tag(PROVIDER_NAME, "stable").await {
                return Some(version);
            }
            return self.cache.read_tag(PROVIDER_NAME, "latest").await;
        }
        self.cache.read_tag(PROVIDER_NAME, tag).await
    }

    pub async fn get_info(&self) -> serde_json::Value {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.codex;

        let mut tags = provider.tags.clone();
        if !tags.contains_key("stable") {
            if let Some(latest) = tags.get("latest").cloned() {
                tags.insert("stable".to_string(), latest);
            }
        }

        let display_version = tags.get("latest").or_else(|| tags.get("stable"));

        let mut platforms = serde_json::Map::new();

        if let Some(version) = display_version {
            if let Some(version_meta) = provider.versions.get(version) {
                for (platform, meta) in &version_meta.platforms {
                    platforms.insert(
                        platform.clone(),
                        serde_json::json!({
                            "version": version,
                            "url": format!("/codex/{}/{}/{}", version, platform, meta.filename),
                            "sha256": meta.sha256,
                            "size": meta.size
                        }),
                    );
                }
            }
        }

        serde_json::json!({
            "tags": tags,
            "platforms": platforms,
            "updated_at": provider.updated_at
        })
    }
}
