use anyhow::{Context, Result};
use chrono::Utc;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::cache::{CacheManager, PlatformMetadata, VersionMetadata};
use crate::config::ClaudeCodeConfig;
use crate::error::MirrorError;

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
}

impl ClaudeCodeProvider {
    pub fn new(config: ClaudeCodeConfig, cache: Arc<CacheManager>) -> Self {
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
        }
    }

    /// Get version for a tag from upstream
    pub async fn fetch_upstream_tag(&self, tag: &str) -> Result<String> {
        let url = format!("{}/{}", self.config.upstream_url, tag);
        info!("Fetching tag from upstream: {}", url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch tag: {}", tag))?;

        if !response.status().is_success() {
            return Err(MirrorError::VersionNotFound(tag.to_string()).into());
        }

        let version = response.text().await?.trim().to_string();

        Ok(version)
    }

    /// Get manifest for a version from upstream
    pub async fn fetch_upstream_manifest(&self, version: &str) -> Result<Manifest> {
        let url = format!("{}/{}/manifest.json", self.config.upstream_url, version);
        info!("Fetching manifest from upstream: {}", url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch manifest for version: {}", version))?;

        if !response.status().is_success() {
            return Err(MirrorError::VersionNotFound(version.to_string()).into());
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

        let response =
            self.client.get(&url).send().await.with_context(|| {
                format!("Failed to download binary for {}/{}", version, platform)
            })?;

        if !response.status().is_success() {
            return Err(MirrorError::PlatformNotFound(platform.to_string()).into());
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
        if deleted > 0 {
            info!("Cleaned up {} old versions", deleted);
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

        // Save manifest
        let manifest_path = self
            .cache
            .version_path(PROVIDER_NAME, version)
            .join("manifest.json");
        if let Some(parent) = manifest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        tokio::fs::write(&manifest_path, &manifest_json).await?;

        // Download each configured platform
        let mut platforms_metadata = HashMap::new();
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

            let path = self
                .cache
                .binary_path(PROVIDER_NAME, version, platform, filename);

            if path.exists() {
                match Self::verify_file_checksum(&path, &platform_manifest.checksum).await {
                    Ok(size) => {
                        platforms_metadata.insert(
                            platform.clone(),
                            PlatformMetadata {
                                sha256: platform_manifest.checksum.clone(),
                                size,
                                filename: filename.to_string(),
                            },
                        );
                        info!(
                            "Binary verified: {}/{}/{} ({} bytes)",
                            version, platform, filename, size
                        );
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            "Existing binary checksum failed for {}/{}: {}",
                            version, platform, e
                        );
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            match self
                .download_binary_to_path(version, platform, filename, &path)
                .await
            {
                Ok(result) => {
                    if result.sha256 != platform_manifest.checksum {
                        warn!(
                            "Checksum verification failed for {}/{}: expected {}, got {}",
                            version, platform, platform_manifest.checksum, result.sha256
                        );
                        let _ = tokio::fs::remove_file(&path).await;
                        failures.push(format!("Checksum mismatch for {}", platform));
                        continue;
                    }

                    // Set executable permission on Unix
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = tokio::fs::metadata(&path).await?.permissions();
                        perms.set_mode(0o755);
                        tokio::fs::set_permissions(&path, perms).await?;
                    }

                    platforms_metadata.insert(
                        platform.clone(),
                        PlatformMetadata {
                            sha256: platform_manifest.checksum.clone(),
                            size: result.size,
                            filename: filename.to_string(),
                        },
                    );

                    info!(
                        "Saved binary: {}/{}/{} ({} bytes)",
                        version, platform, filename, result.size
                    );
                }
                Err(e) => {
                    warn!("Failed to download {}/{}: {}", version, platform, e);
                    let _ = tokio::fs::remove_file(&path).await;
                    failures.push(format!("Download failed for {}", platform));
                }
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

            if !self
                .cache
                .binary_exists(PROVIDER_NAME, version, platform, filename)
                .await
            {
                return false;
            }
        }

        true
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
                    warn!("Failed to sync tag {}: {}", tag, e);
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
