$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Resolve-Path (Join-Path $scriptDir "..\..")
$exe = Join-Path $root "o2a-desktop-new.exe"
if (-not (Test-Path $exe)) {
    $exe = Join-Path $root "o2a-desktop.exe"
}
if (-not (Test-Path $exe)) {
    Write-Error "o2a-desktop-new.exe / o2a-desktop.exe not found: $exe"
    exit 1
}

Write-Host "Starting stable desktop app: $exe"
Start-Process -FilePath $exe

$desktopDir = Join-Path $root "desktop"
Write-Host "Starting Tauri dev app (pnpm tauri dev)..."
Push-Location $desktopDir
try {
    pnpm tauri dev
} finally {
    Pop-Location
}