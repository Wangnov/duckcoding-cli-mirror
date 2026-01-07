#Requires -Version 5.1
param(
    [string]$Tag = "latest",
    [string]$Version = "",
    [string]$NodeTag = "latest",
    [string]$NodeVersion = "",
    [string]$NodePtyTag = "latest",
    [string]$NodePtyVersion = "",
    [switch]$Upgrade,
    [switch]$NoModifyPath,
    [switch]$Check
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

function Msg {
    param([string]$Key, [string]$Arg1 = "", [string]$Arg2 = "")

    $messages = @{
        "zh" = @{
            "version"            = "版本: $Arg1"
            "already_up_to_date" = "已是最新版本: $Arg1"
            "update_available"   = "有可用更新: $Arg1 -> $Arg2"
            "downloading"        = "正在下载 Gemini CLI..."
            "verifying"          = "正在校验文件完整性..."
            "checksum_ok"        = "SHA256 校验通过"
            "checksum_failed"    = "错误: SHA256 校验失败!`n期望: $Arg1`n实际: $Arg2"
            "checksum_missing"   = "错误: 无法获取校验值 (平台: $Arg1)"
            "installing_node"    = "正在安装私有 Node.js: $Arg1"
            "installing_node_pty"= "正在安装 node-pty 预编译: $Arg1"
            "node_ok"            = "已检测到私有 Node.js: $Arg1"
            "node_pty_ok"        = "已检测到 node-pty: $Arg1"
            "installed_to"       = "Gemini CLI $Arg1 已安装到 $Arg2"
            "path_added"         = "已添加 $Arg1 到 PATH"
            "restart_terminal"   = "请重启终端以使用 'gemini' 命令"
            "install_complete"   = "安装完成!"
            "check_node_missing" = "未检测到私有 Node.js"
            "check_node_pty_missing" = "未检测到 node-pty"
        }
        "en" = @{
            "version"            = "Version: $Arg1"
            "already_up_to_date" = "Already up to date: $Arg1"
            "update_available"   = "Update available: $Arg1 -> $Arg2"
            "downloading"        = "Downloading Gemini CLI..."
            "verifying"          = "Verifying file integrity..."
            "checksum_ok"        = "SHA256 checksum verified"
            "checksum_failed"    = "Error: SHA256 checksum mismatch!`nExpected: $Arg1`nActual: $Arg2"
            "checksum_missing"   = "Error: checksum not found (platform: $Arg1)"
            "installing_node"    = "Installing private Node.js: $Arg1"
            "installing_node_pty"= "Installing node-pty prebuilds: $Arg1"
            "node_ok"            = "Private Node.js found: $Arg1"
            "node_pty_ok"        = "node-pty found: $Arg1"
            "installed_to"       = "Gemini CLI $Arg1 installed to $Arg2"
            "path_added"         = "Added $Arg1 to PATH"
            "restart_terminal"   = "Please restart your terminal to use 'gemini' command"
            "install_complete"   = "Installation complete!"
            "check_node_missing" = "Private Node.js not found"
            "check_node_pty_missing" = "node-pty not found"
        }
    }

    Write-Host $messages[$LangCode][$Key]
}

# Resolve versions
if (-not $Version) {
    $Version = Invoke-RestMethod "$MirrorUrl/gemini/$Tag" @WebRequestParams
}
if (-not $NodeVersion) {
    $NodeVersion = Invoke-RestMethod "$MirrorUrl/node/$NodeTag" @WebRequestParams
}
if (-not $NodePtyVersion) {
    $NodePtyVersion = Invoke-RestMethod "$MirrorUrl/node-pty/$NodePtyTag" @WebRequestParams
}

Msg "version" $Version

# Check current versions
$VersionFile = "$InstallDir\versions.json"
$CurrentGeminiVersion = ""
$CurrentNodeVersion = ""
$CurrentNodePtyVersion = ""
if (Test-Path $VersionFile) {
    try {
        $VersionInfo = Get-Content $VersionFile | ConvertFrom-Json
        $CurrentGeminiVersion = $VersionInfo.gemini.version
        $CurrentNodeVersion = $VersionInfo.node.version
        $CurrentNodePtyVersion = $VersionInfo.node_pty.version
    } catch {
        $CurrentGeminiVersion = ""
        $CurrentNodeVersion = ""
        $CurrentNodePtyVersion = ""
    }
}

# Check only mode
if ($Check) {
    if ($CurrentGeminiVersion -eq $Version) {
        Msg "already_up_to_date" $Version
    } else {
        Msg "update_available" $CurrentGeminiVersion $Version
    }

    if ($CurrentNodeVersion) {
        Msg "node_ok" $CurrentNodeVersion
    } else {
        Msg "check_node_missing"
    }

    if ($CurrentNodePtyVersion) {
        Msg "node_pty_ok" $CurrentNodePtyVersion
    } else {
        Msg "check_node_pty_missing"
    }
    exit 0
}

$SkipGemini = $false
if ($CurrentGeminiVersion -eq $Version -and -not $Upgrade) {
    Msg "already_up_to_date" $Version
    $SkipGemini = $true
} elseif ($CurrentGeminiVersion -and $CurrentGeminiVersion -ne $Version) {
    Msg "update_available" $CurrentGeminiVersion $Version
}

# Detect platform
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLower()
$Platform = switch ($Arch) {
    "arm64" { "win32-arm64" }
    "x64" { "win32-x64" }
    "x86" { "win32-x64" }
    default { throw "Unsupported architecture: $Arch" }
}

function Update-VersionInfo {
    param([string]$Key, [string]$Ver, [string]$Tag)

    $info = @{}
    if (Test-Path $VersionFile) {
        try { $info = Get-Content $VersionFile | ConvertFrom-Json } catch { $info = @{} }
    }

    $info | Add-Member -NotePropertyName $Key -NotePropertyValue @{
        version = $Ver
        tag = $Tag
        installed_at = (Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ")
    } -Force

    $info | ConvertTo-Json -Depth 6 | Set-Content $VersionFile
}

# Ensure Node.js runtime
$NodeDir = "$InstallDir\node\versions\$NodeVersion"
$NodeExe = "$NodeDir\node.exe"
if ((Test-Path $NodeExe) -and -not $Upgrade) {
    Msg "node_ok" $NodeVersion
} else {
    Msg "installing_node" $NodeVersion
    $NodeChecksums = Invoke-RestMethod "$MirrorUrl/node/$NodeVersion/checksums.json" @WebRequestParams
    $NodeFiles = $NodeChecksums.platforms.$Platform.files
    if (-not $NodeFiles) {
        Msg "checksum_missing" $Platform
        exit 1
    }

    $nodeFileProp = $NodeFiles.PSObject.Properties | Select-Object -First 1
    $NodeFilename = $nodeFileProp.Name
    $ExpectedSha = $nodeFileProp.Value.sha256
    if (-not $ExpectedSha) {
        Msg "checksum_missing" $Platform
        exit 1
    }

    $TempDir = Join-Path $env:TEMP "duckcoding-node-$NodeVersion"
    Remove-Item $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
    $TempFile = Join-Path $TempDir $NodeFilename
    $ExtractDir = Join-Path $TempDir "extract"
    New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null

    Invoke-WebRequest "$MirrorUrl/node/$NodeVersion/$Platform/$NodeFilename" -OutFile $TempFile -UseBasicParsing @WebRequestParams

    Msg "verifying"
    $ActualSha = (Get-FileHash -Path $TempFile -Algorithm SHA256).Hash.ToLower()
    if ($ActualSha -ne $ExpectedSha) {
        Msg "checksum_failed" $ExpectedSha $ActualSha
        exit 1
    }
    Msg "checksum_ok"

    Expand-Archive -Path $TempFile -DestinationPath $ExtractDir -Force
    $SourceDir = Get-ChildItem $ExtractDir -Directory | Select-Object -First 1
    if (-not $SourceDir) {
        throw "Failed to extract Node.js archive"
    }

    Remove-Item $NodeDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $NodeDir | Out-Null
    Copy-Item "$($SourceDir.FullName)\*" $NodeDir -Recurse -Force

    $NodeChecksums | ConvertTo-Json -Depth 6 | Set-Content "$NodeDir\checksums.json"
    try {
        Invoke-WebRequest "$MirrorUrl/node/$NodeVersion/SHASUMS256.txt" -OutFile "$NodeDir\SHASUMS256.txt" -UseBasicParsing @WebRequestParams
    } catch {}

    Update-VersionInfo "node" $NodeVersion $NodeTag
}

# Ensure node-pty prebuilds
$PtyDir = "$InstallDir\node-pty\versions\$NodePtyVersion\prebuilds\$Platform"
if ((Test-Path "$PtyDir\pty.node") -and -not $Upgrade) {
    Msg "node_pty_ok" $NodePtyVersion
} else {
    Msg "installing_node_pty" $NodePtyVersion
    $PtyChecksums = Invoke-RestMethod "$MirrorUrl/node-pty/$NodePtyVersion/checksums.json" @WebRequestParams
    $PtyFiles = $PtyChecksums.platforms.$Platform.files
    if (-not $PtyFiles) {
        Msg "checksum_missing" $Platform
        exit 1
    }

    New-Item -ItemType Directory -Force -Path $PtyDir | Out-Null
    foreach ($prop in $PtyFiles.PSObject.Properties) {
        $filename = $prop.Name
        $entry = $prop.Value
        $expected = $entry.sha256
        if (-not $expected) {
            Msg "checksum_missing" $Platform
            exit 1
        }

        $dest = Join-Path $PtyDir $filename
        Invoke-WebRequest "$MirrorUrl/node-pty/$NodePtyVersion/prebuilds/$Platform/$filename" -OutFile $dest -UseBasicParsing @WebRequestParams

        Msg "verifying"
        $actual = (Get-FileHash -Path $dest -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            Msg "checksum_failed" $expected $actual
            Remove-Item $dest -Force
            exit 1
        }
        Msg "checksum_ok"
    }

    $PtyVersionDir = "$InstallDir\node-pty\versions\$NodePtyVersion"
    New-Item -ItemType Directory -Force -Path $PtyVersionDir | Out-Null
    $PtyChecksums | ConvertTo-Json -Depth 6 | Set-Content "$PtyVersionDir\checksums.json"
    Update-VersionInfo "node_pty" $NodePtyVersion $NodePtyTag
}

if (-not $SkipGemini) {
    # Download Gemini CLI
    Msg "downloading"
    $GeminiChecksums = Invoke-RestMethod "$MirrorUrl/api/gemini/checksums" @WebRequestParams
    $GeminiEntry = $GeminiChecksums."$Version".universal
    $ExpectedSha = $GeminiEntry.sha256
    if (-not $ExpectedSha) {
        Msg "checksum_missing" "universal"
        exit 1
    }

    $GeminiDir = "$InstallDir\gemini\versions\$Version"
    New-Item -ItemType Directory -Force -Path $GeminiDir | Out-Null
    $GeminiPath = "$GeminiDir\gemini.js"
    Invoke-WebRequest "$MirrorUrl/gemini/$Version/gemini.js" -OutFile $GeminiPath -UseBasicParsing @WebRequestParams

    Msg "verifying"
    $ActualSha = (Get-FileHash -Path $GeminiPath -Algorithm SHA256).Hash.ToLower()
    if ($ActualSha -ne $ExpectedSha) {
        Msg "checksum_failed" $ExpectedSha $ActualSha
        exit 1
    }
    Msg "checksum_ok"

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    $GeminiCmd = @"
@echo off
setlocal
set "INSTALL_DIR=$InstallDir"
set "NODE_EXE=%INSTALL_DIR%\node\versions\$NodeVersion\node.exe"
set "GEMINI_JS=%INSTALL_DIR%\gemini\versions\$Version\gemini.js"
set "DUCKCODING_NODE_PTY_DIR=%INSTALL_DIR%\node-pty\versions\$NodePtyVersion\prebuilds"

if not exist "%NODE_EXE%" (
  echo Private Node.js not found: %NODE_EXE%
  exit /b 1
)
if not exist "%GEMINI_JS%" (
  echo Gemini CLI not found: %GEMINI_JS%
  exit /b 1
)

"%NODE_EXE%" "%GEMINI_JS%" %*
"@
    $GeminiCmd | Set-Content -Encoding ASCII "$BinDir\gemini.cmd"

    Update-VersionInfo "gemini" $Version $Tag
    Msg "installed_to" $Version "$BinDir\gemini.cmd"
}

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
