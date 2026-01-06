use anyhow::Result;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};
use tokio_util::io::ReaderStream;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::cache::CacheManager;
use crate::config::Config;
use crate::providers::{ClaudeCodeProvider, CodexProvider};

/// Shared application state
pub struct AppState {
    pub config: Config,
    pub cache: Arc<CacheManager>,
    pub claude_code: ClaudeCodeProvider,
    pub codex: CodexProvider,
    pub sync_lock: Mutex<()>,
}

pub async fn run(config: Config, cache: CacheManager) -> Result<()> {
    let cache = Arc::new(cache);

    // Create providers
    let claude_code = ClaudeCodeProvider::new(config.claude_code.clone(), cache.clone());
    let codex = CodexProvider::new(config.codex.clone(), cache.clone());

    let state = Arc::new(AppState {
        config: config.clone(),
        cache: cache.clone(),
        claude_code,
        codex,
        sync_lock: Mutex::new(()),
    });

    // Initial sync
    info!("Performing initial cache sync...");
    if let Err(e) = sync_all_locked(state.as_ref()).await {
        error!("Initial sync failed: {}", e);
    }

    // Start background update task
    if config.update.enabled {
        let update_state = state.clone();
        let interval_minutes = config.update.interval_minutes;
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_minutes * 60));
            // Consume the immediate tick to avoid double sync on startup.
            interval.tick().await;
            loop {
                interval.tick().await;
                info!("Running scheduled cache update...");
                if let Err(e) = sync_all_locked(update_state.as_ref()).await {
                    error!("Scheduled sync failed: {}", e);
                }
            }
        });
    }

    if config.server.public_url.is_none() {
        warn!("server.public_url is not set; install scripts will return 503");
    }

    // Build router
    let app = build_router(state);

    // Start server
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(health_check))
        // Claude Code routes
        .route("/claude-code/{tag}", get(claude_code_tag))
        .route(
            "/claude-code/{version}/manifest.json",
            get(claude_code_manifest),
        )
        .route(
            "/claude-code/{version}/{platform}/{filename}",
            get(claude_code_binary),
        )
        .route("/claude-code/install.sh", get(claude_code_install_sh))
        .route("/claude-code/install.ps1", get(claude_code_install_ps1))
        .route("/claude-code/uninstall.sh", get(claude_code_uninstall_sh))
        .route("/claude-code/uninstall.ps1", get(claude_code_uninstall_ps1))
        // Codex routes
        .route("/codex/{tag}", get(codex_tag))
        .route("/codex/{version}/{platform}/{filename}", get(codex_binary))
        .route("/codex/install.sh", get(codex_install_sh))
        .route("/codex/install.ps1", get(codex_install_ps1))
        .route("/codex/uninstall.sh", get(codex_uninstall_sh))
        .route("/codex/uninstall.ps1", get(codex_uninstall_ps1))
        // API routes
        .route("/api/claude-code/info", get(api_claude_code_info))
        .route("/api/claude-code/versions", get(api_claude_code_versions))
        .route("/api/claude-code/checksums", get(api_claude_code_checksums))
        .route("/api/claude-code/refresh", post(api_claude_code_refresh))
        .route("/api/codex/info", get(api_codex_info))
        .route("/api/codex/versions", get(api_codex_versions))
        .route("/api/codex/checksums", get(api_codex_checksums))
        .route("/api/codex/refresh", post(api_codex_refresh))
        // Add middleware
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
        .with_state(state)
}

// Health check
async fn health_check() -> &'static str {
    "OK"
}

// Get tag version
async fn claude_code_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    // Only allow known tags
    if tag != "stable" && tag != "latest" {
        // Try to parse as version number
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .claude_code
        .get_tag_version(&tag)
        .await
        .ok_or(StatusCode::NOT_FOUND)
}

// Get Codex tag version
async fn codex_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    if tag != "stable" && tag != "latest" {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .codex
        .get_tag_version(&tag)
        .await
        .ok_or(StatusCode::NOT_FOUND)
}

// Get manifest
async fn claude_code_manifest(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
) -> Result<Response, StatusCode> {
    let path = state
        .cache
        .get_file_path("claude-code", &["versions", &version, "manifest.json"])
        .ok_or(StatusCode::NOT_FOUND)?;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(content))
        .unwrap())
}

// Download binary
async fn claude_code_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    let expected_filename = if platform.starts_with("win32") {
        "claude.exe"
    } else {
        "claude"
    };

    if filename != expected_filename {
        return Err(StatusCode::NOT_FOUND);
    }

    let path = state
        .cache
        .get_file_path(
            "claude-code",
            &["versions", &version, &platform, expected_filename],
        )
        .ok_or(StatusCode::NOT_FOUND)?;

    // Open file and stream it
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let metadata = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let content_type = "application/octet-stream";

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(body)
        .unwrap())
}

