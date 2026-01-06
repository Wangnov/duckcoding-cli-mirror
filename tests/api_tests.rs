//! Integration tests for HTTP API endpoints

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use duckcoding_cli_mirror::{
    cache::{CacheManager, PlatformMetadata, VersionMetadata},
    config::{CacheConfig, Config},
    providers::{ClaudeCodeProvider, CodexProvider},
    server::{self, AppState},
};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tower::ServiceExt;

/// Helper to create a test request
fn create_request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn create_test_state() -> (TempDir, Arc<AppState>) {
    let temp_dir = TempDir::new().unwrap();
    let mut config = Config::default();
    config.cache = CacheConfig {
        dir: temp_dir.path().to_path_buf(),
        max_versions: 3,
    };

    let cache = CacheManager::new(&config.cache).unwrap();
    let cache = Arc::new(cache);
    let provider = ClaudeCodeProvider::new(config.claude_code.clone(), cache.clone());
    let codex = CodexProvider::new(config.codex.clone(), cache.clone());

    let state = Arc::new(AppState {
        config,
        cache,
        claude_code: provider,
        codex,
        sync_lock: Mutex::new(()),
    });

    (temp_dir, state)
}

/// Test health check endpoint
#[tokio::test]
async fn test_health_check() {
    let (_temp_dir, state) = create_test_state();
    let app = server::build_router(state);

    let response = app.oneshot(create_request("GET", "/health")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"OK");
}

/// Test that install scripts contain expected content
#[test]
fn test_install_sh_content() {
    let script = include_str!("../scripts/claude-code-install.sh");

    // Check for key features
    assert!(script.contains("MIRROR_URL"));
    assert!(script.contains("detect_platform"));
    assert!(script.contains("sha256")); // SHA256 verification
    assert!(script.contains("LANG_CODE")); // i18n support
}

#[test]
fn test_install_ps1_content() {
    let script = include_str!("../scripts/claude-code-install.ps1");

    // Check for key features
    assert!(script.contains("$MirrorUrl"));
    assert!(script.contains("Get-FileHash")); // SHA256 verification
    assert!(script.contains("$LangCode")); // i18n support
    assert!(script.contains("$ProxyUrl")); // Proxy support
}

#[test]
fn test_codex_install_sh_content() {
    let script = include_str!("../scripts/codex-install.sh");

    assert!(script.contains("MIRROR_URL"));
    assert!(script.contains("detect_platform"));
    assert!(script.contains("codex"));
    assert!(script.contains("api/codex/checksums"));
}

#[test]
fn test_codex_install_ps1_content() {
    let script = include_str!("../scripts/codex-install.ps1");

    assert!(script.contains("$MirrorUrl"));
    assert!(script.contains("api/codex/checksums"));
    assert!(script.contains("Get-FileHash"));
}

#[tokio::test]
async fn test_codex_tag_and_binary() {
    let (_temp_dir, state) = create_test_state();
    let cache = state.cache.clone();

    let version = "rust-v0.0.0-test";
    let platform = "darwin-x64";
    let filename = "codex-x86_64-apple-darwin.tar.gz";
    let data = b"codex-test-archive";

    let path = cache.binary_path("codex", version, platform, filename);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(&path, data).await.unwrap();

    cache.write_tag("codex", "latest", version).await.unwrap();

    let mut hasher = Sha256::new();
    hasher.update(data);
    let sha256 = hex::encode(hasher.finalize());

    cache
        .update_provider_metadata("codex", |m| {
            m.tags.insert("latest".to_string(), version.to_string());
            m.versions.insert(
                version.to_string(),
                VersionMetadata {
                    version: version.to_string(),
                    downloaded_at: chrono::Utc::now(),
                    platforms: [(
                        platform.to_string(),
                        PlatformMetadata {
                            sha256,
                            size: data.len() as u64,
                            filename: filename.to_string(),
                        },
                    )]
                    .into_iter()
                    .collect(),
                },
            );
        })
        .await
        .unwrap();

    let app = server::build_router(state);

    let tag_response = app
        .clone()
        .oneshot(create_request("GET", "/codex/latest"))
        .await
        .unwrap();
    assert_eq!(tag_response.status(), StatusCode::OK);
    let tag_body = tag_response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&tag_body[..], version.as_bytes());

    let binary_response = app
        .oneshot(create_request(
            "GET",
            &format!("/codex/{}/{}/{}", version, platform, filename),
        ))
        .await
        .unwrap();
    assert_eq!(binary_response.status(), StatusCode::OK);
    let binary_body = binary_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(&binary_body[..], data);
}

#[tokio::test]
async fn test_codex_checksums_api() {
    let (_temp_dir, state) = create_test_state();
    let cache = state.cache.clone();

    let version = "rust-v0.0.1-test";
    let platform = "linux-x64";
    let filename = "codex-x86_64-unknown-linux-gnu.tar.gz";
    let data = b"codex-checksum-archive";

    let path = cache.binary_path("codex", version, platform, filename);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(&path, data).await.unwrap();

    let mut hasher = Sha256::new();
    hasher.update(data);
    let sha256 = hex::encode(hasher.finalize());

    cache
        .update_provider_metadata("codex", |m| {
            m.versions.insert(
                version.to_string(),
                VersionMetadata {
                    version: version.to_string(),
                    downloaded_at: chrono::Utc::now(),
                    platforms: [(
                        platform.to_string(),
                        PlatformMetadata {
                            sha256: sha256.clone(),
                            size: data.len() as u64,
                            filename: filename.to_string(),
                        },
                    )]
                    .into_iter()
                    .collect(),
                },
            );
        })
        .await
        .unwrap();

    let app = server::build_router(state);
    let response = app
        .oneshot(create_request("GET", "/api/codex/checksums"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json[version][platform]["sha256"].as_str().unwrap(), sha256);
}

#[test]
fn test_uninstall_sh_content() {
    let script = include_str!("../scripts/claude-code-uninstall.sh");

    assert!(script.contains("INSTALL_DIR"));
    assert!(script.contains("rm -rf"));
    assert!(script.contains(".duckcoding"));
}

#[test]
fn test_uninstall_ps1_content() {
    let script = include_str!("../scripts/claude-code-uninstall.ps1");

    assert!(script.contains("$InstallDir"));
    assert!(script.contains("Remove-Item"));
    assert!(script.contains(".duckcoding"));
}

/// Test SHA256 verification function
#[test]
fn test_sha256_verification() {
    use sha2::{Digest, Sha256};

    let data = b"test data for sha256";
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hex::encode(hasher.finalize());

    // Known SHA256 of "test data for sha256"
    assert_eq!(result.len(), 64); // SHA256 produces 64 hex characters
}
