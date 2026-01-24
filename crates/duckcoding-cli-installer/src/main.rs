mod ui;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::Proxy;
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;
use tempfile::{NamedTempFile, TempDir};
use ui::{Theme, Ui, detect_lang, emit_json, init_output, output, record_event};

#[derive(Parser, Debug)]
#[command(
    name = "duckcoding-cli-installer",
    version,
    about = "DuckCoding CLI installer"
)]
struct Cli {
    #[arg(long, default_value = "__MIRROR_URL__")]
    mirror_url: String,

    #[arg(long)]
    install_dir: Option<PathBuf>,

    #[arg(long, default_value_t = 3)]
    retries: u32,

    #[arg(long, default_value_t = 10)]
    connect_timeout_secs: u64,

    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,

    #[arg(long)]
    proxy: Option<String>,

    #[arg(long)]
    no_proxy: bool,

    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true)]
    lang: Option<String>,

    #[command(subcommand)]
    command: CommandGroup,
}

#[derive(Subcommand, Debug)]
enum CommandGroup {
    #[command(name = "codex")]
    Codex(ProviderArgs),
    #[command(name = "claude-code")]
    Claude(ProviderArgs),
    #[command(name = "gemini")]
    Gemini(GeminiArgs),
}

#[derive(Args, Debug, Clone)]
struct ProviderArgs {
    #[arg(long)]
    tag: Option<String>,

    #[arg(long)]
    version: Option<String>,

    #[arg(long)]
    upgrade: bool,

    #[arg(long)]
    check: bool,

    #[arg(long)]
    no_modify_path: bool,
}

#[derive(Args, Debug, Clone)]
struct GeminiArgs {
    #[command(flatten)]
    common: ProviderArgs,

    #[arg(long)]
    node_tag: Option<String>,

    #[arg(long)]
    node_version: Option<String>,

    #[arg(long)]
    node_pty_tag: Option<String>,

    #[arg(long)]
    node_pty_version: Option<String>,
}

