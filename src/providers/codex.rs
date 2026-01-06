use anyhow::{Context, Result};
use chrono::Utc;
use futures::StreamExt;
use reqwest::{Client, header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::cache::{CacheManager, PlatformMetadata, VersionMetadata};
use crate::config::CodexConfig;
use crate::error::MirrorError;

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
}

impl CodexProvider {
    pub fn new(config: CodexConfig, cache: Arc<CacheManager>) -> Self {
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
        let response = self
            .api_request(&url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch releases from {}", url))?;

        if !response.status().is_success() {
            return Err(MirrorError::VersionNotFound("releases".to_string()).into());
        }

        let releases = response.json::<Vec<Release>>().await?;
        Ok(releases)
    }

    async fn fetch_release_by_tag(&self, tag: &str) -> Result<Release> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            GITHUB_API_BASE, self.config.repo, tag
        );
        let response = self
            .api_request(&url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch release tag {}", tag))?;

        if !response.status().is_success() {
            return Err(MirrorError::VersionNotFound(tag.to_string()).into());
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
        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;

        if !response.status().is_success() {
            return Err(MirrorError::Provider(format!("Failed to download asset: {}", url)).into());
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
        if deleted > 0 {
            info!("Cleaned up {} old versions", deleted);
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

        let mut platforms_metadata = HashMap::new();
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

            let path = self
                .cache
                .binary_path(PROVIDER_NAME, version, platform, &asset.name);

            if path.exists() {
                let expected = existing_version
                    .and_then(|version_meta| version_meta.platforms.get(platform))
                    .map(|meta| meta.sha256.clone())
                    .or_else(|| Self::asset_digest_sha256(asset).map(str::to_string));

                if let Some(expected) = expected {
                    match Self::verify_file_checksum(&path, &expected).await {
                        Ok(size) => {
                            platforms_metadata.insert(
                                platform.clone(),
                                PlatformMetadata {
                                    sha256: expected,
                                    size,
                                    filename: asset.name.clone(),
                                },
                            );
                            info!(
                                "Asset verified: {}/{}/{} ({} bytes)",
                                version, platform, asset.name, size
                            );
                            continue;
                        }
                        Err(e) => {
                            warn!(
                                "Existing asset checksum failed for {}/{}: {}",
                                version, platform, e
                            );
                            let _ = tokio::fs::remove_file(&path).await;
                        }
                    }
                } else {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            match self
                .download_asset_to_path(&asset.browser_download_url, &path)
                .await
            {
                Ok(result) => {
                    if let Some(expected) = Self::asset_digest_sha256(asset) {
                        if result.sha256 != expected {
                            warn!(
                                "Checksum verification failed for {}/{}: expected {}, got {}",
                                version, platform, expected, result.sha256
                            );
                            let _ = tokio::fs::remove_file(&path).await;
                            failures.push(format!("Checksum mismatch for {}", platform));
                            continue;
                        }
                    }

                    platforms_metadata.insert(
                        platform.clone(),
                        PlatformMetadata {
                            sha256: result.sha256,
                            size: result.size,
                            filename: asset.name.clone(),
                        },
                    );

                    info!(
                        "Saved asset: {}/{}/{} ({} bytes)",
                        version, platform, asset.name, result.size
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

            if !self
                .cache
                .binary_exists(PROVIDER_NAME, version, platform, &platform_meta.filename)
                .await
            {
                return false;
            }
        }

        true
    }

    pub async fn sync_all(&self) -> Result<Vec<String>> {
        let mut updated = Vec::new();

        for tag in &self.config.tags {
            match self.sync_tag(tag).await {
                Ok(Some(version)) => updated.push(format!("{}: {}", tag, version)),
                Ok(None) => {}
                Err(e) => warn!("Failed to sync tag {}: {}", tag, e),
            }
        }

        Ok(updated)
    }

    pub async fn get_tag_version(&self, tag: &str) -> Option<String> {
        self.cache.read_tag(PROVIDER_NAME, tag).await
    }

    pub async fn get_info(&self) -> serde_json::Value {
        let metadata = self.cache.get_metadata().await;
        let provider = &metadata.codex;

        let display_version = provider
            .tags
            .get("latest")
            .or_else(|| provider.tags.get("stable"));

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
            "tags": provider.tags,
            "platforms": platforms,
            "updated_at": provider.updated_at
        })
    }
}
