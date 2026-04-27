# `iggy-server` sidecar binaries

Phase 2b ships the **logic** to run a bundled iggy-server sidecar (see
`src-tauri/src/orion/iggy_supervisor.rs`), but the binaries themselves
are not yet vendored in this repo. Auto-download with checksum
verification is a Phase 2.1 packaging ticket.

## How to use Iggy on this branch (dev)

For development, do one of these:

### Option A — install iggy-server on PATH

```bash
# macOS / Linux (cargo)
cargo install iggy-server --version 0.10
# or via the official release binaries from
# https://github.com/apache/iggy/releases
```

```powershell
# Windows
# Download iggy-server.exe from
# https://github.com/apache/iggy/releases
# and place it on %PATH% or alongside OrionII.exe.
```

The supervisor's `locate_binary()` checks `PATH` last, so a
PATH-installed `iggy-server` is automatically picked up.

### Option B — point at a specific binary via env var

```bash
export ORIONII_IGGY_SERVER=/path/to/iggy-server
# Windows: set ORIONII_IGGY_SERVER=C:\path\to\iggy-server.exe
```

The supervisor checks this env var first.

### Option C — drop-in next to OrionII

In a `tauri build` artifact directory, place a file named
`iggy-server` (or `iggy-server.exe` on Windows) next to the OrionII
binary. The supervisor's lookup order resolves it second after the
env var.

## How to switch to the bundled-Iggy bus

In `%APPDATA%\OrionII\config.json` (Windows) or
`~/.config/OrionII/config.json` (Unix), set:

```jsonc
{
  "sao_base_url": "...",
  "agent_token": "...",
  "bus_transport": { "kind": "bundled_iggy" }
}
```

Or, to talk to an externally managed iggy node:

```jsonc
{
  "sao_base_url": "...",
  "agent_token": "...",
  "bus_transport": {
    "kind": "external_iggy",
    "endpoint": "tcp://127.0.0.1:8090",
    "pat": "iggy:iggy"
  }
}
```

If `bus_transport` is omitted, OrionII defaults to in-memory (Phase 1
behavior).

## Phase 2.1 future work

- Add the target-triple-suffixed binaries to this directory (e.g.
  `iggy-server-x86_64-pc-windows-msvc.exe`,
  `iggy-server-aarch64-apple-darwin`, etc.) following Tauri's
  `externalBin` naming convention.
- Register them in `tauri.conf.json` under
  `bundle.externalBin` so `tauri build` packages them.
- Add a `build.rs` that downloads the binaries from the iggy GitHub
  release page and verifies SHA-256 checksums on first build.
- Wire the `tauri::api::process::Command::new_sidecar` resolution
  into `iggy_supervisor::locate_binary` so bundled installs don't
  need PATH or env-var fallback.
