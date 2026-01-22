use anyhow::Result;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use std::future::Future;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant, interval};
use tower::ServiceExt;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeFile;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::cache::{CacheManager, ProviderMetadata};
use crate::config::{Config, StorageMode};
use crate::oss;
use crate::providers::{
    ClaudeCodeProvider, CodexProvider, GeminiProvider, InstallerProvider, NodeProvider,
    NodePtyProvider,
};
use crate::s3;
use crate::storage_clients::StorageClients;

/// Shared application state
pub struct AppState {
    pub config: Config,
    pub cache: Arc<CacheManager>,
    pub storage_clients: StorageClients,
    pub claude_code: ClaudeCodeProvider,
    pub codex: CodexProvider,
    pub gemini: GeminiProvider,
    pub installer: InstallerProvider,
    pub node: NodeProvider,
    pub node_pty: NodePtyProvider,
    pub sync_lock: Mutex<()>,
    pub refresh_throttle: Mutex<HashMap<&'static str, Instant>>,
}

async fn build_state(config: Config, cache: Arc<CacheManager>) -> Result<Arc<AppState>> {
    let storage_clients = StorageClients::new(&config.storage).await?;
    let storage = config.storage.clone();
    let http = config.http.clone();
    let claude_code = ClaudeCodeProvider::new(
        config.claude_code.clone(),
        cache.clone(),
        storage.clone(),
        storage_clients.clone(),
        http.clone(),
    )?;
    let codex = CodexProvider::new(
        config.codex.clone(),
        cache.clone(),
        storage.clone(),
        storage_clients.clone(),
        http.clone(),
    )?;
    let gemini = GeminiProvider::new(
        config.gemini.clone(),
        cache.clone(),
        storage.clone(),
        storage_clients.clone(),
        http.clone(),
    )?;
    let installer = InstallerProvider::new(
        config.installer.clone(),
        cache.clone(),
        storage.clone(),
        storage_clients.clone(),
        http.clone(),
    )?;
    let node = NodeProvider::new(
        config.node.clone(),
        cache.clone(),
        storage.clone(),
        storage_clients.clone(),
        http.clone(),
    )?;
    let node_pty = NodePtyProvider::new(
        config.node_pty.clone(),
        cache.clone(),
        storage,
        storage_clients.clone(),
        http,
    )?;

    Ok(Arc::new(AppState {
        config,
        cache,
        storage_clients,
        claude_code,
        codex,
        gemini,
        installer,
        node,
        node_pty,
        sync_lock: Mutex::new(()),
        refresh_throttle: Mutex::new(HashMap::new()),
    }))
}

pub async fn sync_once(config: Config, cache: CacheManager) -> Result<()> {
    let cache = Arc::new(cache);
    let state = build_state(config, cache).await?;
    sync_all_locked(state.as_ref()).await?;
    Ok(())
}

pub async fn run(mut config: Config, cache: CacheManager, skip_initial_sync: bool) -> Result<()> {
    if let Some(public_url) = config.server.public_url.clone() {
        config.server.public_url = Some(crate::config::normalize_public_url(&public_url)?);
    }

    let cache = Arc::new(cache);
    let state = build_state(config.clone(), cache.clone()).await?;

    // Initial sync
    if !skip_initial_sync {
        info!("Performing initial cache sync...");
        if let Err(e) = sync_all_locked(state.as_ref()).await {
            error!("Initial sync failed: {}", e);
        }
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
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                // Only allow cross-origin GET. Refresh endpoints are admin-only and not meant for browsers.
                .allow_methods([Method::GET])
                .allow_headers([header::AUTHORIZATION, header::RANGE]),
        )
        .with_state(state)
}

// Health check
async fn health_check() -> &'static str {
    "OK"
}

async fn serve_storage_file(
    state: &AppState,
    req: Request,
    provider: &str,
    path_segments: &[&str],
    content_type: &'static str,
    filename: Option<&str>,
) -> Result<Response, StatusCode> {
    let key = state
        .cache
        .build_object_key(provider, path_segments)
        .ok_or(StatusCode::NOT_FOUND)?;

    match state.config.storage.mode {
        StorageMode::Local => serve_local_file(state, req, &key, content_type, filename).await,
        StorageMode::Oss => {
            drop(req);
            serve_oss_redirect(state, &key)
        }
        StorageMode::S3 => {
            drop(req);
            serve_s3_redirect(state, &key).await
        }
    }
}