#[derive(Clone)]
struct InstallContext {
    mirror_url: String,
    install_dir: PathBuf,
    bin_dir: PathBuf,
    client: Client,
    retries: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct VersionInfo {
    version: String,
    tag: String,
    installed_at: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct VersionsFile {
    #[serde(flatten)]
    entries: HashMap<String, VersionInfo>,
}

#[derive(Deserialize, Debug)]
struct ChecksumsByVersion {
    #[serde(flatten)]
    versions: HashMap<String, HashMap<String, AssetMeta>>,
}

#[derive(Deserialize, Debug, Clone)]
struct AssetMeta {
    sha256: String,
    size: Option<u64>,
    filename: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ClaudeManifest {
    platforms: HashMap<String, ClaudePlatform>,
}

#[derive(Deserialize, Debug)]
struct ClaudePlatform {
    checksum: String,
    size: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug)]
struct NodeChecksums {
    platforms: HashMap<String, NodePlatform>,
}

#[derive(Serialize, Deserialize, Debug)]
struct NodePlatform {
    files: HashMap<String, NodeFileMeta>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct NodeFileMeta {
    sha256: String,
    size: Option<u64>,
}

fn main() {
    match run() {
        Ok(()) => {
            if output().json {
                emit_json(true, None);
            }
        }
        Err(err) => {
            if output().json {
                emit_json(false, Some(err.to_string()));
            } else {
                eprintln!("error: {err}");
            }
            std::process::exit(1);
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let lang = detect_lang(cli.lang.as_deref());
    init_output(cli.json, lang);

    let mirror_url = if cli.mirror_url.contains("__MIRROR_URL__") {
        std::env::var("MIRROR_URL").unwrap_or(cli.mirror_url)
    } else {
        cli.mirror_url
    };

    if mirror_url.contains("__MIRROR_URL__") {
        bail!("{}", tr("mirror_url_missing"));
    }

    let install_dir = match cli.install_dir {
        Some(path) => expand_tilde(path)?,
        None => default_install_dir()?,
    };
    let bin_dir = install_dir.join("bin");

    fs::create_dir_all(&install_dir)?;

    let client = build_client(
        cli.proxy.as_deref(),
        cli.no_proxy,
        cli.connect_timeout_secs,
        cli.timeout_secs,
    )?;

    let ctx = InstallContext {
        mirror_url,
        install_dir,
        bin_dir,
        client,
        retries: cli.retries,
    };

    match cli.command {
        CommandGroup::Codex(args) => install_codex(&ctx, args),
        CommandGroup::Claude(args) => install_claude(&ctx, args),
        CommandGroup::Gemini(args) => install_gemini(&ctx, args),
    }
}

fn build_client(
    proxy: Option<&str>,
    no_proxy: bool,
    connect_timeout_secs: u64,
    timeout_secs: u64,
) -> Result<Client> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("duckcoding-cli-installer");

    if no_proxy {
        builder = builder.no_proxy();
    } else if let Some(proxy_url) = proxy {
        builder = builder.proxy(Proxy::all(proxy_url)?);
    }

    builder.build().context("build http client")
}

fn install_codex(ctx: &InstallContext, args: ProviderArgs) -> Result<()> {
    let ui = Ui::new(Theme::Codex);
    ui.banner("Codex");

    let platform = detect_platform()?;
    let target = platform_target(&platform)
        .ok_or_else(|| anyhow!("{}: {platform}", tr("unsupported_platform")))?;

    let tag = resolve_tag(args.tag, "TAG");
    let version = match args.version {
        Some(v) => v,
        None => fetch_text(ctx, &format!("{}/codex/{}", ctx.mirror_url, tag))?,
    };

    ui.info(&format!("{}: {platform}", tr("platform")));
    ui.info(&format!("{}: {version}", tr("version")));

    let mut versions = load_versions(&ctx.install_dir)?;
    let installed = versions.entries.get("codex").map(|v| v.version.clone());
    let is_windows = is_windows_platform(&platform);
    let binary_name = if is_windows { "codex.exe" } else { "codex" };
    let version_dir = provider_version_dir(&ctx.install_dir, "codex", &version);
    let version_bin = version_dir.join(binary_name);

    if args.check {
        report_check(&ui, installed.as_deref(), &version);
        return Ok(());
    }

    if installed.as_deref() == Some(&version) && !args.upgrade && version_bin.exists() {
        ui.success(&format!("{}: {version}", tr("already_up_to_date")));
        return Ok(());
    }

    let checksums: ChecksumsByVersion =
        fetch_json(ctx, &format!("{}/api/codex/checksums", ctx.mirror_url))?;
    let meta = checksums
        .versions
        .get(&version)
        .and_then(|m| m.get(&platform))
        .ok_or_else(|| anyhow!("{}: {version} {platform}", tr("checksum_missing")))?;

    let filename = meta.filename.clone().unwrap_or_else(|| {
        if is_windows {
            format!("codex-{target}.exe")
        } else {
            format!("codex-{target}.tar.gz")
        }
    });

    let url = format!(
        "{}/codex/{}/{}/{}",
        ctx.mirror_url, version, platform, filename
    );

    let tmp_dir = TempDir::new_in(&ctx.install_dir)?;
    let archive_path = tmp_dir.path().join(&filename);
    let label = ui.label_downloading("Codex");
    let download = download_with_progress(ctx, &url, &archive_path, meta.size, &label, &ui)?;
    run_with_spinner(&ui, tr("verifying"), || {
        verify_sha256(&download.sha256, &meta.sha256)
    })?;

    fs::create_dir_all(&version_dir)?;

    if is_windows {
        atomic_replace_file(&archive_path, &version_bin)?;
    } else {
        let extract_dir = tmp_dir.path().join("extract");
        fs::create_dir_all(&extract_dir)?;
        run_with_spinner(&ui, tr("extracting"), || {
            extract_archive(&archive_path, &extract_dir)
        })?;

        let binary = find_first_file(&extract_dir, |path| {
            if let Some(name) = path.file_name().and_then(OsStr::to_str) {
                name == "codex" || name.starts_with("codex-")
            } else {
                false
            }
        })?
        .ok_or_else(|| anyhow!("{}", tr("codex_binary_missing")))?;

        atomic_replace_file(&binary, &version_bin)?;
        set_executable(&version_bin)?;
    }

    update_bin_link(&ctx.bin_dir, binary_name, &version_bin, is_windows)?;
    setup_path(&ctx.bin_dir, binary_name, args.no_modify_path, &ui)?;
    update_versions(&mut versions, "codex", &version, &tag, &ctx.install_dir)?;
    let command_path = ctx.bin_dir.join(binary_name);
    ui.success(&format!(
        "{} {}",
        tr("installed_to"),
        command_path.display()
    ));
    record_event("codex", &version, &tag, Some(command_path));
    ui.complete();
    Ok(())
}

fn install_claude(ctx: &InstallContext, args: ProviderArgs) -> Result<()> {
    let ui = Ui::new(Theme::Claude);
    ui.banner("Claude Code");

    let platform = detect_platform()?;
    let tag = resolve_tag(args.tag, "TAG");
    let version = match args.version {
        Some(v) => v,
        None => fetch_text(ctx, &format!("{}/claude-code/{}", ctx.mirror_url, tag))?,
    };

    ui.info(&format!("{}: {platform}", tr("platform")));
    ui.info(&format!("{}: {version}", tr("version")));

    let mut versions = load_versions(&ctx.install_dir)?;
    let installed = versions.entries.get("claude").map(|v| v.version.clone());
    let is_windows = is_windows_platform(&platform);
    let binary_name = if is_windows { "claude.exe" } else { "claude" };
    let version_dir = provider_version_dir(&ctx.install_dir, "claude", &version);
    let version_bin = version_dir.join(binary_name);

    if args.check {
        report_check(&ui, installed.as_deref(), &version);
        return Ok(());
    }

    if installed.as_deref() == Some(&version) && !args.upgrade && version_bin.exists() {
        ui.success(&format!("{}: {version}", tr("already_up_to_date")));
        return Ok(());
    }

    let manifest: ClaudeManifest = fetch_json(
        ctx,
        &format!("{}/claude-code/{}/manifest.json", ctx.mirror_url, version),
    )?;
    let platform_meta = manifest
        .platforms
        .get(&platform)
        .ok_or_else(|| anyhow!("{}: {platform}", tr("manifest_missing")))?;

    let url = format!(
        "{}/claude-code/{}/{}/{}",
        ctx.mirror_url, version, platform, binary_name
    );

    let tmp_dir = TempDir::new_in(&ctx.install_dir)?;
    let tmp_path = tmp_dir.path().join(binary_name);
    let label = ui.label_downloading("Claude Code");
    let download = download_with_progress(ctx, &url, &tmp_path, platform_meta.size, &label, &ui)?;
    run_with_spinner(&ui, tr("verifying"), || {
        verify_sha256(&download.sha256, &platform_meta.checksum)
    })?;

    fs::create_dir_all(&version_dir)?;
    atomic_replace_file(&tmp_path, &version_bin)?;
    if !is_windows {
        set_executable(&version_bin)?;
    }

    update_bin_link(&ctx.bin_dir, binary_name, &version_bin, is_windows)?;
    setup_path(&ctx.bin_dir, binary_name, args.no_modify_path, &ui)?;
    update_versions(&mut versions, "claude", &version, &tag, &ctx.install_dir)?;
    if let Err(err) = write_claude_config() {
        ui.warn(&format!("{}: {err:#}", tr("claude_config_failed")));
    }
    if let Err(err) = prune_old_versions(&ctx.install_dir.join("claude").join("versions"), &version)
    {
        ui.warn(&format!("{}: {err:#}", tr("cleanup_old_versions_failed")));
    }
    let command_path = ctx.bin_dir.join(binary_name);
    ui.success(&format!(
        "{} {}",
        tr("installed_to"),
        command_path.display()
    ));
    record_event("claude", &version, &tag, Some(command_path));
    ui.complete();
    Ok(())
}

fn install_gemini(ctx: &InstallContext, args: GeminiArgs) -> Result<()> {
    let ui = Ui::new(Theme::Gemini);
    ui.banner("Gemini CLI");

    let platform = detect_platform()?;
    let tag = resolve_tag(args.common.tag, "TAG");
    let node_tag = resolve_tag(args.node_tag, "NODE_TAG");
    let node_pty_tag = resolve_tag(args.node_pty_tag, "NODE_PTY_TAG");

    let version = match args.common.version {
        Some(v) => v,
        None => fetch_text(ctx, &format!("{}/gemini/{}", ctx.mirror_url, tag))?,
    };
    let node_version = match args.node_version {
        Some(v) => v,
        None => fetch_text(ctx, &format!("{}/node/{}", ctx.mirror_url, node_tag))?,
    };
    let node_pty_version = match args.node_pty_version {
        Some(v) => v,
        None => fetch_text(
            ctx,
            &format!("{}/node-pty/{}", ctx.mirror_url, node_pty_tag),
        )?,
    };

    ui.info(&format!("{}: {platform}", tr("platform")));
    ui.info(&format!("{}: {version}", tr("version")));

    let mut versions = load_versions(&ctx.install_dir)?;
    let installed_gemini = versions.entries.get("gemini").map(|v| v.version.clone());
    let installed_node = versions.entries.get("node").map(|v| v.version.clone());
    let installed_node_pty = versions.entries.get("node_pty").map(|v| v.version.clone());

    if args.common.check {
        report_check(&ui, installed_gemini.as_deref(), &version);
        if let Some(installed_node) = installed_node.as_deref() {
            ui.success(&format!("{}: {}", tr("node_installed"), installed_node));
        } else {
            ui.info(tr("node_missing"));
        }
        if let Some(installed_node_pty) = installed_node_pty.as_deref() {
            ui.success(&format!(
                "{}: {}",
                tr("node_pty_installed"),
                installed_node_pty
            ));
        } else {
            ui.info(tr("node_pty_missing"));
        }
        return Ok(());
    }

    let (node_platform, is_musl) = normalize_node_platform(&platform);

    install_node(
        ctx,
        &node_platform,
        is_musl,
        &node_version,
        &node_tag,
        args.common.upgrade,
        &mut versions,
        &ui,
    )?;
    install_node_pty(
        ctx,
        &platform,
        &node_pty_version,
        &node_pty_tag,
        args.common.upgrade,
        &mut versions,
        &ui,
    )?;

    if installed_gemini.as_deref() == Some(&version) && !args.common.upgrade {
        ui.success(&format!("{}: {version}", tr("already_up_to_date")));
    } else {
        ui.info(&format!("{}: {version}", tr("installing_gemini")));
        install_gemini_js(
            ctx,
            &version,
            &tag,
            &platform,
            &node_version,
            &node_pty_version,
            &mut versions,
            &ui,
        )?;
    }

    setup_path(
        &ctx.bin_dir,
        gemini_command_name(&platform),
        args.common.no_modify_path,
        &ui,
    )?;
    ui.complete();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_node(
    ctx: &InstallContext,
    node_platform: &str,
    is_musl: bool,
    node_version: &str,
    node_tag: &str,
    upgrade: bool,
    versions: &mut VersionsFile,
    ui: &Ui,
) -> Result<()> {
    let node_dir = ctx
        .install_dir
        .join("node")
        .join("versions")
        .join(node_version);

    let node_bin = node_binary_path(&node_dir, node_platform);
    if node_bin.exists() && !upgrade {
        ui.success(&format!("{}: {}", tr("node_present"), node_version));
        return Ok(());
    }

    if is_musl && !node_bin.exists() {
        bail!("{}", tr("musl_node_unavailable"));
    }

    ui.info(&format!("{}: {}", tr("installing_node"), node_version));
    let checksums: NodeChecksums = fetch_json(
        ctx,
        &format!("{}/node/{}/checksums.json", ctx.mirror_url, node_version),
    )?;
    let platform_meta = checksums
        .platforms
        .get(node_platform)
        .ok_or_else(|| anyhow!("{}: {node_platform}", tr("node_checksums_missing")))?;
    let (filename, meta) = platform_meta
        .files
        .iter()
        .next()
        .ok_or_else(|| anyhow!("{}: {node_platform}", tr("node_checksums_empty")))?;

    let tmp_dir = TempDir::new_in(&ctx.install_dir)?;
    let archive_path = tmp_dir.path().join(filename);
    let url = format!(
        "{}/node/{}/{}/{}",
        ctx.mirror_url, node_version, node_platform, filename
    );
    let download = download_with_progress(ctx, &url, &archive_path, meta.size, "Node.js", ui)?;
    verify_sha256(&download.sha256, &meta.sha256)?;

    let extract_dir = tmp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    extract_archive(&archive_path, &extract_dir)?;

    let node_dir_prefix = node_archive_dir_prefix(node_version);
    let node_root = find_first_dir(&extract_dir, |path| {
        if let Some(name) = path.file_name().and_then(OsStr::to_str) {
            name.starts_with(&node_dir_prefix)
        } else {
            false
        }
    })?
    .ok_or_else(|| anyhow!("{}", tr("node_dir_missing")))?;

    fs::create_dir_all(ctx.install_dir.join("node").join("versions"))?;

    if node_dir.exists() {
        let backup = ctx.install_dir.join("node").join("versions").join(format!(
            "{}.bak.{}",
            node_version,
            unix_timestamp()
        ));
        fs::rename(&node_dir, &backup)?;
    }

    fs::rename(&node_root, &node_dir)?;
    let node_bin = node_binary_path(&node_dir, node_platform);
    if !is_windows_platform(node_platform) {
        set_executable(&node_bin)?;
    }
    fsync_path(&node_dir)?;
    fsync_path(node_dir.parent().unwrap_or(&node_dir))?;

    let checksum_path = node_dir.join("checksums.json");
    let json = serde_json::to_string_pretty(&checksums)?;
    write_file_atomic(&checksum_path, json.as_bytes())?;

    update_versions(versions, "node", node_version, node_tag, &ctx.install_dir)?;
    ui.success(&format!("{} {}", tr("installed_to"), node_dir.display()));
    record_event("node", node_version, node_tag, Some(node_bin));
    Ok(())
}

fn install_node_pty(
    ctx: &InstallContext,
    platform: &str,
    node_pty_version: &str,
    node_pty_tag: &str,
    upgrade: bool,
    versions: &mut VersionsFile,
    ui: &Ui,
) -> Result<()> {
    let pty_dir = ctx
        .install_dir
        .join("node-pty")
        .join("versions")
        .join(node_pty_version)
        .join("prebuilds")
        .join(platform);

    if pty_dir.join("pty.node").exists() && !upgrade {
        ui.success(&format!("{}: {}", tr("node_pty_present"), node_pty_version));
        return Ok(());
    }

    ui.info(&format!(
        "{}: {}",
        tr("installing_node_pty"),
        node_pty_version
    ));
    let checksums: NodeChecksums = fetch_json(
        ctx,
        &format!(
            "{}/node-pty/{}/checksums.json",
            ctx.mirror_url, node_pty_version
        ),
    )?;
    let platform_meta = checksums
        .platforms
        .get(platform)
        .ok_or_else(|| anyhow!("{}: {platform}", tr("node_pty_checksums_missing")))?;

    fs::create_dir_all(&pty_dir)?;
    for (filename, meta) in &platform_meta.files {
        let url = format!(
            "{}/node-pty/{}/prebuilds/{}/{}",
            ctx.mirror_url, node_pty_version, platform, filename
        );
        let tmp = NamedTempFile::new_in(&pty_dir)?;
        let download = download_with_progress(ctx, &url, tmp.path(), meta.size, filename, ui)?;
        verify_sha256(&download.sha256, &meta.sha256)?;
        let dest = pty_dir.join(filename);
        atomic_replace_file(tmp.path(), &dest)?;
        if filename == "spawn-helper" {
            set_executable(&dest)?;
        }
    }

    let checksum_path = ctx
        .install_dir
        .join("node-pty")
        .join("versions")
        .join(node_pty_version)
        .join("checksums.json");
    let json = serde_json::to_string_pretty(&checksums)?;
    write_file_atomic(&checksum_path, json.as_bytes())?;

    update_versions(
        versions,
        "node_pty",
        node_pty_version,
        node_pty_tag,
        &ctx.install_dir,
    )?;
    ui.success(&format!("{} {}", tr("installed_to"), pty_dir.display()));
    record_event("node_pty", node_pty_version, node_pty_tag, Some(pty_dir));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_gemini_js(
    ctx: &InstallContext,
    version: &str,
    tag: &str,
    platform: &str,
    node_version: &str,
    node_pty_version: &str,
    versions: &mut VersionsFile,
    ui: &Ui,
) -> Result<()> {
    let checksums: ChecksumsByVersion =
        fetch_json(ctx, &format!("{}/api/gemini/checksums", ctx.mirror_url))?;
    let meta = checksums
        .versions
        .get(version)
        .and_then(|m| m.get("universal"))
        .ok_or_else(|| anyhow!("{}: {version}", tr("gemini_checksums_missing")))?;

    let filename = meta
        .filename
        .clone()
        .unwrap_or_else(|| "gemini.js".to_string());

    let gemini_dir = ctx
        .install_dir
        .join("gemini")
        .join("versions")
        .join(version);
    fs::create_dir_all(&gemini_dir)?;

    let url = format!("{}/gemini/{}/{}", ctx.mirror_url, version, filename);
    let tmp = NamedTempFile::new_in(&gemini_dir)?;
    let label = ui.label_downloading("Gemini CLI");
    let download = download_with_progress(ctx, &url, tmp.path(), meta.size, &label, ui)?;
    run_with_spinner(ui, tr("verifying"), || {
        verify_sha256(&download.sha256, &meta.sha256)
    })?;

    let gemini_js = gemini_dir.join("gemini.js");
    atomic_replace_file(tmp.path(), &gemini_js)?;
    fsync_path(&gemini_dir)?;

    let is_windows = is_windows_platform(platform);
    let wrapper = build_gemini_wrapper(
        &ctx.install_dir,
        version,
        node_version,
        node_pty_version,
        is_windows,
    );
    let wrapper_name = if is_windows { "gemini.cmd" } else { "gemini" };
    let wrapper_path = ctx.bin_dir.join(wrapper_name);
    fs::create_dir_all(&ctx.bin_dir)?;
    write_file_atomic(&wrapper_path, wrapper.as_bytes())?;
    if !is_windows {
        set_executable(&wrapper_path)?;
    }

    update_versions(versions, "gemini", version, tag, &ctx.install_dir)?;
    ui.success(&format!(
        "{} {}",
        tr("installed_to"),
        wrapper_path.display()
    ));
    record_event("gemini", version, tag, Some(wrapper_path));
    Ok(())
}

fn build_gemini_wrapper(
    install_dir: &Path,
    version: &str,
    node_version: &str,
    node_pty_version: &str,
    is_windows: bool,
) -> String {
    if is_windows {
        return format!(
            r#"@echo off
setlocal
set "DEFAULT_INSTALL_DIR={install_dir}"
if defined INSTALL_DIR (
  set "INSTALL_DIR=%INSTALL_DIR%"
) else (
  set "INSTALL_DIR=%DEFAULT_INSTALL_DIR%"
)
set "NODE_EXE=%INSTALL_DIR%\node\versions\{node_version}\node.exe"
set "GEMINI_JS=%INSTALL_DIR%\gemini\versions\{version}\gemini.js"
set "DUCKCODING_NODE_PTY_DIR=%INSTALL_DIR%\node-pty\versions\{node_pty_version}\prebuilds"

if not exist "%NODE_EXE%" (
  echo Private Node.js not found: %NODE_EXE%
  exit /b 1
)
if not exist "%GEMINI_JS%" (
  echo Gemini CLI not found: %GEMINI_JS%
  exit /b 1
)

"%NODE_EXE%" "%GEMINI_JS%" %*
"#,
            install_dir = install_dir.display(),
            version = version,
            node_version = node_version,
            node_pty_version = node_pty_version
        );
    }

    format!(
        r#"#!/bin/bash
set -e
INSTALL_DIR="${{INSTALL_DIR:-{install_dir}}}"
NODE_BIN="$INSTALL_DIR/node/versions/{node_version}/bin/node"
GEMINI_JS="$INSTALL_DIR/gemini/versions/{version}/gemini.js"
export DUCKCODING_NODE_PTY_DIR="$INSTALL_DIR/node-pty/versions/{node_pty_version}/prebuilds"

if [[ ! -x "$NODE_BIN" ]]; then
  echo "Private Node.js not found: $NODE_BIN" >&2
  exit 1
fi
if [[ ! -f "$GEMINI_JS" ]]; then
  echo "Gemini CLI not found: $GEMINI_JS" >&2
  exit 1
fi

exec "$NODE_BIN" "$GEMINI_JS" "$@"
"#,
        install_dir = install_dir.display(),
        version = version,
        node_version = node_version,
        node_pty_version = node_pty_version
    )
}

fn download_with_progress(
    ctx: &InstallContext,
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    label: &str,
    ui: &Ui,
) -> Result<DownloadResult> {
    with_retry(ctx.retries, || {
        let mut response = ctx
            .client
            .get(url)
            .send()
            .with_context(|| format!("request {url}"))?
            .error_for_status()
            .with_context(|| format!("download {url}"))?;
        stream_to_file(&mut response, dest, expected_size, label, ui)
    })
}

fn stream_to_file(
    response: &mut reqwest::blocking::Response,
    dest: &Path,
    expected_size: Option<u64>,
    label: &str,
    ui: &Ui,
) -> Result<DownloadResult> {
    let total = expected_size.or_else(|| response.content_length());
    let mut file = File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut downloaded = 0u64;
    let mut progress = ui.download_progress(label, total);

    let result = (|| -> Result<()> {
        loop {
            let n = response.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            downloaded += n as u64;
            if let Some(progress) = progress.as_mut() {
                progress.update(downloaded);
            }
        }
        file.sync_all()?;
        if let Some(progress) = progress.as_mut() {
            progress.finish_ok(downloaded);
        }
        Ok(())
    })();

    if let Err(err) = result {
        if let Some(progress) = progress.as_mut() {
            progress.finish_err(Some(&err.to_string()));
        }
        return Err(err);
    }

    Ok(DownloadResult {
        sha256: hex::encode(hasher.finalize()),
    })
}

#[derive(Debug)]
struct DownloadResult {
    sha256: String,
}

fn verify_sha256(actual: &str, expected: &str) -> Result<()> {
    if expected.is_empty() {
        return Ok(());
    }
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        bail!(
            "{}: expected {expected}, got {actual}",
            tr("checksum_mismatch")
        );
    }
}

fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let name = archive_path.to_string_lossy();
    let file = File::open(archive_path)?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
        Ok(())
    } else if name.ends_with(".tar.xz") {
        let decoder = xz2::read::XzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest_dir)?;
        Ok(())
    } else if name.ends_with(".zip") {
        extract_zip(file, dest_dir)
    } else {
        bail!("{}: {name}", tr("unsupported_archive"));
    }
}

fn extract_zip(file: File, dest_dir: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(safe_name) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest_dir.join(safe_name);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = File::create(&out_path)?;
            std::io::copy(&mut entry, &mut outfile)?;
        }
    }
    Ok(())
}

