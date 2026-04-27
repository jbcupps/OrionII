param(
    [string]$NatsVersion = "2.12.7",
    [string]$PrebuiltPath = "",
    [string]$PrebuiltUrl = "",
    [string]$ExpectedSha256 = "",
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$binDir = Join-Path $repoRoot "src-tauri\binaries"
$downloadDir = Join-Path $repoRoot ".sidecars\downloads"
$targetExe = Join-Path $binDir "nats-server-$Target.exe"
$devExe = Join-Path $binDir "nats-server.exe"

New-Item -ItemType Directory -Force -Path $binDir | Out-Null

function Test-Sha256 {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($ExpectedSha256)) {
        return
    }

    $actual = (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
    if ($actual -ne $ExpectedSha256.ToLowerInvariant()) {
        throw "nats-server sha256 mismatch. Expected $ExpectedSha256, got $actual."
    }
}

function Copy-Sidecar {
    param([string]$Source)

    Test-Sha256 -Path $Source
    Copy-Item -Force $Source $targetExe
    Copy-Item -Force $Source $devExe
    Write-Host "Prepared Tauri sidecar:"
    Write-Host "  $targetExe"
    Write-Host "  $devExe"
}

function Expand-NatsArchive {
    param([string]$ArchivePath)

    $extractDir = Join-Path $downloadDir "nats-server-$NatsVersion"
    if (Test-Path $extractDir) {
        Remove-Item -Recurse -Force $extractDir
    }
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null

    Expand-Archive -Force -Path $ArchivePath -DestinationPath $extractDir
    $candidate = Get-ChildItem -Path $extractDir -Recurse -Filter "nats-server.exe" |
        Select-Object -First 1
    if ($null -eq $candidate) {
        throw "Downloaded NATS archive did not contain nats-server.exe."
    }
    return $candidate.FullName
}

if ([string]::IsNullOrWhiteSpace($PrebuiltPath)) {
    $PrebuiltPath = $env:ORIONII_NATS_SERVER
}
if ([string]::IsNullOrWhiteSpace($PrebuiltUrl)) {
    $PrebuiltUrl = $env:ORIONII_NATS_SERVER_URL
}
if ([string]::IsNullOrWhiteSpace($ExpectedSha256)) {
    $ExpectedSha256 = $env:ORIONII_NATS_SERVER_SHA256
}

if (-not [string]::IsNullOrWhiteSpace($PrebuiltPath)) {
    $resolvedPrebuilt = (Resolve-Path $PrebuiltPath).Path
    Write-Host "Using prebuilt nats-server from $resolvedPrebuilt"
    Copy-Sidecar -Source $resolvedPrebuilt
    exit 0
}

New-Item -ItemType Directory -Force -Path $downloadDir | Out-Null

if ([string]::IsNullOrWhiteSpace($PrebuiltUrl)) {
    $PrebuiltUrl = "https://github.com/nats-io/nats-server/releases/download/v$NatsVersion/nats-server-v$NatsVersion-windows-amd64.zip"
}

$leafName = Split-Path -Leaf ([System.Uri]$PrebuiltUrl).AbsolutePath
if ([string]::IsNullOrWhiteSpace($leafName)) {
    $leafName = "nats-server.zip"
}
$downloaded = Join-Path $downloadDir $leafName
Write-Host "Downloading nats-server from $PrebuiltUrl"
Invoke-WebRequest -Uri $PrebuiltUrl -OutFile $downloaded
Test-Sha256 -Path $downloaded

if ($downloaded.EndsWith(".zip", [System.StringComparison]::OrdinalIgnoreCase)) {
    $extracted = Expand-NatsArchive -ArchivePath $downloaded
    Copy-Sidecar -Source $extracted
}
else {
    Copy-Sidecar -Source $downloaded
}
