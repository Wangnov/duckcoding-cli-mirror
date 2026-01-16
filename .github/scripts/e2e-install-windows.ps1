#Requires -Version 5.1
$ErrorActionPreference = "Stop"

if (-not $env:MIRROR_URL) {
    throw "MIRROR_URL is required"
}

$InstallDir = "$env:USERPROFILE\.duckcoding"
$BinDir = "$InstallDir\bin"
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot ".." "..")

function Invoke-TuiCheck {
    param(
        [Parameter(Mandatory=$true)][string]$Path
    )

    $proc = $null
    if ($Path.ToLower().EndsWith(".cmd")) {
        $proc = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", "`"$Path`"" -PassThru
    } else {
        $proc = Start-Process -FilePath $Path -PassThru
    }

    Start-Sleep -Seconds 2
    if ($proc.HasExited) {
        throw "TUI check failed: $Path exited with $($proc.ExitCode)"
    }
    Stop-Process -Id $proc.Id -Force
}

function Run-Cli {
    param(
        [Parameter(Mandatory=$true)][string]$Name,
        [Parameter(Mandatory=$true)][string]$Bin,
        [string[]]$UninstallArgs = @()
    )

    Write-Host "==> Installing $Name"
    Invoke-WebRequest -Uri "$env:MIRROR_URL/$Name/install.ps1" -UseBasicParsing | Out-Null
    $installScript = Join-Path $RepoRoot ("scripts\" + $Name + "-install.ps1")
    & $installScript @("--no-modify-path")

    Write-Host "==> Version check: $Bin"
    & $Bin --version

    Write-Host "==> TUI check: $Bin"
    Invoke-TuiCheck $Bin

    Write-Host "==> Uninstalling $Name"
    Invoke-WebRequest -Uri "$env:MIRROR_URL/$Name/uninstall.ps1" -UseBasicParsing | Out-Null
    $uninstallScript = Join-Path $RepoRoot ("scripts\" + $Name + "-uninstall.ps1")
    & $uninstallScript @UninstallArgs

    if (Test-Path $Bin) {
        throw "Uninstall check failed: $Bin still exists"
    }
}

Run-Cli -Name "claude-code" -Bin "$BinDir\claude.exe"
Run-Cli -Name "codex" -Bin "$BinDir\codex.exe"
if ($env:SKIP_GEMINI -eq "1") {
    Write-Host "Skipping gemini: SKIP_GEMINI=1"
} else {
    Run-Cli -Name "gemini" -Bin "$BinDir\gemini.cmd" -UninstallArgs @("-Yes")
}