fn atomic_replace_file(src: &Path, dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let tmp = NamedTempFile::new_in(parent)?;
    fs::copy(src, tmp.path())?;
    fs::rename(tmp.path(), dest).or_else(|_| {
        if dest.exists() {
            fs::remove_file(dest)?;
        }
        fs::rename(tmp.path(), dest)
    })?;
    fsync_path(dest)?;
    fsync_path(parent)?;
    Ok(())
}

fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(content)?;
    tmp.flush()?;
    fs::rename(tmp.path(), path).or_else(|_| {
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(tmp.path(), path)
    })?;
    fsync_path(path)?;
    fsync_path(parent)?;
    Ok(())
}

fn write_file_atomic_with_permissions(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(content)?;
    tmp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.exists() {
            fs::metadata(path)?.permissions().mode()
        } else {
            0o600
        };
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode))?;
    }
    fs::rename(tmp.path(), path).or_else(|_| {
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(tmp.path(), path)
    })?;
    fsync_path(path)?;
    fsync_path(parent)?;
    Ok(())
}

fn fsync_path(path: &Path) -> Result<()> {
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn find_first_file<F>(root: &Path, predicate: F) -> Result<Option<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if predicate(&path) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

fn find_first_dir<F>(root: &Path, predicate: F) -> Result<Option<PathBuf>>
where
    F: Fn(&Path) -> bool,
{
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if predicate(&path) {
                    return Ok(Some(path));
                }
                stack.push(path);
            }
        }
    }
    Ok(None)
}

