use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::cache::{CacheManager, FileMetadata, PlatformMetadata, VersionMetadata};
use crate::config::{NodePtyConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;

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
}

pub struct NodePtyProvider {
    config: NodePtyConfig,
    cache: Arc<CacheManager>,
    storage: StorageConfig,
}

impl NodePtyProvider {
    pub fn new(config: NodePtyConfig, cache: Arc<CacheManager>, storage: StorageConfig) -> Self {
        Self {
            config,
            cache,
            storage,
        }
    }

    fn prebuild_path(&self, version: &str, platform: &str, filename: &str) -> PathBuf {
        self.cache
            .version_path(PROVIDER_NAME, version)
            .join("prebuilds")
            .join(platform)
            .join(filename)
    }

    async fn read_checksums(&self, version: &str) -> Result<ChecksumsFile> {
        match self.storage.mode {
            StorageMode::Local => {
                let path = self
                    .cache
                    .get_file_path(PROVIDER_NAME, &["versions", version, "checksums.json"])
                    .ok_or_else(|| MirrorError::VersionNotFound(version.to_string()))?;

                let content = tokio::fs::read_to_string(&path)
                    .await
                    .with_context(|| format!("Failed to read checksums.json for {}", version))?;

                Ok(serde_json::from_str(&content)?)
            }
            StorageMode::Oss => {
                let key = self
                    .cache
                    .build_object_key(PROVIDER_NAME, &["versions", version, "checksums.json"])
                    .ok_or_else(|| MirrorError::VersionNotFound(version.to_string()))?;
                let content = oss::get_object_bytes(&self.storage.oss, &key).await?;
                Ok(serde_json::from_slice(&content)?)
            }
        }
    }

    async fn verify_file_size(path: &Path, expected: u64) -> Result<()> {
        let actual = tokio::fs::metadata(path).await?.len();
        if actual != expected {
            return Err(MirrorError::Provider(format!(
                "Size mismatch for {}: expected {}, got {}",
                path.display(),
                expected,
                actual
            ))
            .into());
        }
        Ok(())
    }

    pub async fn sync_tag(&self, tag: &str) -> Result<Option<String>> {
        if !self.config.enabled {
            return Ok(None);
        }

        let cached_version = self.cache.read_tag(PROVIDER_NAME, tag).await;
        let Some(version) = cached_version else {
            warn!("Tag {} not found for node-pty", tag);
            return Ok(None);
        };

        self.sync_version(&version).await?;

        self.cache
            .update_provider_metadata(PROVIDER_NAME, |m| {
                m.tags.insert(tag.to_string(), version.clone());
                m.updated_at = Some(Utc::now());
            })
            .await?;

        let deleted = self.cache.cleanup_old_versions(PROVIDER_NAME).await?;
        if !deleted.is_empty() {
            info!("Cleaned up {} old versions", deleted.len());
            if matches!(self.storage.mode, StorageMode::Oss) {
                self.delete_oss_versions(&deleted).await;
            }
        }

        Ok(Some(version))
    }

    pub async fn sync_version(&self, version: &str) -> Result<()> {
        if self.is_version_complete(version).await {
            info!("Version {} already cached", version);
            return Ok(());
        }

        info!("Syncing version: {}", version);

        let checksums = self.read_checksums(version).await?;
        let mut platforms_metadata = HashMap::new();

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

            let mut files_meta = HashMap::new();
            for (filename, entry) in &platform_meta.files {
                let path = self.prebuild_path(version, platform, filename);
                if matches!(self.storage.mode, StorageMode::Local) {
                    if !path.exists() {
                        return Err(MirrorError::Provider(format!(
                            "Missing file for {}/{}: {}",
                            version, platform, filename
                        ))
                        .into());
                    }

                    Self::verify_file_size(&path, entry.size).await?;
                }

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
                if let Err(e) = oss::delete_object(&self.storage.oss, &key).await {
                    warn!("Failed to delete OSS object {}: {:?}", key, e);
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
