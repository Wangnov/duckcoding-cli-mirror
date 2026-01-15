Param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ArgsList
)

$ProgressPreference = "SilentlyContinue"
$ErrorActionPreference = "Stop"

$MirrorUrl = $env:MIRROR_URL
if (-not $MirrorUrl -or $MirrorUrl -eq "__MIRROR_URL__") {
    $MirrorUrl = "__MIRROR_URL__"
}
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { "$env:USERPROFILE\.duckcoding" }
$Tag = if ($env:TAG) { $env:TAG } else { "latest" }
$Version = ""
$Upgrade = $false
$CheckOnly = $false
$NoModifyPath = $false
$Json = $false
$InstallerTag = if ($env:INSTALLER_TAG) { $env:INSTALLER_TAG } else { "latest" }
$InstallerVersion = ""

for ($i = 0; $i -lt $ArgsList.Count; $i++) {
    switch ($ArgsList[$i]) {
        "--tag" { $Tag = $ArgsList[++$i] }
        "--version" { $Version = $ArgsList[++$i] }
        "--upgrade" { $Upgrade = $true }
        "--check" { $CheckOnly = $true }
        "--no-modify-path" { $NoModifyPath = $true }
        "--json" { $Json = $true }
        "--installer-tag" { $InstallerTag = $ArgsList[++$i] }
        "--installer-version" { $InstallerVersion = $ArgsList[++$i] }
        "-h" { exit 0 }
        "--help" { exit 0 }
        Default {
            Write-Error "Unknown option: $($ArgsList[$i])"
            exit 1
        }
    }
}

if (-not $MirrorUrl -or $MirrorUrl -eq "__MIRROR_URL__") {
    Write-Error "MIRROR_URL is not set"
    exit 1
}

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLower()
switch ($arch) {
    "x64" { $Platform = "win32-x64" }
    "arm64" { $Platform = "win32-arm64" }
    Default {
        Write-Error "Unsupported architecture: $arch"
        exit 1
    }
}

if (-not $InstallerVersion) {
    $InstallerVersion = Invoke-RestMethod "$MirrorUrl/installer/$InstallerTag"
}

$InstallerName = "duckcoding-cli-installer.exe"
$TempDir = Join-Path $env:TEMP "duckcoding-installer-$([guid]::NewGuid().ToString('N').Substring(0,8))"
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
$InstallerPath = Join-Path $TempDir $InstallerName

try {
    Invoke-WebRequest -Uri "$MirrorUrl/installer/$InstallerVersion/$Platform/$InstallerName" -OutFile $InstallerPath
    $ChecksumLine = Invoke-RestMethod "$MirrorUrl/installer/$InstallerVersion/$Platform/checksum.txt"
    $Expected = ($ChecksumLine -split "\s+")[0].ToLower()
    $Actual = (Get-FileHash $InstallerPath -Algorithm SHA256).Hash.ToLower()
    if ($Expected -and $Actual -ne $Expected) {
        Write-Error "Checksum mismatch: expected $Expected, got $Actual"
        exit 1
    }

    $installerArgs = @("--mirror-url", $MirrorUrl, "--install-dir", $InstallDir, "claude-code", "--tag", $Tag)
    if ($Version) { $installerArgs += @("--version", $Version) }
    if ($Upgrade) { $installerArgs += "--upgrade" }
    if ($CheckOnly) { $installerArgs += "--check" }
    if ($NoModifyPath) { $installerArgs += "--no-modify-path" }
    if ($Json) { $installerArgs += "--json" }

    & $InstallerPath @installerArgs
    exit $LASTEXITCODE
} finally {
    if (Test-Path $TempDir) {
        Remove-Item $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