fn load_versions(install_dir: &Path) -> Result<VersionsFile> {
    let path = install_dir.join("versions.json");
    if !path.exists() {
        return Ok(VersionsFile::default());
    }
    let data = fs::read_to_string(&path)?;
    let versions: VersionsFile = serde_json::from_str(&data)?;
    Ok(versions)
}

fn update_versions(
    versions: &mut VersionsFile,
    key: &str,
    version: &str,
    tag: &str,
    install_dir: &Path,
) -> Result<()> {
    let info = VersionInfo {
        version: version.to_string(),
        tag: tag.to_string(),
        installed_at: utc_now_rfc3339(),
    };
    versions.entries.insert(key.to_string(), info);
    let path = install_dir.join("versions.json");
    let json = serde_json::to_string_pretty(&versions)?;
    write_file_atomic(&path, json.as_bytes())?;
    Ok(())
}

fn fetch_text(ctx: &InstallContext, url: &str) -> Result<String> {
    with_retry(ctx.retries, || {
        let text = ctx
            .client
            .get(url)
            .send()
            .with_context(|| format!("request {url}"))?
            .error_for_status()
            .with_context(|| format!("request {url}"))?
            .text()
            .with_context(|| format!("read {url}"))?;
        Ok(text.trim().to_string())
    })
}

