#Requires -Version 5.1
$ErrorActionPreference = "Stop"

if (-not $env:MIRROR_URL) {
    throw "MIRROR_URL is required"
}

$InstallDir = "$env:USERPROFILE\.duckcoding"
$BinDir = "$InstallDir\bin"

function Invoke-RemoteScript {
    param(
        [Parameter(Mandatory=$true)][string]$Url,
        [string[]]$ScriptArgs = @()
    )

    $tmp = Join-Path $env:TEMP ([IO.Path]::GetRandomFileName() + ".ps1")
    Invoke-WebRequest -Uri $Url -OutFile $tmp -UseBasicParsing
    $content = Get-Content $tmp -Raw
    $content = $content -replace "__MIRROR_URL__", $env:MIRROR_URL
    Set-Content -Path $tmp -Value $content -NoNewline
    & $tmp @ScriptArgs
    Remove-Item $tmp -Force
}

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
    Invoke-RemoteScript -Url "$env:MIRROR_URL/$Name/install.ps1" -ScriptArgs @("--no-modify-path")

    Write-Host "==> Version check: $Bin"
    & $Bin --version

    Write-Host "==> TUI check: $Bin"
    Invoke-TuiCheck $Bin

    Write-Host "==> Uninstalling $Name"
    Invoke-RemoteScript -Url "$env:MIRROR_URL/$Name/uninstall.ps1" -ScriptArgs $UninstallArgs

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
