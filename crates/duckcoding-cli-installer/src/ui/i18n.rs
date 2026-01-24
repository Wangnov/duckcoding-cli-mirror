#[derive(Clone, Copy, Debug)]
pub enum Lang {
    Zh,
    En,
}

pub fn detect_lang(cli_lang: Option<&str>) -> Lang {
    let lang = cli_lang
        .map(|v| v.to_string())
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LC_MESSAGES").ok())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();

    let normalized = lang.to_lowercase();
    if normalized.starts_with("zh") {
        return Lang::Zh;
    }

    if normalized.is_empty()
        || normalized == "c"
        || normalized == "c.utf-8"
        || normalized == "posix"
    {
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("defaults")
                .args(["read", "-g", "AppleLocale"])
                .output()
            {
                if output.status.success() {
                    if let Ok(value) = String::from_utf8(output.stdout) {
                        let value = value.trim().to_lowercase();
                        if value.starts_with("zh") {
                            return Lang::Zh;
                        }
                    }
                }
            }
        }
    }

    Lang::En
}

pub fn tr(lang: Lang, key: &str) -> &'static str {
    match lang {
        Lang::Zh => match key {
            "mirror_url_missing" => "MIRROR_URL 未设置",
            "platform" => "平台",
            "version" => "版本",
            "already_up_to_date" => "已是最新版本",
            "update_available" => "发现新版本",
            "none" => "无",
            "node_missing" => "未检测到 Node.js",
            "node_pty_missing" => "未检测到 node-pty",
            "node_installed" => "已安装 Node.js",
            "node_pty_installed" => "已安装 node-pty",
            "node_present" => "Node.js 已存在",
            "node_pty_present" => "node-pty 已存在",
            "installing_node" => "正在安装 Node.js",
            "installing_node_pty" => "正在安装 node-pty",
            "installing_gemini" => "正在安装 Gemini",
            "installed_to" => "已安装到",
            "checksum_mismatch" => "SHA256 校验失败",
            "checksum_missing" => "缺少校验信息",
            "manifest_missing" => "缺少 manifest 平台信息",
            "node_checksums_missing" => "缺少 Node.js 校验信息",
            "node_checksums_empty" => "Node.js 校验文件为空",
            "node_dir_missing" => "未在压缩包中找到 Node.js 目录",
            "node_pty_checksums_missing" => "缺少 node-pty 校验信息",
            "gemini_checksums_missing" => "缺少 Gemini 校验信息",
            "codex_binary_missing" => "压缩包中未找到 Codex 可执行文件",
            "claude_config_failed" => "写入 ~/.claude.json 失败",
            "cleanup_old_versions_failed" => "清理旧版本失败",
            "unsupported_archive" => "不支持的压缩格式",
            "musl_node_unavailable" => "检测到 musl 平台，未提供内置 Node.js",
            "unsupported_platform" => "不支持的平台",
            "path_added" => "已添加 PATH",
            "symlink_created" => "符号链接:",
            "restart_terminal" => "请重启终端",
            "file_in_use" => "文件被占用，请关闭进程后重试",
            "welcome" => "欢迎使用 DuckCoding APP 镜像 CLI 安装脚本",
            "tagline" => "快速、安全、便捷的 CLI 工具安装服务",
            "gui_install" => "你也可以在 DuckCoding APP 中通过软件界面来安装",
            "app_url_label" => "DuckCoding APP 下载地址",
            "service_label" => "欢迎使用 DuckCoding 中转站服务",
            "installer" => "安装程序",
            "complete" => "安装完成!",
            "downloading" => "正在下载",
            "verifying" => "正在校验文件完整性...",
            "extracting" => "正在解压...",
            "checksum_ok" => "SHA256 校验通过",
            _ => "",
        },
        Lang::En => match key {
            "mirror_url_missing" => "MIRROR_URL is not set",
            "platform" => "platform",
            "version" => "version",
            "already_up_to_date" => "already up to date",
            "update_available" => "update available",
            "none" => "none",
            "node_missing" => "Node.js not found",
            "node_pty_missing" => "node-pty not found",
            "node_installed" => "Node.js installed",
            "node_pty_installed" => "node-pty installed",
            "node_present" => "Node.js present",
            "node_pty_present" => "node-pty present",
            "installing_node" => "installing Node.js",
            "installing_node_pty" => "installing node-pty",
            "installing_gemini" => "installing Gemini",
            "installed_to" => "installed to",
            "checksum_mismatch" => "SHA256 mismatch",
            "checksum_missing" => "checksum missing",
            "manifest_missing" => "manifest missing for platform",
            "node_checksums_missing" => "Node.js checksums missing",
            "node_checksums_empty" => "Node.js checksums empty",
            "node_dir_missing" => "Node.js directory not found in archive",
            "node_pty_checksums_missing" => "node-pty checksums missing",
            "gemini_checksums_missing" => "Gemini checksums missing",
            "codex_binary_missing" => "Codex binary not found in archive",
            "claude_config_failed" => "Failed to write ~/.claude.json",
            "cleanup_old_versions_failed" => "Failed to clean up old versions",
            "unsupported_archive" => "unsupported archive format",
            "musl_node_unavailable" => "musl detected; no private Node.js for this platform",
            "unsupported_platform" => "unsupported platform",
            "path_added" => "PATH added",
            "symlink_created" => "Symlink:",
            "restart_terminal" => "restart terminal",
            "file_in_use" => "file in use, close processes and retry",
            "welcome" => "Welcome to DuckCoding APP Mirror CLI Installer",
            "tagline" => "Fast, secure, and convenient CLI tool installation",
            "gui_install" => "You can also install via DuckCoding APP GUI",
            "app_url_label" => "DuckCoding APP Download",
            "service_label" => "DuckCoding Transit Service",
            "installer" => "Installer",
            "complete" => "Installation Complete!",
            "downloading" => "Downloading",
            "verifying" => "Verifying integrity...",
            "extracting" => "Extracting...",
            "checksum_ok" => "SHA256 verified",
            _ => "",
        },
    }
}
