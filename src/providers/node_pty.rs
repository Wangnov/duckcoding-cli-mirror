use anyhow::Result;
use chrono::Utc;
use futures::{StreamExt, stream};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::cache::{CacheManager, FileMetadata, PlatformMetadata, VersionMetadata};
use crate::config::{HttpConfig, NodePtyConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;
use crate::providers::github::{DownloadResult, GithubClient, Release};
use crate::retry::sync_concurrency;
use crate::s3;
use crate::storage_clients::StorageClients;

const PROVIDER_NAME: &str = "node-pty";
const PRIMARY_FILE: &str = "pty.node";

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

pub struct NodePtyProvider {
    config: NodePtyConfig,
    cache: Arc<CacheManager>,
    storage: StorageConfig,
    storage_clients: StorageClients,
    github: GithubClient,
}

impl NodePtyProvider {
    pub fn new(
        config: NodePtyConfig,
        cache: Arc<CacheManager>,
        storage: StorageConfig,
        storage_clients: StorageClients,
        http: HttpConfig,
    ) -> Result<Self> {
        let github = GithubClient::new(&http)?;

        Ok(Self {
            config,
            cache,
            storage,
            storage_clients,
            github,
        })
    }

    fn prebuild_path(&self, version: &str, platform: &str, filename: &str) -> PathBuf {
        self.cache
            .version_path(PROVIDER_NAME, version)
            .join("prebuilds")
            .join(platform)
            .join(filename)
    }

    async fn fetch_releases(&self) -> Result<Vec<Release>> {
        self.github.fetch_releases(&self.config.repo).await
    }

    async fn fetch_release_by_tag(&self, tag: &str) -> Result<Release> {
        self.github
            .fetch_release_by_tag(&self.config.repo, tag)
            .await
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
        self.github.download_asset_bytes(url).await
    }

    async fn download_asset_to_path(&self, url: &str, path: &Path) -> Result<DownloadResult> {
        self.github.download_asset_to_path(url, path).await
    }

    async fn download_asset_to_remote(
        &self,
        url: &str,
        object_key: &str,
    ) -> Result<DownloadResult> {
        self.github
            .download_asset_to_storage(
                url,
                &self.storage,
                &self.storage_clients,
                object_key,
                "application/octet-stream",
            )
            .await
    }

    async fn try_use_existing_s3_object(
        &self,
        object_key: &str,
        expected_sha256: &str,
        expected_size: u64,
    ) -> Result<Option<DownloadResult>> {
        let Some(client) = self.storage_clients.s3() else {
            return Ok(None);
        };
        let info = s3::head_object_info_with_client(client, &self.storage.s3, object_key).await?;
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
        if tag == "latest" || tag == "stable" {
            let releases = self.fetch_releases().await?;
            let release = self
                .select_release(&releases, tag)
                .ok_or_else(|| MirrorError::VersionNotFound(tag.to_string()))?;
            Ok(release.tag_name.clone())
        } else {
            let release = self.fetch_release_by_tag(tag).await?;
            Ok(release.tag_name.clone())
        }
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
                                    if let Some(client) = provider.storage_clients.oss() {
                                        let _ = oss::delete_object_with_client(
                                            client,
                                            &provider.storage.oss,
                                            &key,
                                        )
                                        .await;
                                    }
                                }
                                StorageMode::S3 => {
                                    if let Some(client) = provider.storage_clients.s3() {
                                        let _ = s3::delete_object_with_client(
                                            client,
                                            &provider.storage.s3,
                                            &key,
                                        )
                                        .await;
                                    }
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
                let (name, entry) = platform_meta.files.iter().next().ok_or_else(|| {
                    MirrorError::Provider(format!("No files listed for platform {}", platform))
                })?;
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
                let Some(client) = self.storage_clients.oss() else {
                    return Err(
                        MirrorError::Provider("OSS client not initialized".to_string()).into(),
                    );
                };
                oss::put_bytes_with_client(
                    client,
                    &self.storage.oss,
                    &key,
                    "application/json",
                    bytes.to_vec(),
                )
                .await?;
            }
            StorageMode::S3 => {
                let Some(client) = self.storage_clients.s3() else {
                    return Err(
                        MirrorError::Provider("S3 client not initialized".to_string()).into(),
                    );
                };
                s3::put_bytes_with_client(
                    client,
                    &self.storage.s3,
                    &key,
                    "application/json",
                    bytes.to_vec(),
                )
                .await?;
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
                        if let Some(client) = self.storage_clients.oss() {
                            if let Err(e) =
                                oss::delete_object_with_client(client, &self.storage.oss, &key)
                                    .await
                            {
                                warn!("Failed to delete OSS object {}: {:?}", key, e);
                            }
                        }
                    }
                    StorageMode::S3 => {
                        if let Some(client) = self.storage_clients.s3() {
                            if let Err(e) =
                                s3::delete_object_with_client(client, &self.storage.s3, &key).await
                            {
                                warn!("Failed to delete S3 object {}: {:?}", key, e);
                            }
                        }
                    }
                    StorageMode::Local => {}
                }
            }
        }
    }

    pub async fn sync_all(&self) -> Result<Vec<String>> {
        let mut updated = Vec::new();
        let mut errors = Vec::new();

        for tag in &self.config.tags {
            match self.sync_tag(tag).await {
                Ok(Some(version)) => updated.push(format!("{}: {}", tag, version)),
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to sync tag {}: {:?}", tag, e);
                    errors.push(format!("{}: {}", tag, e));
                }
            }
        }

        if errors.is_empty() {
            Ok(updated)
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
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
            "updated_at": provider.updated_at,
            "sync": &provider.sync,
        })
    }
}
