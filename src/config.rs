use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Main configuration structure
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub cache: CacheConfig,

    #[serde(default)]
    pub update: UpdateConfig,

    #[serde(default)]
    pub claude_code: ClaudeCodeConfig,

    #[serde(default)]
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_host")]
    pub host: String,

    /// Public URL for install scripts (e.g., "http://yourip:1357")
    /// If not set, install script endpoints will return 503
    #[serde(default)]
    pub public_url: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            public_url: default_public_url(),
        }
    }
}

fn default_public_url() -> Option<String> {
    std::env::var("MIRROR_PUBLIC_URL").ok()
}

fn default_port() -> u16 {
    std::env::var("MIRROR_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1357)
}

fn default_host() -> String {
    std::env::var("MIRROR_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_cache_dir")]
    pub dir: PathBuf,

    #[serde(default = "default_max_versions")]
    pub max_versions: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            dir: default_cache_dir(),
            max_versions: default_max_versions(),
        }
    }
}

fn default_cache_dir() -> PathBuf {
    std::env::var("MIRROR_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./cache"))
}

fn default_max_versions() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u64,

    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_interval_minutes(),
            enabled: default_enabled(),
        }
    }
}

fn default_interval_minutes() -> u64 {
    std::env::var("MIRROR_UPDATE_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCodeConfig {
    #[serde(default = "default_claude_code_enabled")]
    pub enabled: bool,

    #[serde(default = "default_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_claude_code_platforms")]
    pub platforms: Vec<String>,

    #[serde(default = "default_upstream_url")]
    pub upstream_url: String,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            enabled: default_claude_code_enabled(),
            tags: default_tags(),
            platforms: default_claude_code_platforms(),
            upstream_url: default_upstream_url(),
        }
    }
}

fn default_claude_code_enabled() -> bool {
    std::env::var("MIRROR_CLAUDE_CODE_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_tags() -> Vec<String> {
    vec!["stable".to_string(), "latest".to_string()]
}

fn default_claude_code_platforms() -> Vec<String> {
    vec![
        "darwin-x64".to_string(),
        "darwin-arm64".to_string(),
        "linux-x64".to_string(),
        "linux-arm64".to_string(),
        "linux-x64-musl".to_string(),
        "linux-arm64-musl".to_string(),
        "win32-x64".to_string(),
    ]
}

fn default_upstream_url() -> String {
    "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexConfig {
    #[serde(default = "default_codex_enabled")]
    pub enabled: bool,

    #[serde(default = "default_codex_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_codex_platforms")]
    pub platforms: Vec<String>,

    #[serde(default = "default_codex_repo")]
    pub repo: String,

    #[serde(default = "default_codex_include_prerelease")]
    pub include_prerelease: bool,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            enabled: default_codex_enabled(),
            tags: default_codex_tags(),
            platforms: default_codex_platforms(),
            repo: default_codex_repo(),
            include_prerelease: default_codex_include_prerelease(),
        }
    }
}

fn default_codex_enabled() -> bool {
    std::env::var("MIRROR_CODEX_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_codex_tags() -> Vec<String> {
    default_tags()
}

fn default_codex_platforms() -> Vec<String> {
    vec![
        "darwin-x64".to_string(),
        "darwin-arm64".to_string(),
        "linux-x64".to_string(),
        "linux-arm64".to_string(),
        "linux-x64-musl".to_string(),
        "linux-arm64-musl".to_string(),
        "win32-x64".to_string(),
        "win32-arm64".to_string(),
    ]
}

fn default_codex_repo() -> String {
    std::env::var("MIRROR_CODEX_REPO").unwrap_or_else(|_| "openai/codex".to_string())
}

fn default_codex_include_prerelease() -> bool {
    std::env::var("MIRROR_CODEX_INCLUDE_PRERELEASE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false)
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;
            let config: Config = toml::from_str(&content)
                .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
            Ok(config)
        } else {
            // Return default config if file doesn't exist
            tracing::warn!(
                "Config file not found at {}, using defaults",
                path.display()
            );
            Ok(Config::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 1357);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.cache.max_versions, 10);
        assert!(config.update.enabled);
        assert_eq!(config.update.interval_minutes, 10);
        assert!(config.claude_code.enabled);
        assert!(config.codex.enabled);
        assert_eq!(config.codex.repo, "openai/codex");
    }

    #[test]
    fn test_load_config_from_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[server]
port = 8080
host = "127.0.0.1"
public_url = "http://example.com"

[cache]
dir = "/tmp/cache"
max_versions = 5

[update]
interval_minutes = 30
enabled = false

[claude_code]
enabled = true
tags = ["stable"]

[codex]
enabled = true
tags = ["stable", "latest"]
include_prerelease = true
repo = "openai/codex"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(
            config.server.public_url,
            Some("http://example.com".to_string())
        );
        assert_eq!(config.cache.max_versions, 5);
        assert_eq!(config.update.interval_minutes, 30);
        assert!(!config.update.enabled);
        assert_eq!(config.claude_code.tags, vec!["stable"]);
        assert_eq!(config.codex.tags, vec!["stable", "latest"]);
        assert!(config.codex.include_prerelease);
    }

    #[test]
    fn test_load_nonexistent_file_returns_default() {
        let config = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config.server.port, 1357);
    }

    #[test]
    fn test_default_claude_code_platforms() {
        let platforms = default_claude_code_platforms();
        assert!(platforms.contains(&"darwin-arm64".to_string()));
        assert!(platforms.contains(&"linux-x64".to_string()));
        assert!(platforms.contains(&"win32-x64".to_string()));
        assert_eq!(platforms.len(), 7);
    }

    #[test]
    fn test_default_codex_platforms() {
        let platforms = default_codex_platforms();
        assert!(platforms.contains(&"darwin-arm64".to_string()));
        assert!(platforms.contains(&"linux-x64".to_string()));
        assert!(platforms.contains(&"win32-x64".to_string()));
        assert!(platforms.contains(&"win32-arm64".to_string()));
        assert_eq!(platforms.len(), 8);
    }
}
