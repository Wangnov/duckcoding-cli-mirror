use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::CacheConfig;

/// Metadata for a cached version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMetadata {
    pub version: String,
    pub downloaded_at: DateTime<Utc>,
    pub platforms: HashMap<String, PlatformMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformMetadata {
    pub sha256: String,
    pub size: u64,
    pub filename: String,
}

/// Provider-specific metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderMetadata {
    pub tags: HashMap<String, String>, // tag -> version
    pub versions: HashMap<String, VersionMetadata>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Global cache metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheMetadata {
    #[serde(default)]
    pub claude_code: ProviderMetadata,
    #[serde(default)]
    pub codex: ProviderMetadata,
    // Future: gemini, node
}

/// Cache manager handles all file operations and metadata
pub struct CacheManager {
    pub config: CacheConfig,
    metadata: Arc<RwLock<CacheMetadata>>,
}

impl CacheManager {
    pub fn new(config: &CacheConfig) -> Result<Self> {
        // Create cache directory if it doesn't exist
        std::fs::create_dir_all(&config.dir).with_context(|| {
            format!("Failed to create cache directory: {}", config.dir.display())
        })?;

        // Load or create metadata
        let metadata_path = config.dir.join("metadata.json");
        let metadata = if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            CacheMetadata::default()
        };

