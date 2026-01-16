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

use crate::cache::{CacheManager, ProviderMetadata};
use crate::config::Config;
use crate::providers::{
    ClaudeCodeProvider, CodexProvider, GeminiProvider, InstallerProvider, NodeProvider,
    NodePtyProvider,
};

/// Shared application state
pub struct AppState {
    pub config: Config,
    pub cache: Arc<CacheManager>,
    pub claude_code: ClaudeCodeProvider,
    pub codex: CodexProvider,
    pub gemini: GeminiProvider,
    pub installer: InstallerProvider,
    pub node: NodeProvider,
    pub node_pty: NodePtyProvider,
    pub sync_lock: Mutex<()>,
}

pub async fn run(config: Config, cache: CacheManager) -> Result<()> {
    let cache = Arc::new(cache);

    // Create providers
    let claude_code = ClaudeCodeProvider::new(config.claude_code.clone(), cache.clone());
    let codex = CodexProvider::new(config.codex.clone(), cache.clone());
    let gemini = GeminiProvider::new(config.gemini.clone(), cache.clone());
    let installer = InstallerProvider::new(config.installer.clone(), cache.clone());
    let node = NodeProvider::new(config.node.clone(), cache.clone());
    let node_pty = NodePtyProvider::new(config.node_pty.clone(), cache.clone());

    let state = Arc::new(AppState {
        config: config.clone(),
        cache: cache.clone(),
        claude_code,
        codex,
        gemini,
        installer,
        node,
        node_pty,
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
        // Gemini routes
        .route("/gemini/{tag}", get(gemini_tag))
        .route("/gemini/{version}/gemini.js", get(gemini_binary))
        .route("/gemini/install.sh", get(gemini_install_sh))
        .route("/gemini/install.ps1", get(gemini_install_ps1))
        .route("/gemini/uninstall.sh", get(gemini_uninstall_sh))
        .route("/gemini/uninstall.ps1", get(gemini_uninstall_ps1))
        // Installer routes
        .route("/installer/{tag}", get(installer_tag))
        .route(
            "/installer/{version}/{platform}/{filename}",
            get(installer_binary),
        )
        .route(
            "/installer/{version}/{platform}/checksum.txt",
            get(installer_checksum_txt),
        )
        // Node runtime routes
        .route("/node/{tag}", get(node_tag))
        .route("/node/{version}/{platform}/{filename}", get(node_binary))
        .route("/node/{version}/checksums.json", get(node_checksums))
        .route("/node/{version}/SHASUMS256.txt", get(node_shasums))
        // node-pty routes
        .route("/node-pty/{tag}", get(node_pty_tag))
        .route(
            "/node-pty/{version}/prebuilds/{platform}/{filename}",
            get(node_pty_binary),
        )
        .route(
            "/node-pty/{version}/checksums.json",
            get(node_pty_checksums),
        )
        // API routes
        .route("/api/claude-code/info", get(api_claude_code_info))
        .route("/api/claude-code/versions", get(api_claude_code_versions))
        .route("/api/claude-code/checksums", get(api_claude_code_checksums))
        .route("/api/claude-code/refresh", post(api_claude_code_refresh))
        .route("/api/codex/info", get(api_codex_info))
        .route("/api/codex/versions", get(api_codex_versions))
        .route("/api/codex/checksums", get(api_codex_checksums))
        .route("/api/codex/refresh", post(api_codex_refresh))
        .route("/api/gemini/info", get(api_gemini_info))
        .route("/api/gemini/versions", get(api_gemini_versions))
        .route("/api/gemini/checksums", get(api_gemini_checksums))
        .route("/api/gemini/refresh", post(api_gemini_refresh))
        .route("/api/installer/info", get(api_installer_info))
        .route("/api/installer/versions", get(api_installer_versions))
        .route("/api/installer/checksums", get(api_installer_checksums))
        .route("/api/installer/refresh", post(api_installer_refresh))
        .route("/api/node/info", get(api_node_info))
        .route("/api/node/versions", get(api_node_versions))
        .route("/api/node/checksums", get(api_node_checksums))
        .route("/api/node/refresh", post(api_node_refresh))
        .route("/api/node-pty/info", get(api_node_pty_info))
        .route("/api/node-pty/versions", get(api_node_pty_versions))
        .route("/api/node-pty/checksums", get(api_node_pty_checksums))
        .route("/api/node-pty/refresh", post(api_node_pty_refresh))
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

// Get Gemini tag version
async fn gemini_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    if tag != "stable" && tag != "latest" {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .gemini
        .get_tag_version(&tag)
        .await
        .ok_or(StatusCode::NOT_FOUND)
}

// Get Installer tag version
async fn installer_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    if tag != "stable" && tag != "latest" {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .installer
        .get_tag_version(&tag)
        .await
        .ok_or(StatusCode::NOT_FOUND)
}

// Get Node.js tag version
async fn node_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    if tag != "stable" && tag != "latest" {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .node
        .get_tag_version(&tag)
        .await
        .ok_or(StatusCode::NOT_FOUND)
}

// Get node-pty tag version
async fn node_pty_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
    if tag != "stable" && tag != "latest" {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .node_pty
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

// Download Gemini CLI JS
async fn gemini_binary(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
) -> Result<Response, StatusCode> {
    let path = state
        .cache
        .get_file_path("gemini", &["versions", &version, "universal", "gemini.js"])
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
            "attachment; filename=\"gemini.js\"",
        )
        .body(body)
        .unwrap())
}

// Download installer binary
async fn installer_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.installer;
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
        .get_file_path("installer", &["versions", &version, &platform, &filename])
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

// Installer checksum helper
async fn installer_checksum_txt(
    State(state): State<Arc<AppState>>,
    Path((version, platform)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.installer;
    let version_meta = provider
        .versions
        .get(&version)
        .ok_or(StatusCode::NOT_FOUND)?;
    let platform_meta = version_meta
        .platforms
        .get(&platform)
        .ok_or(StatusCode::NOT_FOUND)?;

    let body = format!("{}  {}\n", platform_meta.sha256, platform_meta.filename);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(body))
        .unwrap())
}

// Download Node.js runtime
async fn node_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.node;
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
        .get_file_path("node", &["versions", &version, &platform, &filename])
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

async fn node_checksums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
) -> Result<Response, StatusCode> {
    let path = state
        .cache
        .get_file_path("node", &["versions", &version, "checksums.json"])
        .ok_or(StatusCode::NOT_FOUND)?;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(content))
        .unwrap())
}

