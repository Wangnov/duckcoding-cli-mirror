mod cache;
mod config;
mod error;
mod providers;
mod server;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;
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

    // Initialize cache manager
    let cache_manager = cache::CacheManager::new(&config.cache)?;

    // Optionally refresh cache on startup
    if args.refresh {
        info!("Refreshing cache on startup...");
        // TODO: Implement refresh
    }

    // Start the HTTP server
    server::run(config, cache_manager).await?;

    Ok(())
}