        Ok(Self {
            config: config.clone(),
            metadata: Arc::new(RwLock::new(metadata)),
        })
    }

    /// Get the base path for a provider
    pub fn provider_path(&self, provider: &str) -> PathBuf {
        self.config.dir.join(provider)
    }

    /// Get the path for a specific version
    pub fn version_path(&self, provider: &str, version: &str) -> PathBuf {
        self.provider_path(provider).join("versions").join(version)
    }

    /// Get the path for a binary
    pub fn binary_path(
        &self,
        provider: &str,
        version: &str,
        platform: &str,
        filename: &str,
    ) -> PathBuf {
        self.version_path(provider, version)
            .join(platform)
            .join(filename)
    }

    /// Get the path for a tag file
    pub fn tag_path(&self, provider: &str, tag: &str) -> PathBuf {
        self.provider_path(provider).join("tags").join(tag)
    }

    /// Read a tag to get version
    pub async fn read_tag(&self, provider: &str, tag: &str) -> Option<String> {
        let path = self.tag_path(provider, tag);
        tokio::fs::read_to_string(&path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Write a tag
    pub async fn write_tag(&self, provider: &str, tag: &str, version: &str) -> Result<()> {
        let path = self.tag_path(provider, tag);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, version).await?;
        Ok(())
    }

    /// Check if a binary exists
    pub async fn binary_exists(
        &self,
        provider: &str,
        version: &str,
        platform: &str,
        filename: &str,
    ) -> bool {
        self.binary_path(provider, version, platform, filename)
            .exists()
    }

    /// Read metadata
    pub async fn get_metadata(&self) -> CacheMetadata {
        self.metadata.read().await.clone()
    }

    /// Update metadata for a provider
    pub async fn update_provider_metadata<F>(&self, provider: &str, updater: F) -> Result<()>
    where
        F: FnOnce(&mut ProviderMetadata),
    {
        let content = {
            let mut metadata = self.metadata.write().await;

            let provider_metadata = match provider {
                "claude-code" => &mut metadata.claude_code,
                "codex" => &mut metadata.codex,
                _ => return Err(anyhow::anyhow!("Unknown provider: {}", provider)),
            };

            updater(provider_metadata);

            serde_json::to_string_pretty(&*metadata)?
        };

        // Save to disk without holding the lock across await.
        let metadata_path = self.config.dir.join("metadata.json");
        tokio::fs::write(&metadata_path, content).await?;

        Ok(())
    }

    /// List all cached versions for a provider
    pub async fn list_versions(&self, provider: &str) -> Vec<String> {
        let metadata = self.metadata.read().await;
        match provider {
            "claude-code" => metadata.claude_code.versions.keys().cloned().collect(),
            "codex" => metadata.codex.versions.keys().cloned().collect(),
            _ => vec![],
        }
    }

    /// Clean up old versions, keeping only max_versions
    pub async fn cleanup_old_versions(&self, provider: &str) -> Result<usize> {
        let max_versions = self.config.max_versions;

        let versions_to_remove = {
            let metadata = self.metadata.read().await;
            let provider_metadata = match provider {
                "claude-code" => &metadata.claude_code,
                "codex" => &metadata.codex,
                _ => return Ok(0),
            };

            // Get versions that are currently tagged (should not be deleted)
            let tagged_versions: std::collections::HashSet<_> =
                provider_metadata.tags.values().cloned().collect();

            // Collect versions to delete (oldest first, excluding tagged)
            let mut versions_with_time: Vec<(String, chrono::DateTime<Utc>)> = provider_metadata
                .versions
                .iter()
                .filter(|(v, _)| !tagged_versions.contains(*v))
                .map(|(v, m)| (v.clone(), m.downloaded_at))
                .collect();

            // Sort by download time (oldest first)
            versions_with_time.sort_by_key(|(_, dt)| *dt);

            // Calculate how many to delete
            let total_versions = provider_metadata.versions.len();
            let deletable = versions_with_time.len();
            let to_delete = if total_versions > max_versions {
                (total_versions - max_versions).min(deletable)
            } else {
                0
            };

            // Get version names to delete
            versions_with_time
                .into_iter()
                .take(to_delete)
                .map(|(v, _)| v)
                .collect::<Vec<String>>()
        };

        let mut deleted_versions = Vec::new();
        for version in &versions_to_remove {
            let version_path = self.version_path(provider, version);
            if version_path.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&version_path).await {
                    tracing::warn!(
                        "Failed to delete version directory {}: {}",
                        version_path.display(),
                        e
                    );
                } else {
                    deleted_versions.push(version.clone());
                    tracing::info!("Deleted old version: {}/{}", provider, version);
                }
            }
        }

        // Save metadata if we deleted anything
        if !deleted_versions.is_empty() {
            let content = {
                let mut metadata = self.metadata.write().await;
                let provider_metadata = match provider {
                    "claude-code" => &mut metadata.claude_code,
                    "codex" => &mut metadata.codex,
                    _ => return Ok(0),
                };

                for version in &deleted_versions {
                    provider_metadata.versions.remove(version);
                }

                serde_json::to_string_pretty(&*metadata)?
            };

            let metadata_path = self.config.dir.join("metadata.json");
            tokio::fs::write(&metadata_path, content).await?;
        }

        Ok(deleted_versions.len())
    }

    /// Get file path for serving
    pub fn get_file_path(&self, provider: &str, path_segments: &[&str]) -> Option<PathBuf> {
        let base = self.provider_path(provider);
        let mut result = base;
        for segment in path_segments {
            // Prevent path traversal
            if segment.contains("..") || segment.contains('/') || segment.contains('\\') {
                return None;
            }
            result = result.join(segment);
        }
        if result.exists() { Some(result) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_cache() -> (TempDir, CacheManager) {
        let temp_dir = TempDir::new().unwrap();
        let config = CacheConfig {
            dir: temp_dir.path().to_path_buf(),
            max_versions: 3,
        };
        let cache = CacheManager::new(&config).unwrap();
        (temp_dir, cache)
    }

    #[test]
    fn test_cache_manager_creation() {
        let (_temp_dir, cache) = create_test_cache();
        assert_eq!(cache.config.max_versions, 3);
    }

    #[test]
    fn test_provider_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.provider_path("claude-code");
        assert_eq!(path, temp_dir.path().join("claude-code"));
    }

    #[test]
    fn test_version_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.version_path("claude-code", "1.0.0");
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("claude-code")
                .join("versions")
                .join("1.0.0")
        );
    }

    #[test]
    fn test_binary_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.binary_path("claude-code", "1.0.0", "darwin-arm64", "claude");
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("claude-code")
                .join("versions")
                .join("1.0.0")
                .join("darwin-arm64")
                .join("claude")
        );
    }

    #[test]
    fn test_tag_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.tag_path("claude-code", "stable");
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("claude-code")
                .join("tags")
                .join("stable")
        );
    }

    #[test]
    fn test_get_file_path_prevents_traversal() {
        let (_temp_dir, cache) = create_test_cache();

        // Path traversal should return None
        assert!(
            cache
                .get_file_path("claude-code", &["../etc/passwd"])
                .is_none()
        );
        assert!(
            cache
                .get_file_path("claude-code", &["foo", "..", "bar"])
                .is_none()
        );
        assert!(cache.get_file_path("claude-code", &["foo/bar"]).is_none());
    }

    #[tokio::test]
    async fn test_read_write_tag() {
        let (_temp_dir, cache) = create_test_cache();

        // Write tag
        cache
            .write_tag("claude-code", "stable", "1.0.0")
            .await
            .unwrap();

        // Read tag
        let version = cache.read_tag("claude-code", "stable").await;
        assert_eq!(version, Some("1.0.0".to_string()));
    }

    #[tokio::test]
    async fn test_read_nonexistent_tag() {
        let (_temp_dir, cache) = create_test_cache();

        let version = cache.read_tag("claude-code", "nonexistent").await;
        assert!(version.is_none());
    }
}
