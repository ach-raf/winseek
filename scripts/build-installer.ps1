<#
.SYNOPSIS
  Build the one-click Windows installer: target\release\bundle\nsis\winseek_<version>_x64-setup.exe
.DESCRIPTION
  1. npm ci for the UI and the Tauri CLI (only when node_modules is missing)
  2. cargo build --release -p searchd   (shipped as an installer resource next to the app)
  3. tauri build (NSIS): builds the UI with Vite (beforeBuildCommand), the app in release with the
     custom protocol (no dev server), and the per-machine NSIS installer whose hooks register the
     searchd service (app\src-tauri\installer-hooks.nsh).
  The installer is unsigned (SmartScreen: "More info" -> "Run anyway"). There is no updater.
  Silent install for testing: <setup.exe> /S /R   (/R = launch the app afterwards as the user)
.NOTES
  PowerShell 5.1 compatible. Run from any prompt (no elevation needed to build):
    powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
#>
param([switch]$SkipNpm)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root 'app'

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Run($exe, [string[]]$argv, $cwd) {
    Push-Location $cwd
    try {
        & $exe @argv
        if ($LASTEXITCODE -ne 0) { throw "$exe $($argv -join ' ') failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
}

if (-not $SkipNpm) {
    if (-not (Test-Path (Join-Path $App 'node_modules\.bin\tauri.cmd'))) { Step 'npm ci (Tauri CLI)'; Run 'npm.cmd' @('ci') $App }
    if (-not (Test-Path (Join-Path $App 'ui\node_modules'))) { Step 'npm ci (UI)'; Run 'npm.cmd' @('ci') (Join-Path $App 'ui') }
}

Step 'cargo build --release -p searchd'
Run 'cargo' @('build', '--release', '-p', 'searchd') $Root

Step 'cargo build --release -p winseek-mcp'
Run 'cargo' @('build', '--release', '-p', 'winseek-mcp') $Root

Step 'cargo build --release -p winseek-cli'
Run 'cargo' @('build', '--release', '-p', 'winseek-cli') $Root

Step 'tauri build (UI + app + NSIS installer)'
Run 'npx.cmd' @('tauri', 'build', '--bundles', 'nsis') $App

$Setup = Get-ChildItem (Join-Path $Root 'target\release\bundle\nsis') -Filter '*-setup.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $Setup) { throw 'no *-setup.exe under target\release\bundle\nsis' }
Step ("installer: {0} ({1:N1} MB)" -f $Setup.FullName, ($Setup.Length / 1MB))
