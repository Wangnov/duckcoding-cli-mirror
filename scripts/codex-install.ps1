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
            "stopping_process"   = "正在停止运行中的 Codex 进程..."
            "process_running"    = "Codex 正在运行中。请先关闭它或使用 -Force 参数。"
            "process_id"         = "进程 ID: $Arg1"
            "downloading"        = "正在下载 Codex..."
            "verifying"          = "正在校验文件完整性..."
            "checksum_ok"        = "SHA256 校验通过"
            "checksum_failed"    = "错误: SHA256 校验失败!`n期望: $Arg1`n实际: $Arg2"
            "checksum_missing"   = "错误: 无法获取校验值 (平台: $Arg1)"
            "installed_to"       = "Codex $Arg1 已安装到 $Arg2"
            "path_added"         = "已添加 $Arg1 到 PATH"
            "restart_terminal"   = "请重启终端以使用 'codex' 命令"
            "install_complete"   = "安装完成!"
        }
        "en" = @{
            "version"            = "Version: $Arg1"
            "already_up_to_date" = "Already up to date: $Arg1"
            "update_available"   = "Update available: $Arg1 -> $Arg2"
            "stopping_process"   = "Stopping running Codex process..."
            "process_running"    = "Codex is currently running. Please close it first or use -Force flag."
            "process_id"         = "Process ID: $Arg1"
            "downloading"        = "Downloading Codex..."
            "verifying"          = "Verifying file integrity..."
            "checksum_ok"        = "SHA256 checksum verified"
            "checksum_failed"    = "Error: SHA256 checksum mismatch!`nExpected: $Arg1`nActual: $Arg2"
            "checksum_missing"   = "Error: checksum not found (platform: $Arg1)"
            "installed_to"       = "Codex $Arg1 installed to $Arg2"
            "path_added"         = "Added $Arg1 to PATH"
            "restart_terminal"   = "Please restart your terminal to use 'codex' command"
            "install_complete"   = "Installation complete!"
        }
    }

    Write-Host $messages[$LangCode][$Key]
}

# Get version
if (-not $Version) {
    $Version = Invoke-RestMethod "$MirrorUrl/codex/$Tag" @WebRequestParams
}
Msg "version" $Version

# Check current version
$CurrentVersion = ""
$VersionFile = "$InstallDir\versions.json"
if (Test-Path $VersionFile) {
    try {
        $VersionInfo = Get-Content $VersionFile | ConvertFrom-Json
        $CurrentVersion = $VersionInfo.codex.version
    } catch {
        $CurrentVersion = ""
    }
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

# Check if codex is running
$running = Get-Process -Name "codex" -ErrorAction SilentlyContinue
if ($running) {
    if ($Force) {
        Msg "stopping_process"
        Stop-Process -Name "codex" -Force
        Start-Sleep -Seconds 1
    } else {
        Msg "process_running"
        Msg "process_id" $running.Id
        exit 1
    }
}

# Create directories
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

# Get expected checksum from checksums API
$Checksums = Invoke-RestMethod "$MirrorUrl/api/codex/checksums" @WebRequestParams
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLower()
$Platform = switch ($Arch) {
    "arm64" { "win32-arm64" }
    "x64" { "win32-x64" }
    "x86" { "win32-x64" }
    default { throw "Unsupported architecture: $Arch" }
}
$ExpectedSha256 = $Checksums.$Version.$Platform.sha256
$ExpectedFilename = $Checksums.$Version.$Platform.filename

if (-not $ExpectedSha256) {
    Msg "checksum_missing" $Platform
    exit 1
}

$AssetName = if ($ExpectedFilename) { $ExpectedFilename } else { "codex-x86_64-pc-windows-msvc.exe" }
$TempFile = Join-Path $env:TEMP $AssetName

# Download binary
Msg "downloading"
$ProgressPreference = 'Continue'
Invoke-WebRequest "$MirrorUrl/codex/$Version/$Platform/$AssetName" -OutFile $TempFile -UseBasicParsing @WebRequestParams

# Verify SHA256 checksum
Msg "verifying"
$ActualSha256 = (Get-FileHash -Path $TempFile -Algorithm SHA256).Hash.ToLower()

if ($ActualSha256 -ne $ExpectedSha256) {
    Msg "checksum_failed" $ExpectedSha256 $ActualSha256
    Remove-Item $TempFile -Force
    exit 1
}
Msg "checksum_ok"

Copy-Item $TempFile "$BinDir\codex.exe" -Force
Remove-Item $TempFile -Force

# Save version info
$VersionInfo = @{}
if (Test-Path $VersionFile) {
    try {
        $VersionInfo = Get-Content $VersionFile | ConvertFrom-Json
    } catch {
        $VersionInfo = @{}
    }
}

$VersionInfo.codex = @{
    version = $Version
    tag = $Tag
    installed_at = (Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ")
}

$VersionInfo | ConvertTo-Json | Set-Content "$VersionFile"

Msg "installed_to" $Version "$BinDir\codex.exe"

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