fn fetch_json<T: DeserializeOwned>(ctx: &InstallContext, url: &str) -> Result<T> {
    let text = fetch_text(ctx, url)?;
    let value = serde_json::from_str(&text).with_context(|| format!("parse json from {url}"))?;
    Ok(value)
}

fn with_retry<T, F>(retries: u32, mut op: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut last_err = None;
    for attempt in 1..=retries {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_err = Some(err);
                if attempt < retries {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("retry failed")))
}

fn run_with_spinner<T, F>(ui: &Ui, label: &str, op: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send,
    T: Send,
{
    if !ui.is_interactive() {
        return op();
    }
    let Some(mut spinner) = ui.spinner(label) else {
        return op();
    };

    thread::scope(|s| -> Result<T> {
        let (tx, rx) = mpsc::channel();
        s.spawn(move || {
            let _ = tx.send(op());
        });

        loop {
            match rx.try_recv() {
                Ok(result) => {
                    if result.is_ok() {
                        spinner.finish_ok();
                    } else {
                        spinner.finish_err();
                    }
                    return result;
                }
                Err(TryRecvError::Empty) => {
                    spinner.tick();
                    thread::sleep(Duration::from_millis(120));
                }
                Err(TryRecvError::Disconnected) => {
                    spinner.finish_err();
                    return Err(anyhow!("spinner disconnected"));
                }
            }
        }
    })
}

fn resolve_tag(value: Option<String>, env_key: &str) -> String {
    value
        .or_else(|| std::env::var(env_key).ok())
        .unwrap_or_else(|| "latest".to_string())
}

fn report_check(ui: &Ui, installed: Option<&str>, latest: &str) {
    if installed == Some(latest) {
        ui.success(&format!("{}: {latest}", tr("already_up_to_date")));
    } else {
        let current = installed.unwrap_or(tr("none"));
        ui.update(&format!(
            "{}: {current} -> {latest}",
            tr("update_available")
        ));
    }
}

fn tr(key: &str) -> &'static str {
    ui::tr(output().lang, key)
}