async fn serve_local_file(
    state: &AppState,
    req: Request,
    key: &str,
    content_type: &'static str,
    filename: Option<&str>,
) -> Result<Response, StatusCode> {
    let path = state.cache.config.dir.join(key);
    let response = ServeFile::new(path).oneshot(req).await.map_err(|err| {
        error!("Failed to serve local file: {}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (mut parts, body) = response.into_parts();

    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    if let Some(name) = filename {
        let header_value = content_disposition_header_value(name)?;
        parts
            .headers
            .insert(header::CONTENT_DISPOSITION, header_value);
    }

    Ok(Response::from_parts(parts, Body::new(body)))
}

fn serve_oss_redirect(state: &AppState, key: &str) -> Result<Response, StatusCode> {
    let url = oss::presign_get_url(&state.config.storage.oss, key).map_err(|e| {
        error!("Failed to presign OSS URL: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let location = HeaderValue::from_str(&url).map_err(|e| {
        error!("Failed to build OSS Location header: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .map_err(|err| {
            error!("Failed to build OSS redirect response: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn serve_s3_redirect(state: &AppState, key: &str) -> Result<Response, StatusCode> {
    let client = state.storage_clients.s3().ok_or_else(|| {
        error!("Storage mode is S3 but S3 client is not initialized");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let url = s3::presign_get_url_with_client(client, &state.config.storage.s3, key)
        .await
        .map_err(|e| {
            error!("Failed to presign S3 URL: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let location = HeaderValue::from_str(&url).map_err(|e| {
        error!("Failed to build S3 Location header: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .map_err(|err| {
            error!("Failed to build S3 redirect response: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// Get tag version
async fn claude_code_tag(
    State(state): State<Arc<AppState>>,
    Path(tag): Path<String>,
) -> Result<String, StatusCode> {
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
    req: Request,
) -> Result<Response, StatusCode> {
    serve_storage_file(
        state.as_ref(),
        req,
        "claude-code",
        &["versions", &version, "manifest.json"],
        "application/json",
        None,
    )
    .await
}

// Download binary
async fn claude_code_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let expected_filename = if platform.starts_with("win32") {
        "claude.exe"
    } else {
        "claude"
    };

    if filename != expected_filename {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_storage_file(
        state.as_ref(),
        req,
        "claude-code",
        &["versions", &version, &platform, expected_filename],
        "application/octet-stream",
        Some(expected_filename),
    )
    .await
}

// Download Codex binary/archive
async fn codex_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let expected_filename = state
        .cache
        .with_provider_metadata("codex", |provider| {
            provider
                .versions
                .get(&version)
                .and_then(|version_meta| version_meta.platforms.get(&platform))
                .map(|platform_meta| platform_meta.filename.clone())
        })
        .await
        .flatten()
        .ok_or(StatusCode::NOT_FOUND)?;

    if expected_filename != filename {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_storage_file(
        state.as_ref(),
        req,
        "codex",
        &["versions", &version, &platform, &filename],
        "application/octet-stream",
        Some(&filename),
    )
    .await
}

// Download Gemini CLI JS
async fn gemini_binary(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_storage_file(
        state.as_ref(),
        req,
        "gemini",
        &["versions", &version, "universal", "gemini.js"],
        "application/octet-stream",
        Some("gemini.js"),
    )
    .await
}

// Download installer binary
async fn installer_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let expected_filename = state
        .cache
        .with_provider_metadata("installer", |provider| {
            provider
                .versions
                .get(&version)
                .and_then(|version_meta| version_meta.platforms.get(&platform))
                .map(|platform_meta| platform_meta.filename.clone())
        })
        .await
        .flatten()
        .ok_or(StatusCode::NOT_FOUND)?;

    if expected_filename != filename {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_storage_file(
        state.as_ref(),
        req,
        "installer",
        &["versions", &version, &platform, &filename],
        "application/octet-stream",
        Some(&filename),
    )
    .await
}

// Installer checksum helper
async fn installer_checksum_txt(
    State(state): State<Arc<AppState>>,
    Path((version, platform)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let body = state
        .cache
        .with_provider_metadata("installer", |provider| {
            provider
                .versions
                .get(&version)
                .and_then(|version_meta| version_meta.platforms.get(&platform))
                .map(|platform_meta| {
                    format!("{}  {}\n", platform_meta.sha256, platform_meta.filename)
                })
        })
        .await
        .flatten()
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(([(header::CONTENT_TYPE, "text/plain")], body).into_response())
}

// Download Node.js runtime
async fn node_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let expected_filename = state
        .cache
        .with_provider_metadata("node", |provider| {
            provider
                .versions
                .get(&version)
                .and_then(|version_meta| version_meta.platforms.get(&platform))
                .map(|platform_meta| platform_meta.filename.clone())
        })
        .await
        .flatten()
        .ok_or(StatusCode::NOT_FOUND)?;

    if expected_filename != filename {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_storage_file(
        state.as_ref(),
        req,
        "node",
        &["versions", &version, &platform, &filename],
        "application/octet-stream",
        Some(&filename),
    )
    .await
}

async fn node_checksums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_storage_file(
        state.as_ref(),
        req,
        "node",
        &["versions", &version, "checksums.json"],
        "application/json",
        None,
    )
    .await
}

async fn node_shasums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_storage_file(
        state.as_ref(),
        req,
        "node",
        &["versions", &version, "SHASUMS256.txt"],
        "text/plain",
        None,
    )
    .await
}

// Download node-pty prebuild file
async fn node_pty_binary(
    State(state): State<Arc<AppState>>,
    Path((version, platform, filename)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let allowed = state
        .cache
        .with_provider_metadata("node-pty", |provider| {
            provider
                .versions
                .get(&version)
                .and_then(|version_meta| version_meta.platforms.get(&platform))
                .is_some_and(|platform_meta| {
                    if platform_meta.files.is_empty() {
                        platform_meta.filename == filename
                    } else {
                        platform_meta.files.contains_key(&filename)
                    }
                })
        })
        .await
        .unwrap_or(false);

    if !allowed {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_storage_file(
        state.as_ref(),
        req,
        "node-pty",
        &["versions", &version, "prebuilds", &platform, &filename],
        "application/octet-stream",
        Some(&filename),
    )
    .await
}

async fn node_pty_checksums(
    State(state): State<Arc<AppState>>,
    Path(version): Path<String>,
    req: Request,
) -> Result<Response, StatusCode> {
    serve_storage_file(
        state.as_ref(),
        req,
        "node-pty",
        &["versions", &version, "checksums.json"],
        "application/json",
        None,
    )
    .await
}

// Install script for Linux/macOS
async fn claude_code_install_sh(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_install_sh(&mirror_url);

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Install script for Windows
async fn claude_code_install_ps1(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_install_ps1(&mirror_url);

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
}

// Uninstall script for Linux/macOS
async fn claude_code_uninstall_sh() -> Response {
    let script = generate_uninstall_sh();

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Uninstall script for Windows
async fn claude_code_uninstall_ps1() -> Response {
    let script = generate_uninstall_ps1();

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
}

// Install script for Codex (Linux/macOS)
async fn codex_install_sh(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_codex_install_sh(&mirror_url);

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Install script for Codex (Windows)
async fn codex_install_ps1(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_codex_install_ps1(&mirror_url);

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
}

// Uninstall script for Codex (Linux/macOS)
async fn codex_uninstall_sh() -> Response {
    let script = generate_codex_uninstall_sh();

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Uninstall script for Codex (Windows)
async fn codex_uninstall_ps1() -> Response {
    let script = generate_codex_uninstall_ps1();

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
}

// Install script for Gemini (Linux/macOS)
async fn gemini_install_sh(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_gemini_install_sh(&mirror_url);

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Install script for Gemini (Windows)
async fn gemini_install_ps1(State(state): State<Arc<AppState>>) -> Response {
    let Some(mirror_url) = state.config.server.public_url.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response();
    };

    let script = generate_gemini_install_ps1(&mirror_url);

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
}

// Uninstall script for Gemini (Linux/macOS)
async fn gemini_uninstall_sh() -> Response {
    let script = generate_gemini_uninstall_sh();

    ([(header::CONTENT_TYPE, "text/x-shellscript")], script).into_response()
}

// Uninstall script for Gemini (Windows)
async fn gemini_uninstall_ps1() -> Response {
    let script = generate_gemini_uninstall_ps1();

    ([(header::CONTENT_TYPE, "text/plain")], script).into_response()
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
    let checksums = state
        .cache
        .with_provider_metadata("claude-code", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh cache
async fn api_claude_code_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "claude-code").await?;
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
    let checksums = state
        .cache
        .with_provider_metadata("codex", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh Codex cache
async fn api_codex_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "codex").await?;
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
    let checksums = state
        .cache
        .with_provider_metadata("gemini", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh Gemini cache
async fn api_gemini_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "gemini").await?;
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
    let checksums = state
        .cache
        .with_provider_metadata("installer", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh installer cache
async fn api_installer_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "installer").await?;
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
    let checksums = state
        .cache
        .with_provider_metadata("node", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh Node cache
async fn api_node_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "node").await?;
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
    let checksums = state
        .cache
        .with_provider_metadata("node-pty", provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Json(checksums)
}

// API: Refresh node-pty cache
async fn api_node_pty_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), "node-pty").await?;
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

fn truncate_for_metadata(mut value: String, max_len: usize) -> String {
    if value.len() <= max_len {
        return value;
    }
    value.truncate(max_len);
    value.push_str("...");
    value
}

fn summarize_sync_error(err: &anyhow::Error) -> String {
    // Keep it compact for JSON responses.
    let summary = format!("{:#}", err)
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_for_metadata(summary, 500)
}

async fn sync_provider_with_status<T, Fut, F>(
    state: &AppState,
    provider: &'static str,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started_at = Utc::now();
    state
        .cache
        .update_provider_metadata(provider, |meta| {
            meta.sync.last_started_at = Some(started_at);
        })
        .await?;

    let started = Instant::now();
    let result = f().await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let finished_at = Utc::now();

    match &result {
        Ok(_) => {
            state
                .cache
                .update_provider_metadata(provider, |meta| {
                    meta.sync.last_success_at = Some(finished_at);
                    meta.sync.last_duration_ms = Some(duration_ms);
                    meta.sync.last_error = None;
                })
                .await?;
        }
        Err(err) => {
            let summary = summarize_sync_error(err);
            state
                .cache
                .update_provider_metadata(provider, |meta| {
                    meta.sync.last_failure_at = Some(finished_at);
                    meta.sync.last_duration_ms = Some(duration_ms);
                    meta.sync.last_error = Some(summary);
                })
                .await?;
        }
    }

    result
}

async fn sync_claude_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "claude-code", || state.claude_code.sync_all()).await
}

async fn sync_codex_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "codex", || state.codex.sync_all()).await
}

async fn sync_gemini_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "gemini", || state.gemini.sync_all()).await
}

async fn sync_installer_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "installer", || state.installer.sync_all()).await
}

async fn sync_node_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "node", || state.node.sync_all()).await
}

async fn sync_node_pty_locked(state: &AppState) -> Result<Vec<String>> {
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, "node-pty", || state.node_pty.sync_all()).await
}

async fn sync_all_locked(state: &AppState) -> Result<()> {
    let _guard = state.sync_lock.lock().await;
    let mut errors = Vec::new();

    if let Err(e) =
        sync_provider_with_status(state, "claude-code", || state.claude_code.sync_all()).await
    {
        errors.push(format!("claude-code: {}", e));
    }
    if let Err(e) = sync_provider_with_status(state, "codex", || state.codex.sync_all()).await {
        errors.push(format!("codex: {}", e));
    }
    if let Err(e) = sync_provider_with_status(state, "gemini", || state.gemini.sync_all()).await {
        errors.push(format!("gemini: {}", e));
    }
    if let Err(e) =
        sync_provider_with_status(state, "installer", || state.installer.sync_all()).await
    {
        errors.push(format!("installer: {}", e));
    }
    if let Err(e) = sync_provider_with_status(state, "node", || state.node.sync_all()).await {
        errors.push(format!("node: {}", e));
    }
    if let Err(e) = sync_provider_with_status(state, "node-pty", || state.node_pty.sync_all()).await
    {
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

fn require_refresh_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = state.config.server.refresh_token.as_deref() else {
        warn!("Refresh endpoint called but server.refresh_token is not configured");
        return Err(StatusCode::FORBIDDEN);
    };

    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if token == expected {
        Ok(())
    } else {
        warn!("Unauthorized refresh attempt");
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn check_refresh_throttle(
    state: &AppState,
    provider: &'static str,
) -> Result<(), StatusCode> {
    let min_secs = state.config.server.refresh_min_interval_seconds;
    if min_secs == 0 {
        return Ok(());
    }

    let interval = Duration::from_secs(min_secs);
    let now = Instant::now();
    let mut guard = state.refresh_throttle.lock().await;

    if let Some(last) = guard.get(provider) {
        if now.duration_since(*last) < interval {
            warn!("Refresh throttled for provider {}", provider);
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    guard.insert(provider, now);
    Ok(())
}

fn sanitize_filename_for_header(filename: &str) -> String {
    // Keep it conservative to avoid header injection/panic:
    // allow [A-Za-z0-9._-], replace others with '_'.
    let mut out = String::with_capacity(filename.len());
    for ch in filename.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "download".to_string()
    } else {
        out
    }
}

fn content_disposition_header_value(filename: &str) -> Result<HeaderValue, StatusCode> {
    let safe = sanitize_filename_for_header(filename);
    let value = format!("attachment; filename=\"{}\"", safe);
    HeaderValue::from_str(&value).map_err(|err| {
        error!("Failed to build Content-Disposition header: {}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })
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
