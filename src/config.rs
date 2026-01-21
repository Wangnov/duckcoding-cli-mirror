use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Main configuration structure
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub cache: CacheConfig,

    #[serde(default)]
    pub update: UpdateConfig,

    #[serde(default)]
    pub claude_code: ClaudeCodeConfig,

    #[serde(default)]
    pub codex: CodexConfig,

    #[serde(default)]
    pub gemini: GeminiConfig,

    #[serde(default)]
    pub installer: InstallerConfig,

    #[serde(default)]
    pub node: NodeConfig,

    #[serde(default)]
    pub node_pty: NodePtyConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_storage_mode")]
    pub mode: StorageMode,

    #[serde(default)]
    pub oss: OssConfig,

    #[serde(default)]
    pub s3: S3Config,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            mode: default_storage_mode(),
            oss: OssConfig::default(),
            s3: S3Config::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    #[default]
    Local,
    Oss,
    #[serde(alias = "r2")]
    S3,
}

fn default_storage_mode() -> StorageMode {
    match std::env::var("MIRROR_STORAGE_MODE")
        .ok()
        .as_deref()
        .map(|s| s.to_lowercase())
    {
        Some(ref v) if v == "oss" => StorageMode::Oss,
        Some(ref v) if v == "s3" || v == "r2" => StorageMode::S3,
        _ => StorageMode::Local,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OssConfig {
    #[serde(default = "default_oss_endpoint")]
    pub endpoint: String,

    #[serde(default = "default_oss_bucket")]
    pub bucket: String,

    #[serde(default = "default_oss_access_key_id")]
    pub access_key_id: String,

    #[serde(default = "default_oss_access_key_secret")]
    pub access_key_secret: String,

    #[serde(default = "default_oss_security_token")]
    pub security_token: Option<String>,

    #[serde(default = "default_oss_prefix")]
    pub prefix: String,

    #[serde(default = "default_oss_https")]
    pub https: bool,

    #[serde(default = "default_oss_path_style")]
    pub path_style: bool,

    #[serde(default = "default_oss_expires_seconds")]
    pub expires_seconds: u64,
}

impl Default for OssConfig {
    fn default() -> Self {
        Self {
            endpoint: default_oss_endpoint(),
            bucket: default_oss_bucket(),
            access_key_id: default_oss_access_key_id(),
            access_key_secret: default_oss_access_key_secret(),
            security_token: default_oss_security_token(),
            prefix: default_oss_prefix(),
            https: default_oss_https(),
            path_style: default_oss_path_style(),
            expires_seconds: default_oss_expires_seconds(),
        }
    }
}

fn default_oss_endpoint() -> String {
    std::env::var("MIRROR_OSS_ENDPOINT").unwrap_or_default()
}

fn default_oss_bucket() -> String {
    std::env::var("MIRROR_OSS_BUCKET").unwrap_or_default()
}

fn default_oss_access_key_id() -> String {
    std::env::var("MIRROR_OSS_ACCESS_KEY_ID").unwrap_or_default()
}

fn default_oss_access_key_secret() -> String {
    std::env::var("MIRROR_OSS_ACCESS_KEY_SECRET").unwrap_or_default()
}

fn default_oss_security_token() -> Option<String> {
    std::env::var("MIRROR_OSS_SECURITY_TOKEN").ok()
}

fn default_oss_prefix() -> String {
    std::env::var("MIRROR_OSS_PREFIX").unwrap_or_default()
}

fn default_oss_https() -> bool {
    std::env::var("MIRROR_OSS_HTTPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_oss_path_style() -> bool {
    std::env::var("MIRROR_OSS_PATH_STYLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false)
}

fn default_oss_expires_seconds() -> u64 {
    std::env::var("MIRROR_OSS_EXPIRES_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    #[serde(default = "default_s3_endpoint")]
    pub endpoint: String,

    #[serde(default = "default_s3_bucket")]
    pub bucket: String,

    #[serde(default = "default_s3_access_key_id")]
    pub access_key_id: String,

    #[serde(default = "default_s3_secret_access_key")]
    pub secret_access_key: String,

    #[serde(default = "default_s3_session_token")]
    pub session_token: Option<String>,

    #[serde(default = "default_s3_region")]
    pub region: String,

    #[serde(default = "default_s3_prefix")]
    pub prefix: String,

    #[serde(default = "default_s3_path_style")]
    pub path_style: bool,

    #[serde(default = "default_s3_expires_seconds")]
    pub expires_seconds: u64,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: default_s3_endpoint(),
            bucket: default_s3_bucket(),
            access_key_id: default_s3_access_key_id(),
            secret_access_key: default_s3_secret_access_key(),
            session_token: default_s3_session_token(),
            region: default_s3_region(),
            prefix: default_s3_prefix(),
            path_style: default_s3_path_style(),
            expires_seconds: default_s3_expires_seconds(),
        }
    }
}

fn default_s3_endpoint() -> String {
    std::env::var("MIRROR_S3_ENDPOINT").unwrap_or_default()
}

fn default_s3_bucket() -> String {
    std::env::var("MIRROR_S3_BUCKET").unwrap_or_default()
}

fn default_s3_access_key_id() -> String {
    std::env::var("MIRROR_S3_ACCESS_KEY_ID").unwrap_or_default()
}

fn default_s3_secret_access_key() -> String {
    std::env::var("MIRROR_S3_SECRET_ACCESS_KEY").unwrap_or_default()
}

fn default_s3_session_token() -> Option<String> {
    std::env::var("MIRROR_S3_SESSION_TOKEN").ok()
}

fn default_s3_region() -> String {
    std::env::var("MIRROR_S3_REGION").unwrap_or_else(|_| "auto".to_string())
}

fn default_s3_prefix() -> String {
    std::env::var("MIRROR_S3_PREFIX").unwrap_or_default()
}

fn default_s3_path_style() -> bool {
    std::env::var("MIRROR_S3_PATH_STYLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_s3_expires_seconds() -> u64 {
    std::env::var("MIRROR_S3_EXPIRES_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiConfig {
    #[serde(default = "default_gemini_enabled")]
    pub enabled: bool,

    #[serde(default = "default_gemini_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_gemini_repo")]
    pub repo: String,

    #[serde(default = "default_gemini_include_prerelease")]
    pub include_prerelease: bool,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            enabled: default_gemini_enabled(),
            tags: default_gemini_tags(),
            repo: default_gemini_repo(),
            include_prerelease: default_gemini_include_prerelease(),
        }
    }
}

fn default_gemini_enabled() -> bool {
    std::env::var("MIRROR_GEMINI_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_gemini_tags() -> Vec<String> {
    default_tags()
}

fn default_gemini_repo() -> String {
    std::env::var("MIRROR_GEMINI_REPO").unwrap_or_else(|_| "google-gemini/gemini-cli".to_string())
}

fn default_gemini_include_prerelease() -> bool {
    std::env::var("MIRROR_GEMINI_INCLUDE_PRERELEASE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallerConfig {
    #[serde(default = "default_installer_enabled")]
    pub enabled: bool,

    #[serde(default = "default_installer_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_installer_platforms")]
    pub platforms: Vec<String>,
}

impl Default for InstallerConfig {
    fn default() -> Self {
        Self {
            enabled: default_installer_enabled(),
            tags: default_installer_tags(),
            platforms: default_installer_platforms(),
        }
    }
}

fn default_installer_enabled() -> bool {
    std::env::var("MIRROR_INSTALLER_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_installer_tags() -> Vec<String> {
    vec!["latest".to_string()]
}

fn default_installer_platforms() -> Vec<String> {
    default_codex_platforms()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default = "default_node_enabled")]
    pub enabled: bool,

    #[serde(default = "default_node_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_node_platforms")]
    pub platforms: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            enabled: default_node_enabled(),
            tags: default_node_tags(),
            platforms: default_node_platforms(),
        }
    }
}

fn default_node_enabled() -> bool {
    std::env::var("MIRROR_NODE_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_node_tags() -> Vec<String> {
    vec!["latest".to_string()]
}

fn default_node_platforms() -> Vec<String> {
    vec![
        "darwin-x64".to_string(),
        "darwin-arm64".to_string(),
        "linux-x64".to_string(),
        "linux-arm64".to_string(),
        "win32-x64".to_string(),
        "win32-arm64".to_string(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePtyConfig {
    #[serde(default = "default_node_pty_enabled")]
    pub enabled: bool,

    #[serde(default = "default_node_pty_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_node_pty_platforms")]
    pub platforms: Vec<String>,
}

impl Default for NodePtyConfig {
    fn default() -> Self {
        Self {
            enabled: default_node_pty_enabled(),
            tags: default_node_pty_tags(),
            platforms: default_node_pty_platforms(),
        }
    }
}

fn default_node_pty_enabled() -> bool {
    std::env::var("MIRROR_NODE_PTY_ENABLED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_node_pty_tags() -> Vec<String> {
    vec!["latest".to_string()]
}

fn default_node_pty_platforms() -> Vec<String> {
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
        assert!(matches!(config.storage.mode, StorageMode::Local));
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

[storage]
mode = "oss"

[storage.oss]
endpoint = "oss-cn-hangzhou.aliyuncs.com"
bucket = "example-bucket"
access_key_id = "test-id"
access_key_secret = "test-secret"
prefix = "mirror"
https = true
path_style = false
expires_seconds = 600

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
        assert!(matches!(config.storage.mode, StorageMode::Oss));
        assert_eq!(config.storage.oss.bucket, "example-bucket");
        assert_eq!(config.storage.oss.expires_seconds, 600);
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

    #[test]
    fn test_default_installer_platforms() {
        let platforms = default_installer_platforms();
        assert!(platforms.contains(&"darwin-arm64".to_string()));
        assert!(platforms.contains(&"linux-x64".to_string()));
        assert!(platforms.contains(&"win32-x64".to_string()));
        assert!(platforms.contains(&"win32-arm64".to_string()));
        assert_eq!(platforms.len(), 8);
    }
}