fn detect_platform() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let platform = match os {
        "macos" => match arch {
            "x86_64" => "darwin-x64".to_string(),
            "aarch64" | "arm64" => "darwin-arm64".to_string(),
            _ => bail!("{}: {arch}", tr("unsupported_platform")),
        },
        "windows" => match arch {
            "x86_64" => "win32-x64".to_string(),
            "aarch64" | "arm64" => "win32-arm64".to_string(),
            _ => bail!("{}: {arch}", tr("unsupported_platform")),
        },
        "linux" => {
            let mut suffix = String::new();
            if is_musl() {
                suffix = "-musl".to_string();
            }
            match arch {
                "x86_64" => format!("linux-x64{suffix}"),
                "aarch64" | "arm64" => format!("linux-arm64{suffix}"),
                _ => bail!("{}: {arch}", tr("unsupported_platform")),
            }
        }
        _ => bail!("{}: {os}", tr("unsupported_platform")),
    };

    Ok(platform)
}

fn is_musl() -> bool {
    if std::env::consts::OS != "linux" {
        return false;
    }
    if let Ok(out) = Command::new("ldd").arg("--version").output() {
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&out.stdout).to_lowercase());
        text.push_str(&String::from_utf8_lossy(&out.stderr).to_lowercase());
        if text.contains("musl") {
            return true;
        }
    }
    false
}

