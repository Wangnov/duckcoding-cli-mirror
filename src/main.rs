use anyhow::Result;
use clap::Parser;
use duckcoding_cli_mirror::{cache, config, server};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "duckcoding-cli-mirror")]
#[command(about = "A mirror service for CLI tools (Claude Code, Codex, Gemini CLI)")]
struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Override port
    #[arg(short, long)]
    port: Option<u16>,

    /// Override host
    #[arg(long)]
    host: Option<String>,

    /// Refresh cache on startup
    #[arg(long)]
    refresh: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "duckcoding_cli_mirror=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();

    // Load configuration
    let mut config = config::Config::load(&args.config)?;

    // Override with CLI args
    if let Some(port) = args.port {
        config.server.port = port;
    }
    if let Some(host) = args.host {
        config.server.host = host;
    }

    info!(
        "Starting DuckCoding CLI Mirror on {}:{}",
        config.server.host, config.server.port
    );

    // Optionally refresh cache on startup
    let mut skip_initial_sync = false;
    if args.refresh {
        info!("Refreshing cache on startup...");
        let refresh_cache = cache::CacheManager::new(&config.cache)?;
        match server::sync_once(config.clone(), refresh_cache).await {
            Ok(()) => skip_initial_sync = true,
            Err(e) => {
                error!("Refresh failed, continuing with normal startup: {}", e);
            }
        }
    }

    // Initialize cache manager for server runtime
    let cache_manager = cache::CacheManager::new(&config.cache)?;

    // Start the HTTP server
    server::run(config, cache_manager, skip_initial_sync).await?;

    Ok(())
}