async fn node_shasums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
) -> Result<Response, StatusCode> {
    let path = state
        .cache
        .get_file_path("node", &["versions", &version, "SHASUMS256.txt"])
        .ok_or(StatusCode::NOT_FOUND)?;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(content))
        .unwrap())
}

// Download node-pty prebuild file
async fn node_pty_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    let metadata = state.cache.get_metadata().await;
    let provider = &metadata.node_pty;
    let version_meta = provider
        .versions
        .get(&version)
        .ok_or(StatusCode::NOT_FOUND)?;
    let platform_meta = version_meta
        .platforms
        .get(&platform)
        .ok_or(StatusCode::NOT_FOUND)?;

    let allowed = if platform_meta.files.is_empty() {
        platform_meta.filename == filename
    } else {
        platform_meta.files.contains_key(&filename)
    };
    if !allowed {
        return Err(StatusCode::NOT_FOUND);
    }

    let path = state
        .cache
        .get_file_path(
            "node-pty",
            &["versions", &version, "prebuilds", &platform, &filename],
        )
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

async fn node_pty_checksums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
) -> Result<Response, StatusCode> {
    let path = state
        .cache
        .get_file_path("node-pty", &["versions", &version, "checksums.json"])
        .ok_or(StatusCode::NOT_FOUND)?;

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(content))
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

// Install script for Gemini (Linux/macOS)
async fn gemini_install_sh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_gemini_install_sh(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Install script for Gemini (Windows)
async fn gemini_install_ps1(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("server.public_url is not configured"))
            .unwrap();
    };

    let script = generate_gemini_install_ps1(&mirror_url);

    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Gemini (Linux/macOS)
async fn gemini_uninstall_sh() -> impl IntoResponse {
    let script = generate_gemini_uninstall_sh();

    Response::builder()
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .body(Body::from(script))
        .unwrap()
}

// Uninstall script for Gemini (Windows)
async fn gemini_uninstall_ps1() -> impl IntoResponse {
    let script = generate_gemini_uninstall_ps1();

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
    Json(provider_checksums_json(&metadata.claude_code))
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
    Json(provider_checksums_json(&metadata.codex))
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

// API: Gemini info
async fn api_gemini_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.gemini.get_info().await)
}

// API: Gemini versions
async fn api_gemini_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("gemini").await)
}

// API: Gemini checksums
async fn api_gemini_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    Json(provider_checksums_json(&metadata.gemini))
}

// API: Refresh Gemini cache
async fn api_gemini_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_gemini_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("Gemini refresh failed: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// API: Installer info
async fn api_installer_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.installer.get_info().await)
}

// API: Installer versions
async fn api_installer_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("installer").await)
}

// API: Installer checksums
async fn api_installer_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    Json(provider_checksums_json(&metadata.installer))
}

// API: Refresh installer cache
async fn api_installer_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_installer_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("Installer refresh failed: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// API: Node info
async fn api_node_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.node.get_info().await)
}

