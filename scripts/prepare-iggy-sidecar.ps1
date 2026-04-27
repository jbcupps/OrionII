param(
    [string]$IggyRef = "server-0.7.0",
    [string]$IggyRepo = "https://github.com/apache/iggy.git",
    [string]$PrebuiltPath = "",
    [string]$PrebuiltUrl = "",
    [string]$ExpectedSha256 = "",
    [switch]$AllowSourceBuild,
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$binDir = Join-Path $repoRoot "src-tauri\binaries"
$sourceDir = Join-Path $repoRoot ".sidecars\iggy-src"
$downloadDir = Join-Path $repoRoot ".sidecars\downloads"
$targetExe = Join-Path $binDir "iggy-server-$Target.exe"
$devExe = Join-Path $binDir "iggy-server.exe"
$isWindowsOs = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)

New-Item -ItemType Directory -Force -Path $binDir | Out-Null

function Copy-Sidecar {
    param([string]$Source)

    if (-not [string]::IsNullOrWhiteSpace($ExpectedSha256)) {
        $actual = (Get-FileHash -Algorithm SHA256 -Path $Source).Hash.ToLowerInvariant()
        if ($actual -ne $ExpectedSha256.ToLowerInvariant()) {
            throw "iggy-server sha256 mismatch. Expected $ExpectedSha256, got $actual."
        }
    }

    Copy-Item -Force $Source $targetExe
    Copy-Item -Force $Source $devExe
    Write-Host "Prepared Tauri sidecar:"
    Write-Host "  $targetExe"
    Write-Host "  $devExe"
}

if ([string]::IsNullOrWhiteSpace($PrebuiltPath)) {
    $PrebuiltPath = $env:ORIONII_IGGY_SERVER
}
if ([string]::IsNullOrWhiteSpace($PrebuiltUrl)) {
    $PrebuiltUrl = $env:ORIONII_IGGY_SERVER_URL
}
if ([string]::IsNullOrWhiteSpace($ExpectedSha256)) {
    $ExpectedSha256 = $env:ORIONII_IGGY_SERVER_SHA256
}

if (-not [string]::IsNullOrWhiteSpace($PrebuiltPath)) {
    $resolvedPrebuilt = (Resolve-Path $PrebuiltPath).Path
    Write-Host "Using prebuilt iggy-server from $resolvedPrebuilt"
    Copy-Sidecar -Source $resolvedPrebuilt
    exit 0
}

if (-not [string]::IsNullOrWhiteSpace($PrebuiltUrl)) {
    New-Item -ItemType Directory -Force -Path $downloadDir | Out-Null
    $downloaded = Join-Path $downloadDir "iggy-server.exe"
    Write-Host "Downloading prebuilt iggy-server from $PrebuiltUrl"
    Invoke-WebRequest -Uri $PrebuiltUrl -OutFile $downloaded
    Copy-Sidecar -Source $downloaded
    exit 0
}

if ($isWindowsOs -and -not $AllowSourceBuild) {
    throw "Windows Iggy sidecar packaging requires a prebuilt iggy-server.exe. Set ORIONII_IGGY_SERVER or ORIONII_IGGY_SERVER_URL (+ ORIONII_IGGY_SERVER_SHA256), or pass -AllowSourceBuild to try Apache Iggy source anyway. The current Apache Iggy server source path is not reliable on clean Windows runners."
}

Write-Host "Building iggy-server from $IggyRepo ($IggyRef) for $Target..."

if (Test-Path $sourceDir) {
    git -C $sourceDir fetch --depth 1 origin "refs/tags/${IggyRef}:refs/tags/${IggyRef}"
    git -C $sourceDir checkout --force $IggyRef
} else {
    New-Item -ItemType Directory -Force -Path (Join-Path $repoRoot ".sidecars") | Out-Null
    git clone --depth 1 --branch $IggyRef $IggyRepo $sourceDir
}

Push-Location $sourceDir
try {
    cargo build --release --bin iggy-server --locked --features hwlocality/vendored
    if ($LASTEXITCODE -ne 0) {
        throw "Iggy source build failed with exit code $LASTEXITCODE."
    }
} finally {
    Pop-Location
}

$installedExe = Join-Path $sourceDir "target\release\iggy-server.exe"

if (-not (Test-Path $installedExe)) {
    throw "Iggy source build completed, but $installedExe was not produced."
}

Copy-Sidecar -Source $installedExe