// Download Codex binary/archive
async fn codex_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.codex;
    let version_meta = provider
        .versions
        .get(&version)
        .ok_or(StatusCode::NOT_FOUND)?;
    let platform_meta = version_meta
        .platforms
        .get(&platform)
        .ok_or(StatusCode::NOT_FOUND)?;

    if platform_meta.filename != filename {
        return Err(StatusCode::NOT_FOUND);
    }

    let path = state
        .cache
        .get_file_path("codex", &["versions", &version, &platform, &filename])
        .ok_or(StatusCode::NOT_FOUND)?;

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let metadata = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(body)
        .unwrap())
}

// Install script for Linux/macOS
async fn claude_code_install_sh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_install_sh(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Install script for Windows
async fn claude_code_install_ps1(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_install_ps1(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Linux/macOS
async fn claude_code_uninstall_sh() -> impl IntoResponse {
    let script = generate_uninstall_sh();

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Windows
async fn claude_code_uninstall_ps1() -> impl IntoResponse {
    let script = generate_uninstall_ps1();

    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(script))
        .unwrap()
}

// Install script for Codex (Linux/macOS)
async fn codex_install_sh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_codex_install_sh(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Install script for Codex (Windows)
async fn codex_install_ps1(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_codex_install_ps1(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Codex (Linux/macOS)
async fn codex_uninstall_sh() -> impl IntoResponse {
    let script = generate_codex_uninstall_sh();

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Codex (Windows)
async fn codex_uninstall_ps1() -> impl IntoResponse {
    let script = generate_codex_uninstall_ps1();

    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(script))
        .unwrap()
}

// API: Get info
async fn api_claude_code_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.claude_code.get_info().await)
}

// API: List versions
async fn api_claude_code_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("claude-code").await)
}

// API: Get checksums for all versions and platforms
async fn api_claude_code_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.claude_code;

    let mut checksums = serde_json::Map::new();

    for (version, version_meta) in &provider.versions {
        let mut platforms = serde_json::Map::new();
        for (platform, platform_meta) in &version_meta.platforms {
            platforms.insert(
                platform.clone(),
                serde_json::json!({
                    "sha256": platform_meta.sha256,
                    "size": platform_meta.size,
                    "filename": platform_meta.filename
                }),
            );
        }
        checksums.insert(version.clone(), serde_json::Value::Object(platforms));
    }

    Json(serde_json::Value::Object(checksums))
}

// API: Refresh cache
async fn api_claude_code_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_claude_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("Refresh failed: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// API: Codex info
async fn api_codex_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.codex.get_info().await)
}

// API: Codex versions
async fn api_codex_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("codex").await)
}

// API: Codex checksums
async fn api_codex_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.codex;

    let mut checksums = serde_json::Map::new();

    for (version, version_meta) in &provider.versions {
        let mut platforms = serde_json::Map::new();
        for (platform, platform_meta) in &version_meta.platforms {
            platforms.insert(
                platform.clone(),
                serde_json::json!({
                    "sha256": platform_meta.sha256,
                    "size": platform_meta.size,
                    "filename": platform_meta.filename
                }),
            );
        }
        checksums.insert(version.clone(), serde_json::Value::Object(platforms));
    }

    Json(serde_json::Value::Object(checksums))
}

// API: Refresh Codex cache
async fn api_codex_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_codex_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("Codex refresh failed: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn sync_claude_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.claude_code.sync_all().await
}

async fn sync_codex_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.codex.sync_all().await
}

async fn sync_all_locked(state: &AppState) -> Result<()> {
    let _guard = state.sync_lock.lock().await;
    let mut errors = Vec::new();

    if let Err(e) = state.claude_code.sync_all().await {
        errors.push(format!("claude-code: {}", e));
    }
    if let Err(e) = state.codex.sync_all().await {
        errors.push(format!("codex: {}", e));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

// Generate install.sh script
fn generate_install_sh(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/claude-code-install.sh");
    SCRIPT.replace("__MIRROR_URL__", mirror_url)
}

// Generate install.ps1 script
fn generate_install_ps1(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/claude-code-install.ps1");
    SCRIPT.replace("__MIRROR_URL__", mirror_url)
}

// Generate uninstall.sh script
fn generate_uninstall_sh() -> String {
    include_str!("../scripts/claude-code-uninstall.sh").to_string()
}

// Generate uninstall.ps1 script
fn generate_uninstall_ps1() -> String {
    include_str!("../scripts/claude-code-uninstall.ps1").to_string()
}

fn generate_codex_install_sh(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/codex-install.sh");
    SCRIPT.replace("__MIRROR_URL__", mirror_url)
}

fn generate_codex_install_ps1(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/codex-install.ps1");
    SCRIPT.replace("__MIRROR_URL__", mirror_url)
}

fn generate_codex_uninstall_sh() -> String {
    include_str!("../scripts/codex-uninstall.sh").to_string()
}

fn generate_codex_uninstall_ps1() -> String {
    include_str!("../scripts/codex-uninstall.ps1").to_string()
}
