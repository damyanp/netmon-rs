<#
.SYNOPSIS
    Builds the Network Monitor (netmon-rs) Windows installer with Inno Setup.

.DESCRIPTION
    Reads the version from Cargo.toml, builds the release binary (unless
    -SkipBuild), locates the Inno Setup compiler (installing it via winget if
    needed), and compiles installer\netmon-rs.iss into a per-user setup .exe
    under installer\Output\.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1

.EXAMPLE
    # Reuse an existing release build:
    powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$iss = Join-Path $PSScriptRoot 'netmon-rs.iss'

# --- Version from Cargo.toml -------------------------------------------------
$cargoToml = Join-Path $repoRoot 'Cargo.toml'
$match = Select-String -Path $cargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $match) { throw "Could not read 'version' from $cargoToml" }
$version = $match.Matches[0].Groups[1].Value
Write-Host "Version: $version"

# --- Locate (or install) Inno Setup compiler --------------------------------
function Find-ISCC {
    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    )
    foreach ($p in $candidates) { if ($p -and (Test-Path $p)) { return $p } }
    return $null
}

$iscc = Find-ISCC
if (-not $iscc) {
    Write-Host "Inno Setup not found. Installing via winget (JRSoftware.InnoSetup)..."
    winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements
    $iscc = Find-ISCC
    if (-not $iscc) {
        throw "Inno Setup still not found. Install it from https://jrsoftware.org/isdl.php and re-run."
    }
}
Write-Host "ISCC: $iscc"

# --- Build release binary ----------------------------------------------------
if (-not $SkipBuild) {
    Write-Host "Building release binary (cargo build --release)..."
    Push-Location $repoRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $repoRoot 'target\release\netmon-rs.exe'
if (-not (Test-Path $exe)) {
    throw "Release binary not found: $exe. Run without -SkipBuild, or `cargo build --release` first."
}

# --- Compile the installer ---------------------------------------------------
& $iscc "/DAppVersion=$version" $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit $LASTEXITCODE)" }

$out = Join-Path $PSScriptRoot "Output\NetworkMonitor-Setup-$version.exe"
Write-Host ""
Write-Host "Installer built: $out" -ForegroundColor Green
