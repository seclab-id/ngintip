# Jalankan Cegat di Windows (padanan run.sh).
#   .\run.ps1          -> mode dev (hot reload)
#   .\run.ps1 build    -> bundle release (.msi / .exe)
param([ValidateSet('dev', 'build')][string]$Mode = 'dev')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

foreach ($cmd in 'cargo', 'node', 'npm') {
  if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
    Write-Error "'$cmd' tidak ditemukan. Install Rust (https://rustup.rs) dan Node.js 22.12+ (https://nodejs.org)."
  }
}

if (-not (Test-Path node_modules) -or (Get-Item package-lock.json).LastWriteTime -gt (Get-Item node_modules).LastWriteTime) {
  Write-Host '-> npm install'
  npm install
}

if ($Mode -eq 'dev') {
  npm run tauri dev
} else {
  npm run tauri build
  Write-Host 'Hasil bundle: src-tauri\target\release\bundle\'
}
