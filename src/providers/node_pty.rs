use anyhow::{Context, Result};
use chrono::Utc;
use futures::{StreamExt, stream};
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::cache::{CacheManager, FileMetadata, PlatformMetadata, VersionMetadata};
use crate::config::{NodePtyConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;
use crate::retry::{RetryPolicy, send_with_retry, sync_concurrency};
use crate::s3;

const PROVIDER_NAME: &str = "node-pty";
const PRIMARY_FILE: &str = "pty.node";
const GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Deserialize)]
struct ChecksumsFile {
    #[allow(dead_code)]
    version: String,
    platforms: HashMap<String, ChecksumsPlatform>,
}

#[derive(Debug, Deserialize)]
struct ChecksumsPlatform {
    files: HashMap<String, ChecksumsEntry>,
}

#[derive(Debug, Deserialize, Clone)]
struct ChecksumsEntry {
    sha256: String,
    size: u64,
    #[serde(default)]
    asset: Option<String>,
}

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
}

struct DownloadResult {
    size: u64,
    sha256: String,
}

pub struct NodePtyProvider {
    config: NodePtyConfig,
    client: Client,
    cache: Arc<CacheManager>,
    github_token: Option<String>,
    storage: StorageConfig,
}

impl NodePtyProvider {
    pub fn new(config: NodePtyConfig, cache: Arc<CacheManager>, storage: StorageConfig) -> Self {
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

    fn prebuild_path(&self, version: &str, platform: &str, filename: &str) -> PathBuf {
        self.cache
            .version_path(PROVIDER_NAME, version)
            .join("prebuilds")
            .join(platform)
            .join(filename)
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
        releases.iter().find(|release| {
            if release.draft || (!allow_prerelease && release.prerelease) {
                return false;
            }
            let has_checksums = release
                .assets
                .iter()
                .any(|asset| asset.name == "checksums.json");
            let has_prebuild = release
                .assets
                .iter()
                .any(|asset| asset.name.contains("pty.node"));
            has_checksums && has_prebuild
        })
    }

    async fn download_asset_bytes(&self, url: &str) -> Result<Vec<u8>> {
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
        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
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
        let result = match self.storage.mode {
            StorageMode::Oss => {
                let upload = oss::upload_stream(
                    &self.storage.oss,
                    object_key,
                    "application/octet-stream",
                    total_size,
                    response.bytes_stream(),
                )
                .await?;
                DownloadResult {
                    size: upload.size,
                    sha256: upload.sha256,
                }
            }
            StorageMode::S3 => {
                let upload = s3::upload_stream(
                    &self.storage.s3,
                    object_key,
                    "application/octet-stream",
                    total_size,
                    response.bytes_stream(),
                )
                .await?;
                DownloadResult {
                    size: upload.size,
                    sha256: upload.sha256,
                }
            }
            StorageMode::Local => {
                return Err(MirrorError::Provider(
                    "download_asset_to_remote called in local mode".to_string(),
                )
                .into());
            }
        };

        Ok(result)
    }

    async fn try_use_existing_s3_object(
        &self,
        object_key: &str,
        expected_sha256: &str,
        expected_size: u64,
    ) -> Result<Option<DownloadResult>> {
        let info = s3::head_object_info(&self.storage.s3, object_key).await?;
        let Some(info) = info else {
            return Ok(None);
        };
        let Some(actual_sha256) = info.sha256 else {
            return Ok(None);
        };

        if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
            return Ok(None);
        }

        if expected_size > 0 {
            if let Some(size) = info.size {
                if size != expected_size {
                    warn!(
                        "S3 object size mismatch for {}: expected {}, got {}",
                        object_key, expected_size, size
                    );
                    return Ok(None);
                }
            }
        }

        Ok(Some(DownloadResult {
            size: info.size.unwrap_or(expected_size),
            sha256: expected_sha256.to_string(),
        }))
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
        let checksums_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.json")
            .ok_or_else(|| {
                MirrorError::Provider(format!("checksums.json not found in release {}", version))
            })?;

        let checksums_bytes = self
            .download_asset_bytes(&checksums_asset.browser_download_url)
            .await?;
        let checksums: ChecksumsFile = serde_json::from_slice(&checksums_bytes)?;

        #[derive(Clone)]
        struct FileTask {
            platform: String,
            filename: String,
            asset_url: String,
            sha256: String,
            size: u64,
        }

        let mut tasks = Vec::new();
        let mut failures = Vec::new();

        for platform in &self.config.platforms {
            let platform_meta = checksums
                .platforms
                .get(platform)
                .ok_or_else(|| MirrorError::PlatformNotFound(platform.to_string()))?;

            if platform_meta.files.is_empty() {
                return Err(MirrorError::Provider(format!(
                    "No files listed for platform {}",
                    platform
                ))
                .into());
            }

            for (filename, entry) in &platform_meta.files {
                let asset_name = entry.asset.clone().unwrap_or_else(|| filename.clone());
                let asset = release
                    .assets
                    .iter()
                    .find(|a| a.name == asset_name)
                    .or_else(|| {
                        if entry.asset.is_none() {
                            let fallback = format!("{}--{}", platform, filename);
                            release.assets.iter().find(|a| a.name == fallback)
                        } else {
                            None
                        }
                    });

                let Some(asset) = asset else {
                    failures.push(format!("Asset not found for {}/{}", platform, filename));
                    continue;
                };

                tasks.push(FileTask {
                    platform: platform.clone(),
                    filename: filename.clone(),
                    asset_url: asset.browser_download_url.clone(),
                    sha256: entry.sha256.clone(),
                    size: entry.size,
                });
            }
        }

        let concurrency = sync_concurrency();
        let version_label = version.to_string();
        let provider = self;
        let storage_mode = provider.storage.mode.clone();
        let remote_mode = matches!(storage_mode, StorageMode::Oss | StorageMode::S3);
        let mut stream = stream::iter(tasks)
            .map(|task| {
                let version = version_label.clone();
                let storage_mode = storage_mode.clone();
                async move {
                    let key = provider
                        .cache
                        .build_object_key(
                            PROVIDER_NAME,
                            &[
                                "versions",
                                &version,
                                "prebuilds",
                                &task.platform,
                                &task.filename,
                            ],
                        )
                        .ok_or_else(|| "Invalid storage key".to_string())?;

                    if remote_mode {
                        let existing = if matches!(storage_mode, StorageMode::S3) {
                            provider
                                .try_use_existing_s3_object(&key, &task.sha256, task.size)
                                .await
                                .map_err(|e| e.to_string())?
                        } else {
                            None
                        };

                        let result = if let Some(result) = existing {
                            info!(
                                "S3 object already present for {}/{} (sha256 match), skip download",
                                version, task.filename
                            );
                            result
                        } else {
                            match provider
                                .download_asset_to_remote(&task.asset_url, &key)
                                .await
                            {
                                Ok(result) => result,
                                Err(e) => {
                                    warn!(
                                        "Failed to download {}/{}: {:?}",
                                        version, task.filename, e
                                    );
                                    return Err(format!("Download failed for {}", task.filename));
                                }
                            }
                        };

                        if result.sha256 != task.sha256 {
                            warn!(
                                "Checksum verification failed for {}/{}: expected {}, got {}",
                                version, task.filename, task.sha256, result.sha256
                            );
                            match storage_mode {
                                StorageMode::Oss => {
                                    let _ = oss::delete_object(&provider.storage.oss, &key).await;
                                }
                                StorageMode::S3 => {
                                    let _ = s3::delete_object(&provider.storage.s3, &key).await;
                                }
                                StorageMode::Local => {}
                            }
                            return Err(format!("Checksum mismatch for {}", task.filename));
                        }
                    } else {
                        let path = provider.prebuild_path(&version, &task.platform, &task.filename);
                        if let Some(parent) = path.parent() {
                            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                                format!("Failed to create directory {}: {}", parent.display(), e)
                            })?;
                        }

                        let result = match provider
                            .download_asset_to_path(&task.asset_url, &path)
                            .await
                        {
                            Ok(result) => result,
                            Err(e) => {
                                warn!("Failed to download {}/{}: {:?}", version, task.filename, e);
                                let _ = tokio::fs::remove_file(&path).await;
                                return Err(format!("Download failed for {}", task.filename));
                            }
                        };

                        if result.sha256 != task.sha256 || result.size != task.size {
                            warn!(
                                "Checksum verification failed for {}/{}: expected {}, got {}",
                                version, task.filename, task.sha256, result.sha256
                            );
                            let _ = tokio::fs::remove_file(&path).await;
                            return Err(format!("Checksum mismatch for {}", task.filename));
                        }
                    }

                    Ok(())
                }
            })
            .buffer_unordered(concurrency);

        while let Some(result) = stream.next().await {
            if let Err(err) = result {
                failures.push(err);
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

        let mut platforms_metadata = HashMap::new();
        for platform in &self.config.platforms {
            let platform_meta = checksums
                .platforms
                .get(platform)
                .ok_or_else(|| MirrorError::PlatformNotFound(platform.to_string()))?;

            let mut files_meta = HashMap::new();
            for (filename, entry) in &platform_meta.files {
                files_meta.insert(
                    filename.clone(),
                    FileMetadata {
                        sha256: entry.sha256.clone(),
                        size: entry.size,
                    },
                );
            }

            let primary = if let Some(entry) = platform_meta.files.get(PRIMARY_FILE) {
                (PRIMARY_FILE.to_string(), entry.clone())
            } else {
                let (name, entry) = platform_meta.files.iter().next().unwrap();
                (name.clone(), entry.clone())
            };

            platforms_metadata.insert(
                platform.clone(),
                PlatformMetadata {
                    sha256: primary.1.sha256,
                    size: primary.1.size,
                    filename: primary.0,
                    files: files_meta,
                },
            );
        }

        self.persist_checksums(version, &checksums_bytes).await?;

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

    async fn persist_checksums(&self, version: &str, bytes: &[u8]) -> Result<()> {
        let key = self
            .cache
            .build_object_key(PROVIDER_NAME, &["versions", version, "checksums.json"])
            .ok_or_else(|| MirrorError::VersionNotFound(version.to_string()))?;

        match self.storage.mode {
            StorageMode::Local => {
                let path = self
                    .cache
                    .get_file_path(PROVIDER_NAME, &["versions", version, "checksums.json"])
                    .ok_or_else(|| MirrorError::VersionNotFound(version.to_string()))?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, bytes).await?;
            }
            StorageMode::Oss => {
                oss::put_bytes(&self.storage.oss, &key, "application/json", bytes.to_vec()).await?;
            }
            StorageMode::S3 => {
                s3::put_bytes(&self.storage.s3, &key, "application/json", bytes.to_vec()).await?;
            }
        }

        Ok(())
    }

    async fn is_version_complete(&self, version: &str) -> bool {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.node_pty;
        let Some(version_meta) = provider.versions.get(version) else {
            return false;
        };

        for platform in &self.config.platforms {
            let Some(platform_meta) = version_meta.platforms.get(platform) else {
                return false;
            };

            if platform_meta.files.is_empty() {
                if matches!(self.storage.mode, StorageMode::Local) {
                    let path = self.prebuild_path(version, platform, &platform_meta.filename);
                    if !path.exists() {
                        return false;
                    }
                }
                continue;
            }

            for filename in platform_meta.files.keys() {
                if matches!(self.storage.mode, StorageMode::Local) {
                    let path = self.prebuild_path(version, platform, filename);
                    if !path.exists() {
                        return false;
                    }
                }
            }
        }

        true
    }

    async fn delete_oss_versions(&self, versions: &[VersionMetadata]) {
        for version_meta in versions {
            let version = &version_meta.version;
            let mut keys = Vec::new();
            if let Some(key) = self
                .cache
                .build_object_key(PROVIDER_NAME, &["versions", version, "checksums.json"])
            {
                keys.push(key);
            }
            for (platform, meta) in &version_meta.platforms {
                if meta.files.is_empty() {
                    if let Some(key) = self.cache.build_object_key(
                        PROVIDER_NAME,
                        &["versions", version, "prebuilds", platform, &meta.filename],
                    ) {
                        keys.push(key);
                    }
                    continue;
                }

                for filename in meta.files.keys() {
                    if let Some(key) = self.cache.build_object_key(
                        PROVIDER_NAME,
                        &["versions", version, "prebuilds", platform, filename],
                    ) {
                        keys.push(key);
                    }
                }
            }

            for key in keys {
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
        self.cache.read_tag(PROVIDER_NAME, tag).await
    }

    pub async fn get_info(&self) -> serde_json::Value {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.node_pty;

        let display_version = provider
            .tags
            .get("latest")
            .or_else(|| provider.tags.get("stable"));

        let mut platforms = serde_json::Map::new();

        if let Some(version) = display_version {
            if let Some(version_meta) = provider.versions.get(version) {
                for (platform, meta) in &version_meta.platforms {
                    let mut files = serde_json::Map::new();

                    if meta.files.is_empty() {
                        files.insert(
                            meta.filename.clone(),
                            serde_json::json!({
                                "url": format!("/node-pty/{}/prebuilds/{}/{}", version, platform, meta.filename),
                                "sha256": meta.sha256,
                                "size": meta.size
                            }),
                        );
                    } else {
                        for (filename, file_meta) in &meta.files {
                            files.insert(
                                filename.clone(),
                                serde_json::json!({
                                    "url": format!("/node-pty/{}/prebuilds/{}/{}", version, platform, filename),
                                    "sha256": file_meta.sha256,
                                    "size": file_meta.size
                                }),
                            );
                        }
                    }

                    platforms.insert(
                        platform.clone(),
                        serde_json::json!({
                            "version": version,
                            "url": format!("/node-pty/{}/prebuilds/{}/{}", version, platform, meta.filename),
                            "sha256": meta.sha256,
                            "size": meta.size,
                            "files": files
                        }),
                    );
                }
            }
        }

        serde_json::json!({
            "tags": provider.tags,
            "platforms": platforms,
            "updated_at": provider.updated_at
        })
    }
}