// API: Node versions
async fn api_node_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("node").await)
}

// API: Node checksums
async fn api_node_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    Json(provider_checksums_json(&metadata.node))
}

// API: Refresh Node cache
async fn api_node_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_node_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("Node refresh failed: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// API: node-pty info
async fn api_node_pty_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(state.node_pty.get_info().await)
}

// API: node-pty versions
async fn api_node_pty_versions(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.cache.list_versions("node-pty").await)
}

// API: node-pty checksums
async fn api_node_pty_checksums(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let metadata = state.cache.get_metadata().await;
    Json(provider_checksums_json(&metadata.node_pty))
}

// API: Refresh node-pty cache
async fn api_node_pty_refresh(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sync_node_pty_locked(state.as_ref()).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("node-pty refresh failed: {}", e);
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

async fn sync_gemini_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.gemini.sync_all().await
}

async fn sync_installer_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.installer.sync_all().await
}

async fn sync_node_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.node.sync_all().await
}

async fn sync_node_pty_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    state.node_pty.sync_all().await
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
    if let Err(e) = state.gemini.sync_all().await {
        errors.push(format!("gemini: {}", e));
    }
    if let Err(e) = state.installer.sync_all().await {
        errors.push(format!("installer: {}", e));
    }
    if let Err(e) = state.node.sync_all().await {
        errors.push(format!("node: {}", e));
    }
    if let Err(e) = state.node_pty.sync_all().await {
        errors.push(format!("node-pty: {}", e));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

fn provider_checksums_json(provider: &ProviderMetadata) -> serde_json::Value {
    let mut checksums = serde_json::Map::new();

    for (version, version_meta) in &provider.versions {
        let mut platforms = serde_json::Map::new();
        for (platform, platform_meta) in &version_meta.platforms {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "sha256".to_string(),
                serde_json::Value::String(platform_meta.sha256.clone()),
            );
            entry.insert(
                "size".to_string(),
                serde_json::Value::Number(serde_json::Number::from(platform_meta.size)),
            );
            entry.insert(
                "filename".to_string(),
                serde_json::Value::String(platform_meta.filename.clone()),
            );

            if !platform_meta.files.is_empty() {
                if let Ok(files_value) = serde_json::to_value(&platform_meta.files) {
                    entry.insert("files".to_string(), files_value);
                }
            }

            platforms.insert(platform.clone(), serde_json::Value::Object(entry));
        }
        checksums.insert(version.clone(), serde_json::Value::Object(platforms));
    }

    serde_json::Value::Object(checksums)
}

fn inject_mirror_url_sh(script: &str, mirror_url: &str) -> String {
    let marker = r#"MIRROR_URL="${MIRROR_URL:-__MIRROR_URL__}""#;
    if script.contains(marker) {
        script.replacen(
            marker,
            &format!(r#"MIRROR_URL="${{MIRROR_URL:-{}}}""#, mirror_url),
            1,
        )
    } else {
        script.to_string()
    }
}

fn inject_mirror_url_ps1(script: &str, mirror_url: &str) -> String {
    let marker = r#"$MirrorUrl = "__MIRROR_URL__""#;
    if script.contains(marker) {
        script.replacen(marker, &format!(r#"$MirrorUrl = "{}""#, mirror_url), 1)
    } else {
        script.to_string()
    }
}

// Generate install.sh script
fn generate_install_sh(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/claude-code-install.sh");
    inject_mirror_url_sh(SCRIPT, mirror_url)
}

// Generate install.ps1 script
fn generate_install_ps1(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/claude-code-install.ps1");
    inject_mirror_url_ps1(SCRIPT, mirror_url)
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
    inject_mirror_url_sh(SCRIPT, mirror_url)
}

fn generate_codex_install_ps1(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/codex-install.ps1");
    inject_mirror_url_ps1(SCRIPT, mirror_url)
}

fn generate_codex_uninstall_sh() -> String {
    include_str!("../scripts/codex-uninstall.sh").to_string()
}

fn generate_codex_uninstall_ps1() -> String {
    include_str!("../scripts/codex-uninstall.ps1").to_string()
}

fn generate_gemini_install_sh(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/gemini-install.sh");
    inject_mirror_url_sh(SCRIPT, mirror_url)
}

fn generate_gemini_install_ps1(mirror_url: &str) -> String {
    const SCRIPT: &str = include_str!("../scripts/gemini-install.ps1");
    inject_mirror_url_ps1(SCRIPT, mirror_url)
}

fn generate_gemini_uninstall_sh() -> String {
    include_str!("../scripts/gemini-uninstall.sh").to_string()
}

fn generate_gemini_uninstall_ps1() -> String {
    include_str!("../scripts/gemini-uninstall.ps1").to_string()
}