fn platform_target(platform: &str) -> Option<&'static str> {
    match platform {
        "darwin-x64" => Some("x86_64-apple-darwin"),
        "darwin-arm64" => Some("aarch64-apple-darwin"),
        "linux-x64" => Some("x86_64-unknown-linux-gnu"),
        "linux-arm64" => Some("aarch64-unknown-linux-gnu"),
        "linux-x64-musl" => Some("x86_64-unknown-linux-musl"),
        "linux-arm64-musl" => Some("aarch64-unknown-linux-musl"),
        "win32-x64" => Some("x86_64-pc-windows-msvc"),
        "win32-arm64" => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn is_windows_platform(platform: &str) -> bool {
    platform.starts_with("win32")
}

fn gemini_command_name(platform: &str) -> &'static str {
    if is_windows_platform(platform) {
        "gemini.cmd"
    } else {
        "gemini"
    }
}

fn node_binary_path(node_dir: &Path, platform: &str) -> PathBuf {
    if is_windows_platform(platform) {
        node_dir.join("node.exe")
    } else {
        node_dir.join("bin").join("node")
    }
}

fn normalize_node_platform(platform: &str) -> (String, bool) {
    if let Some(stripped) = platform.strip_suffix("-musl") {
        (stripped.to_string(), true)
    } else {
        (platform.to_string(), false)
    }
}

fn node_archive_dir_prefix(node_version: &str) -> String {
    let mut version = node_version;
    if let Some(stripped) = version.strip_prefix("node-v") {
        version = stripped;
    } else if let Some(stripped) = version.strip_prefix("node-") {
        version = stripped;
    }
    if let Some(stripped) = version.strip_prefix('v') {
        version = stripped;
    }
    format!("node-v{version}-")
}

fn write_claude_config() -> Result<()> {
    let path = claude_config_path()?;
    let existing = load_json_if_exists(&path)?;
    let mut root = normalize_json_object(existing);

    let user_id = root
        .get("userID")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(generate_user_id);
    let first_start = root
        .get("firstStartTime")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(utc_now_rfc3339_millis);

    root.insert(
        "installMethod".to_string(),
        Value::String("native".to_string()),
    );
    root.insert("autoUpdates".to_string(), Value::Bool(false));
    root.insert(
        "autoUpdatesProtectedForNative".to_string(),
        Value::Bool(true),
    );
    root.insert("userID".to_string(), Value::String(user_id));
    root.insert("firstStartTime".to_string(), Value::String(first_start));
    root.insert("sonnet45MigrationComplete".to_string(), Value::Bool(true));
    root.insert("opus45MigrationComplete".to_string(), Value::Bool(true));
    root.insert("opusProMigrationComplete".to_string(), Value::Bool(true));
    root.insert("thinkingMigrationComplete".to_string(), Value::Bool(true));

    if let Some(Value::Object(features)) = root.get_mut("cachedGrowthBookFeatures") {
        let defaults = default_growthbook_features();
        for (key, value) in defaults {
            features.entry(key).or_insert(value);
        }
    } else {
        root.insert(
            "cachedGrowthBookFeatures".to_string(),
            Value::Object(default_growthbook_features()),
        );
    }

    let json = serde_json::to_vec_pretty(&Value::Object(root))?;
    write_file_atomic_with_permissions(&path, &json)
}

