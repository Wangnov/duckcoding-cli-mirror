use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use crate::cache::{CacheManager, PlatformMetadata, VersionMetadata};
use crate::config::{NodeConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::oss;
use crate::s3;

const PROVIDER_NAME: &str = "node";

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

pub struct NodeProvider {
    config: NodeConfig,
    cache: Arc<CacheManager>,
    storage: StorageConfig,
}

impl NodeProvider {
    pub fn new(config: NodeConfig, cache: Arc<CacheManager>, storage: StorageConfig) -> Self {
        Self {
            config,
            cache,
            storage,
        }
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
            StorageMode::S3 => {
                let key = self
                    .cache
                    .build_object_key(PROVIDER_NAME, &["versions", version, "checksums.json"])
                    .ok_or_else(|| MirrorError::VersionNotFound(version.to_string()))?;
                let content = s3::get_object_bytes(&self.storage.s3, &key).await?;
                Ok(serde_json::from_slice(&content)?)
            }
        }
    }

    fn select_single_file(
        platform: &str,
        files: &HashMap<String, ChecksumsEntry>,
    ) -> Result<(String, ChecksumsEntry)> {
        if files.is_empty() {
            return Err(MirrorError::Provider(format!(
                "No files listed for platform {}",
                platform
            ))
            .into());
        }
        if files.len() > 1 {
            return Err(MirrorError::Provider(format!(
                "Multiple files listed for platform {}",
                platform
            ))
            .into());
        }

        let (name, entry) = files.iter().next().unwrap();
        Ok((name.clone(), entry.clone()))
    }

    async fn verify_file_size(path: &std::path::Path, expected: u64) -> Result<()> {
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
            warn!("Tag {} not found for node", tag);
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
            if matches!(self.storage.mode, StorageMode::Oss | StorageMode::S3) {
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

            let (filename, entry) = Self::select_single_file(platform, &platform_meta.files)?;
            let path = self
                .cache
                .binary_path(PROVIDER_NAME, version, platform, &filename);

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

            platforms_metadata.insert(
                platform.clone(),
                PlatformMetadata {
                    sha256: entry.sha256,
                    size: entry.size,
                    filename,
                    files: HashMap::new(),
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
        let provider = &metadata.node;
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
            let mut keys = Vec::new();
            if let Some(key) = self
                .cache
                .build_object_key(PROVIDER_NAME, &["versions", version, "checksums.json"])
            {
                keys.push(key);
            }
            if let Some(key) = self
                .cache
                .build_object_key(PROVIDER_NAME, &["versions", version, "SHASUMS256.txt"])
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
        let provider = &metadata.node;

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
                            "url": format!("/node/{}/{}/{}", version, platform, meta.filename),
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
