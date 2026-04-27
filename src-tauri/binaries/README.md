# Broker sidecar binaries

Release installer builds prepare a local broker sidecar here, then enable
Tauri `externalBin` only for the MSI build. The default `tauri.conf.json`
intentionally does not list external binaries so clean-machine `cargo check`
does not require a prepared sidecar.

## Product default: NATS JetStream

`npm run build:installer` runs:

```powershell
.\scripts\build-installer.ps1
```

By default that script runs `scripts/prepare-nats-sidecar.ps1`, downloads a
Windows NATS release ZIP if needed, extracts `nats-server.exe`, and copies it
using Tauri's sidecar naming convention:

```text
nats-server-x86_64-pc-windows-msvc.exe
```

The installer overlay includes it through `bundle.externalBin`, so the MSI
installs the broker together with `orionii.exe`. OrionII starts it with
JetStream enabled, bound to `127.0.0.1`, with data in
`%APPDATA%\OrionII\nats`.

Development overrides:

```powershell
$env:ORIONII_NATS_SERVER = "C:\path\to\nats-server.exe"

# or download a pinned artifact
$env:ORIONII_NATS_SERVER_URL = "https://github.com/nats-io/nats-server/releases/download/v2.12.7/nats-server-v2.12.7-windows-amd64.zip"
$env:ORIONII_NATS_SERVER_SHA256 = "<expected-sha256>"
npm run build:installer
```

Config:

```json
{
  "bus_transport": { "kind": "nats_jetstream", "port": 4222 }
}
```

## Experimental: Iggy

The earlier Iggy adapter and supervisor remain in the codebase, but Iggy is
not the default Windows product sidecar because official Apache Iggy Windows
server convenience binaries are not published yet.

To build an Iggy installer explicitly:

```powershell
npm run build:installer:iggy
```

On Windows this requires `ORIONII_IGGY_SERVER` or `ORIONII_IGGY_SERVER_URL`
plus optional `ORIONII_IGGY_SERVER_SHA256`. The source build escape hatch is
still available through `scripts/build-installer.ps1 -BusSidecar iggy
-AllowIggySourceBuild`, but it is not reliable on clean Windows runners.