fn prune_old_versions(versions_dir: &Path, keep_version: &str) -> Result<()> {
    if !versions_dir.exists() {
        return Ok(());
    }
    let mut last_err: Option<anyhow::Error> = None;
    for entry in fs::read_dir(versions_dir)? {
        let entry = match entry {
            Ok(item) => item,
            Err(err) => {
                last_err = Some(err.into());
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(value) => value,
            Err(err) => {
                last_err = Some(err.into());
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == keep_version {
            continue;
        }
        let trash_name = format!("{}.old.{}", name, unix_timestamp());
        let trash_path = versions_dir.join(trash_name);
        let remove_result = match fs::rename(&path, &trash_path) {
            Ok(()) => fs::remove_dir_all(&trash_path),
            Err(_) => fs::remove_dir_all(&path),
        };
        if let Err(err) = remove_result {
            last_err = Some(err.into());
        }
    }
    if let Some(err) = last_err {
        return Err(err);
    }
    Ok(())
}

fn load_json_if_exists(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(path)?;
    let json = serde_json::from_str::<Value>(&data)?;
    Ok(Some(json))
}

fn normalize_json_object(value: Option<Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn default_growthbook_features() -> Map<String, Value> {
    let mut map = Map::new();
    let mut batch = Map::new();
    batch.insert(
        "scheduledDelayMillis".to_string(),
        Value::Number(5000.into()),
    );
    batch.insert("maxExportBatchSize".to_string(), Value::Number(200.into()));
    batch.insert("maxQueueSize".to_string(), Value::Number(8192.into()));
    map.insert(
        "tengu_1p_event_batch_config".to_string(),
        Value::Object(batch),
    );
    map.insert("tengu_mcp_tool_search".to_string(), Value::Bool(false));
    map.insert("tengu_scratch".to_string(), Value::Bool(false));
    map.insert("tengu_log_segment_events".to_string(), Value::Bool(false));
    map.insert("tengu_log_datadog_events".to_string(), Value::Bool(true));
    map.insert(
        "tengu_pid_based_version_locking".to_string(),
        Value::Bool(true),
    );
    map.insert(
        "tengu_event_sampling_config".to_string(),
        Value::Object(Map::new()),
    );
    map.insert("tengu_tool_pear".to_string(), Value::Bool(false));
    map.insert(
        "tengu_keybinding_customization".to_string(),
        Value::Bool(false),
    );
    map.insert("tengu_thinkback".to_string(), Value::Bool(false));
    map
}

fn generate_user_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let user = std::env::var("USER").unwrap_or_default();
    let seed = format!("{now}-{pid}-{user}");
    hex::encode(Sha256::digest(seed.as_bytes()))
}

fn claude_config_path() -> Result<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").context("USERPROFILE is not set")?
    } else {
        std::env::var("HOME").context("HOME is not set")?
    };
    Ok(PathBuf::from(home).join(".claude.json"))
}

fn provider_version_dir(install_dir: &Path, provider: &str, version: &str) -> PathBuf {
    install_dir.join(provider).join("versions").join(version)
}

fn default_install_dir() -> Result<PathBuf> {
    if cfg!(windows) {
        let home = std::env::var("USERPROFILE").context("USERPROFILE is not set")?;
        Ok(PathBuf::from(home).join(".duckcoding"))
    } else {
        let home = std::env::var("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home).join(".duckcoding"))
    }
}

fn expand_tilde(path: PathBuf) -> Result<PathBuf> {
    if cfg!(windows) {
        return Ok(path);
    }
    let path_str = path.to_string_lossy();
    if let Some(stripped) = path_str.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME is not set")?;
        return Ok(PathBuf::from(home).join(stripped));
    }
    Ok(path)
}

fn setup_path(bin_dir: &Path, binary: &str, no_modify_path: bool, ui: &Ui) -> Result<()> {
    if no_modify_path {
        return Ok(());
    }
    if cfg!(windows) {
        let bin_dir = bin_dir
            .canonicalize()
            .unwrap_or_else(|_| bin_dir.to_path_buf());
        if path_contains(&bin_dir) {
            return Ok(());
        }
        let current = std::env::var("PATH").unwrap_or_default();
        let new_value = format!("{};{}", bin_dir.display(), current);
        let status = Command::new("cmd")
            .args(["/C", "setx", "PATH", &new_value])
            .status();
        unsafe {
            std::env::set_var("PATH", &new_value);
        }
        if let Ok(status) = status {
            if status.success() {
                ui.info(&format!("{} {}", tr("path_added"), bin_dir.display()));
            } else {
                ui.warn(tr("restart_terminal"));
            }
        } else {
            ui.warn(tr("restart_terminal"));
        }
        return Ok(());
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    let local_bin = PathBuf::from(&home).join(".local").join("bin");
    if path_contains(&local_bin) {
        fs::create_dir_all(&local_bin)?;
        let link = local_bin.join(binary);
        create_symlink(&bin_dir.join(binary), &link)?;
        ui.success(&format!("{} {}", tr("symlink_created"), link.display()));
        return Ok(());
    }

    let rc_file = shell_rc_file(&home);
    let path_line = r#"export PATH="$HOME/.duckcoding/bin:$PATH""#;
    let mut content = String::new();
    if let Ok(existing) = fs::read_to_string(&rc_file) {
        content = existing;
    }
    if !content.contains(".duckcoding/bin") {
        let mut file = OpenOptionsExt::open_append(&rc_file)?;
        writeln!(file)?;
        writeln!(file, "# DuckCoding CLI Mirror")?;
        writeln!(file, "{path_line}")?;
        ui.info(&format!("{} {}", tr("path_added"), rc_file.display()));
    }
    Ok(())
}

fn path_contains(path: &Path) -> bool {
    let path_var = match std::env::var_os("PATH") {
        Some(v) => v,
        None => return false,
    };
    for entry in std::env::split_paths(&path_var) {
        if entry == path {
            return true;
        }
    }
    false
}

fn shell_rc_file(home: &str) -> PathBuf {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    match name {
        "bash" => PathBuf::from(home).join(".bashrc"),
        "zsh" => PathBuf::from(home).join(".zshrc"),
        "fish" => PathBuf::from(home)
            .join(".config")
            .join("fish")
            .join("config.fish"),
        _ => PathBuf::from(home).join(".profile"),
    }
}

fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    if link.exists() {
        fs::remove_file(link)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)?;
    }
    Ok(())
}

fn update_bin_link(bin_dir: &Path, binary: &str, target: &Path, is_windows: bool) -> Result<()> {
    fs::create_dir_all(bin_dir)?;
    let link_path = bin_dir.join(binary);
    if is_windows {
        match atomic_replace_file(target, &link_path) {
            Ok(()) => Ok(()),
            Err(err) => {
                if is_file_in_use_error(&err) {
                    bail!("{}", tr("file_in_use"));
                }
                Err(err)
            }
        }
    } else {
        create_symlink(target, &link_path)?;
        Ok(())
    }
}

fn is_file_in_use_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io_err) = cause.downcast_ref::<std::io::Error>() {
            if matches!(io_err.kind(), std::io::ErrorKind::PermissionDenied) {
                return true;
            }
        }
        let msg = cause.to_string().to_lowercase();
        if msg.contains("text file busy")
            || msg.contains("file in use")
            || msg.contains("being used")
            || msg.contains("access is denied")
        {
            return true;
        }
    }
    false
}

fn utc_now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn utc_now_rfc3339_millis() -> String {
    let format = time::format_description::parse(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z",
    );
    match format {
        Ok(fmt) => time::OffsetDateTime::now_utc()
            .format(&fmt)
            .unwrap_or_else(|_| utc_now_rfc3339()),
        Err(_) => utc_now_rfc3339(),
    }
}

fn unix_timestamp() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

struct OpenOptionsExt;

impl OpenOptionsExt {
    fn open_append(path: &Path) -> Result<File> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(file)
    }
}
