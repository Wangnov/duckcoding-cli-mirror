#Requires -Version 5.1
param(
    [string]$Tag = "latest",
    [string]$Version = "",
    [switch]$Upgrade,
    [switch]$NoModifyPath,
    [switch]$Check,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$MirrorUrl = if ($env:MIRROR_URL) { $env:MIRROR_URL } else { "__MIRROR_URL__" }
$InstallDir = "$env:USERPROFILE\.duckcoding"
$BinDir = "$InstallDir\bin"

# Configure proxy if set
$ProxyUrl = if ($env:HTTPS_PROXY) { $env:HTTPS_PROXY } elseif ($env:HTTP_PROXY) { $env:HTTP_PROXY } else { $null }
$WebRequestParams = @{}
if ($ProxyUrl) {
    $WebRequestParams['Proxy'] = $ProxyUrl
}

# Detect language (zh = Chinese, otherwise English)
$LangCode = if ((Get-UICulture).Name -like "zh*") { "zh" } else { "en" }

# Internationalization messages
function Msg {
    param([string]$Key, [string]$Arg1 = "", [string]$Arg2 = "")

    $messages = @{
        "zh" = @{
            "version"            = "版本: $Arg1"
            "already_up_to_date" = "已是最新版本: $Arg1"
            "update_available"   = "有可用更新: $Arg1 -> $Arg2"
            "stopping_process"   = "正在停止运行中的 Claude Code 进程..."
            "process_running"    = "Claude Code 正在运行中。请先关闭它或使用 -Force 参数。"
            "process_id"         = "进程 ID: $Arg1"
            "downloading"        = "正在下载 Claude Code..."
            "verifying"          = "正在校验文件完整性..."
            "checksum_ok"        = "SHA256 校验通过"
            "checksum_failed"    = "错误: SHA256 校验失败!`n期望: $Arg1`n实际: $Arg2"
            "installed_to"       = "Claude Code $Arg1 已安装到 $Arg2"
            "path_added"         = "已添加 $Arg1 到 PATH"
            "restart_terminal"   = "请重启终端以使用 'claude' 命令"
            "install_complete"   = "安装完成!"
        }
        "en" = @{
            "version"            = "Version: $Arg1"
            "already_up_to_date" = "Already up to date: $Arg1"
            "update_available"   = "Update available: $Arg1 -> $Arg2"
            "stopping_process"   = "Stopping running Claude Code process..."
            "process_running"    = "Claude Code is currently running. Please close it first or use -Force flag."
            "process_id"         = "Process ID: $Arg1"
            "downloading"        = "Downloading Claude Code..."
            "verifying"          = "Verifying file integrity..."
            "checksum_ok"        = "SHA256 checksum verified"
            "checksum_failed"    = "Error: SHA256 checksum mismatch!`nExpected: $Arg1`nActual: $Arg2"
            "installed_to"       = "Claude Code $Arg1 installed to $Arg2"
            "path_added"         = "Added $Arg1 to PATH"
            "restart_terminal"   = "Please restart your terminal to use 'claude' command"
            "install_complete"   = "Installation complete!"
        }
    }

    Write-Host $messages[$LangCode][$Key]
}

# Get version
if (-not $Version) {
    $Version = Invoke-RestMethod "$MirrorUrl/claude-code/$Tag" @WebRequestParams
}
Msg "version" $Version

# Check current version
$CurrentVersion = ""
$VersionFile = "$InstallDir\versions.json"
if (Test-Path $VersionFile) {
    $VersionInfo = Get-Content $VersionFile | ConvertFrom-Json
    $CurrentVersion = $VersionInfo.claude.version
}

# Check only mode
if ($Check) {
    if ($CurrentVersion -eq $Version) {
        Msg "already_up_to_date" $Version
    } else {
        Msg "update_available" $CurrentVersion $Version
    }
    exit 0
}

# Skip download if already up to date (unless -Upgrade is specified)
if ($CurrentVersion -eq $Version -and -not $Upgrade) {
    Msg "already_up_to_date" $Version
    exit 0
}

# Show update info if upgrading
if ($CurrentVersion -and $CurrentVersion -ne $Version) {
    Msg "update_available" $CurrentVersion $Version
}

# Check if claude is running
$running = Get-Process -Name "claude" -ErrorAction SilentlyContinue
if ($running) {
    if ($Force) {
        Msg "stopping_process"
        Stop-Process -Name "claude" -Force
        Start-Sleep -Seconds 1
    } else {
        Msg "process_running"
        Msg "process_id" $running.Id
        exit 1
    }
}

# Create directories
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

# Get expected checksum from manifest
$Manifest = Invoke-RestMethod "$MirrorUrl/claude-code/$Version/manifest.json" @WebRequestParams
$ExpectedSha256 = $Manifest.platforms.'win32-x64'.checksum

# Download binary
Msg "downloading"
$ProgressPreference = 'Continue'
Invoke-WebRequest "$MirrorUrl/claude-code/$Version/win32-x64/claude.exe" -OutFile "$BinDir\claude.exe" -UseBasicParsing @WebRequestParams

# Verify SHA256 checksum
Msg "verifying"
$ActualSha256 = (Get-FileHash -Path "$BinDir\claude.exe" -Algorithm SHA256).Hash.ToLower()

if ($ActualSha256 -ne $ExpectedSha256) {
    Msg "checksum_failed" $ExpectedSha256 $ActualSha256
    Remove-Item "$BinDir\claude.exe" -Force
    exit 1
}
Msg "checksum_ok"

# Save version info (update instead of overwrite)
$VersionInfo = @{}
if (Test-Path $VersionFile) {
    try {
        $VersionInfo = Get-Content $VersionFile | ConvertFrom-Json
    } catch {
        $VersionInfo = @{}
    }
}

$VersionInfo | Add-Member -NotePropertyName "claude" -NotePropertyValue @{
    version = $Version
    tag = $Tag
    installed_at = (Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ")
} -Force

$VersionInfo | ConvertTo-Json -Depth 6 | Set-Content $VersionFile

Msg "installed_to" $Version "$BinDir\claude.exe"

# Setup PATH
if (-not $NoModifyPath) {
    $CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($CurrentPath -notlike "*$BinDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$BinDir;$CurrentPath", "User")
        $env:Path = "$BinDir;$env:Path"
        Msg "path_added" $BinDir
        Msg "restart_terminal"
    }
}

Msg "install_complete"
