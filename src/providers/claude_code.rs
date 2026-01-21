use anyhow::{Context, Result};
use chrono::Utc;
use futures::{StreamExt, stream};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::cache::{CacheManager, PlatformMetadata, VersionMetadata};
use crate::config::{ClaudeCodeConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;
use crate::retry::{RetryPolicy, send_with_retry, sync_concurrency};
use crate::s3;

const PROVIDER_NAME: &str = "claude-code";

/// Manifest structure from upstream
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub build_date: Option<String>,
    pub platforms: HashMap<String, ManifestPlatform>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPlatform {
    pub checksum: String,
    #[serde(default)]
    pub size: u64,
}

struct DownloadResult {
    size: u64,
    sha256: String,
}

/// Claude Code provider
pub struct ClaudeCodeProvider {
    config: ClaudeCodeConfig,
    client: Client,
    cache: Arc<CacheManager>,
    storage: StorageConfig,
}

impl ClaudeCodeProvider {
    pub fn new(config: ClaudeCodeConfig, cache: Arc<CacheManager>, storage: StorageConfig) -> Self {
        let client = Client::builder()
            .user_agent("duckcoding-cli-mirror/0.1.0")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            cache,
            storage,
        }
    }

    /// Get version for a tag from upstream
    pub async fn fetch_upstream_tag(&self, tag: &str) -> Result<String> {
        let url = format!("{}/{}", self.config.upstream_url, tag);
        info!("Fetching tag from upstream: {}", url);

        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch tag: {}", tag))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(tag.to_string()).into());
        }
        if !status.is_success() {
            return Err(
                MirrorError::Provider(format!("Failed to fetch tag {}: {}", tag, status)).into(),
            );
        }

        let version = response.text().await?.trim().to_string();

        Ok(version)
    }

    /// Get manifest for a version from upstream
    pub async fn fetch_upstream_manifest(&self, version: &str) -> Result<Manifest> {
        let url = format!("{}/{}/manifest.json", self.config.upstream_url, version);
        info!("Fetching manifest from upstream: {}", url);

        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch manifest for version: {}", version))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(version.to_string()).into());
        }
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to fetch manifest {}: {}",
                version, status
            ))
            .into());
        }

        let manifest: Manifest = response.json().await?;
        Ok(manifest)
    }

    /// Download a binary for a specific platform and write to disk.
    async fn download_binary_to_path(
        &self,
        version: &str,
        platform: &str,
        filename: &str,
        path: &Path,
    ) -> Result<DownloadResult> {
        let url = format!(
            "{}/{}/{}/{}",
            self.config.upstream_url, version, platform, filename
        );
        info!("Downloading binary: {}", url);

        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download binary for {}/{}", version, platform))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::PlatformNotFound(platform.to_string()).into());
        }
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download binary for {}/{}: {}",
                version, platform, status
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

        let hash = hex::encode(hasher.finalize());
        info!(
            "Downloaded {}/{}/{} ({} bytes, sha256: {})",
            version, platform, filename, size, hash
        );

        Ok(DownloadResult { size, sha256: hash })
    }

    async fn download_binary_to_remote(
        &self,
        version: &str,
        platform: &str,
        filename: &str,
        object_key: &str,
    ) -> Result<DownloadResult> {
        let url = format!(
            "{}/{}/{}/{}",
            self.config.upstream_url, version, platform, filename
        );
        info!("Downloading binary: {}", url);

        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download binary for {}/{}", version, platform))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::PlatformNotFound(platform.to_string()).into());
        }
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download binary for {}/{}: {}",
                version, platform, status
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
                "download_binary_to_remote called in local mode".to_string(),
            )
            .into());
        };

        Ok(DownloadResult { size, sha256 })
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

    /// Sync a specific tag (download if new version available)
    pub async fn sync_tag(&self, tag: &str) -> Result<Option<String>> {
        if !self.config.enabled {
            return Ok(None);
        }

        // Get current cached version
        let cached_version = self.cache.read_tag(PROVIDER_NAME, tag).await;

        // Get upstream version
        let upstream_version = self.fetch_upstream_tag(tag).await?;

        // Check if we need to download
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

        // Download the new version
        self.sync_version(&upstream_version).await?;

        // Update the tag
        self.cache
            .write_tag(PROVIDER_NAME, tag, &upstream_version)
            .await?;

        // Update metadata
        self.cache
            .update_provider_metadata(PROVIDER_NAME, |m| {
                m.tags.insert(tag.to_string(), upstream_version.clone());
                m.updated_at = Some(Utc::now());
            })
            .await?;

        // Cleanup old versions
        let deleted = self.cache.cleanup_old_versions(PROVIDER_NAME).await?;
        if !deleted.is_empty() {
            info!("Cleaned up {} old versions", deleted.len());
            if matches!(self.storage.mode, StorageMode::Oss | StorageMode::S3) {
                self.delete_oss_versions(&deleted).await;
            }
        }

        Ok(Some(upstream_version))
    }

    /// Sync a specific version (download all platforms)
    pub async fn sync_version(&self, version: &str) -> Result<()> {
        if self.is_version_complete(version).await {
            info!("Version {} already cached", version);
            return Ok(());
        }

        info!("Syncing version: {}", version);

        // Fetch manifest
        let manifest = self.fetch_upstream_manifest(version).await?;

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        match self.storage.mode {
            StorageMode::Local => {
                let manifest_path = self
                    .cache
                    .version_path(PROVIDER_NAME, version)
                    .join("manifest.json");
                if let Some(parent) = manifest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&manifest_path, &manifest_json).await?;
            }
            StorageMode::Oss => {
                let key = self
                    .cache
                    .build_object_key(PROVIDER_NAME, &["versions", version, "manifest.json"])
                    .ok_or_else(|| MirrorError::Provider("Invalid manifest key".to_string()))?;
                oss::put_bytes(
                    &self.storage.oss,
                    &key,
                    "application/json",
                    manifest_json.into_bytes(),
                )
                .await?;
            }
            StorageMode::S3 => {
                let key = self
                    .cache
                    .build_object_key(PROVIDER_NAME, &["versions", version, "manifest.json"])
                    .ok_or_else(|| MirrorError::Provider("Invalid manifest key".to_string()))?;
                s3::put_bytes(
                    &self.storage.s3,
                    &key,
                    "application/json",
                    manifest_json.into_bytes(),
                )
                .await?;
            }
        }

        #[derive(Clone)]
        struct PlatformTask {
            platform: String,
            checksum: String,
            filename: String,
            size: u64,
        }

        let mut tasks = Vec::new();
        let mut failures = Vec::new();

        for platform in &self.config.platforms {
            let platform_manifest = match manifest.platforms.get(platform) {
                Some(platform_manifest) => platform_manifest,
                None => {
                    failures.push(format!("Platform {} not found in manifest", platform));
                    continue;
                }
            };

            let filename = if platform.starts_with("win32") {
                "claude.exe"
            } else {
                "claude"
            };

            tasks.push(PlatformTask {
                platform: platform.clone(),
                checksum: platform_manifest.checksum.clone(),
                filename: filename.to_string(),
                size: platform_manifest.size,
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
                                &["versions", &version, &task.platform, &task.filename],
                            )
                            .ok_or_else(|| "Invalid storage key".to_string())?;

                        let existing = if matches!(storage_mode, StorageMode::S3) {
                            provider
                                .try_use_existing_s3_object(&key, &task.checksum, task.size)
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
                            match provider
                                .download_binary_to_remote(
                                    &version,
                                    &task.platform,
                                    &task.filename,
                                    &key,
                                )
                                .await
                            {
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

                        if result.sha256 != task.checksum {
                            warn!(
                                "Checksum verification failed for {}/{}: expected {}, got {}",
                                version, task.platform, task.checksum, result.sha256
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
                            return Err(format!("Checksum mismatch for {}", task.platform));
                        }

                        info!(
                            "Uploaded binary: {}/{}/{} ({} bytes)",
                            version, task.platform, task.filename, result.size
                        );
                        Ok((
                            task.platform,
                            PlatformMetadata {
                                sha256: task.checksum,
                                size: result.size,
                                filename: task.filename,
                                files: HashMap::new(),
                            },
                        ))
                    } else {
                        let path = provider.cache.binary_path(
                            PROVIDER_NAME,
                            &version,
                            &task.platform,
                            &task.filename,
                        );

                        if path.exists() {
                            match Self::verify_file_checksum(&path, &task.checksum).await {
                                Ok(size) => {
                                    info!(
                                        "Binary verified: {}/{}/{} ({} bytes)",
                                        version, task.platform, task.filename, size
                                    );
                                    return Ok((
                                        task.platform,
                                        PlatformMetadata {
                                            sha256: task.checksum,
                                            size,
                                            filename: task.filename,
                                            files: HashMap::new(),
                                        },
                                    ));
                                }
                                Err(e) => {
                                    warn!(
                                        "Existing binary checksum failed for {}/{}: {:?}",
                                        version, task.platform, e
                                    );
                                    let _ = tokio::fs::remove_file(&path).await;
                                }
                            }
                        }

                        if let Some(parent) = path.parent() {
                            tokio::fs::create_dir_all(parent)
                                .await
                                .map_err(|e| e.to_string())?;
                        }

                        match provider
                            .download_binary_to_path(&version, &task.platform, &task.filename, &path)
                            .await
                        {
                            Ok(result) => {
                                if result.sha256 != task.checksum {
                                    warn!(
                                        "Checksum verification failed for {}/{}: expected {}, got {}",
                                        version, task.platform, task.checksum, result.sha256
                                    );
                                    let _ = tokio::fs::remove_file(&path).await;
                                    return Err(format!("Checksum mismatch for {}", task.platform));
                                }

                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt;
                                    let mut perms = tokio::fs::metadata(&path)
                                        .await
                                        .map_err(|e| e.to_string())?
                                        .permissions();
                                    perms.set_mode(0o755);
                                    tokio::fs::set_permissions(&path, perms)
                                        .await
                                        .map_err(|e| e.to_string())?;
                                }

                                info!(
                                    "Saved binary: {}/{}/{} ({} bytes)",
                                    version, task.platform, task.filename, result.size
                                );
                                Ok((
                                    task.platform,
                                    PlatformMetadata {
                                        sha256: task.checksum,
                                        size: result.size,
                                        filename: task.filename,
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

        // Update metadata only after all platforms are ready
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
        let provider = &metadata.claude_code;
        let Some(version_meta) = provider.versions.get(version) else {
            return false;
        };

        for platform in &self.config.platforms {
            let filename = if platform.starts_with("win32") {
                "claude.exe"
            } else {
                "claude"
            };

            if !version_meta.platforms.contains_key(platform) {
                return false;
            }

            if matches!(self.storage.mode, StorageMode::Local)
                && !self
                    .cache
                    .binary_exists(PROVIDER_NAME, version, platform, filename)
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
            let mut keys = Vec::new();
            if let Some(key) = self
                .cache
                .build_object_key(PROVIDER_NAME, &["versions", version, "manifest.json"])
            {
                keys.push(key);
            }
            for (platform, meta) in &version_meta.platforms {
                if let Some(key) = self.cache.build_object_key(
                    PROVIDER_NAME,
                    &["versions", version, platform, &meta.filename],
                ) {
                    keys.push(key);
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

    /// Sync all configured tags
    pub async fn sync_all(&self) -> Result<Vec<String>> {
        let mut updated = Vec::new();

        for tag in &self.config.tags {
            match self.sync_tag(tag).await {
                Ok(Some(version)) => {
                    updated.push(format!("{}: {}", tag, version));
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to sync tag {}: {:?}", tag, e);
                }
            }
        }

        Ok(updated)
    }

    /// Get cached tag version
    pub async fn get_tag_version(&self, tag: &str) -> Option<String> {
        self.cache.read_tag(PROVIDER_NAME, tag).await
    }

    /// Get info for API response
    pub async fn get_info(&self) -> serde_json::Value {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.claude_code;

        let mut platforms = serde_json::Map::new();

        // Get the latest version to show platform info
        if let Some(latest_version) = provider.tags.get("latest") {
            if let Some(version_meta) = provider.versions.get(latest_version) {
                for (platform, meta) in &version_meta.platforms {
                    let filename = &meta.filename;
                    platforms.insert(
                        platform.clone(),
                        serde_json::json!({
                            "version": latest_version,
                            "url": format!("/claude-code/{}/{}/{}", latest_version, platform, filename),
                            "sha256": meta.sha256,
                            "size": meta.size
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
