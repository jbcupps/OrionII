# OrionII

OrionII is a Windows-first Tauri + React desktop companion for the Phoenix Project Orion line.
It runs locally with durable JSON-backed identity, a bicameral Id/Ego pipeline, document
indexing, and routes every model call through SAO's LLM proxy — entity tokens are SAO-issued,
provider keys never live on the entity's disk.

Project status (canonical "what works today"): see [docs/STATUS.md](docs/STATUS.md).

## Stack

- React 19, TypeScript, Vite 8 for the frontend.
- Rust + Tauri 2 for the desktop shell.
- Local Orion core under `src-tauri/src/orion/` (identity, persistence, model router, SAO
  client, birth client).
- SAO integration:
  - `GET /api/orion/birth` (live runtime config — fetched on every launch)
  - `POST /api/llm/generate` (proxied LLM calls — keys stay on SAO)
  - `POST /api/orion/egress` (sanitized event ship)
  - `GET /api/orion/policy` (governance pull)

## Two ways to run OrionII

### 1. Bundle-driven — what real users get

A SAO admin configures provider keys + an installer source; a SAO user creates an entity and
clicks **Download bundle**. The ZIP contains:

- `OrionII-Setup.msi` — this app's installer
- `config.json` — anchor (`sao_base_url` + `agent_token`)
- `README-FIRST-RUN.txt` — install steps

The user runs the MSI and either:

- drops `config.json` into `%APPDATA%\OrionII\config.json`, or
- launches OrionII first and pastes the JSON into the in-app **Enroll with SAO** panel —
  OrionII writes the file and hot-swaps the running core, no restart.

On every launch, OrionII calls `GET /api/orion/birth` to fetch live agent metadata, endpoints,
scopes, current policy, and personality seed. Admin changes in SAO take effect on the next
launch with no re-bundling.

The status card at the top of the window shows one of three modes:

- **birthed** — live SAO connection, real LLM responses; shows owner, provider, models,
  birthed-at.
- **anchor only** — config loaded but the birth call failed; running on bundle defaults.
- **offline** — no anchor at all; deterministic local fallback.

### 2. Dev mode — for working on OrionII itself

```bash
npm ci
npm run tauri dev
```

To run with SAO sync enabled in dev (no bundle), set the env vars:

```powershell
$env:SAO_BASE_URL          = "http://localhost:3100"
$env:SAO_DEV_BEARER_TOKEN  = "<sao-server mint-dev-token output>"
$env:SAO_AGENT_ID          = "<optional-sao-agent-id>"
npm run tauri dev
```

In this mode the bearer is a user JWT (no per-entity scoping) and the model layer talks
directly to local Ollama. Useful for iterating without rebuilding the MSI.

## Building the installer

```powershell
npm ci
npm run tauri build -- --bundles msi
```

Output: `src-tauri/target/release/bundle/msi/OrionII_<version>_x64_en-US.msi`.

This is the artifact SAO's installer-source registry serves. Either:

- Publish via GitHub Actions ([release-installer.yml](.github/workflows/release-installer.yml))
  on tag, then point a SAO installer source at `https://github.com/jbcupps/OrionII/releases/latest/download/<asset>`.
- Or in dev, run a temporary HTTP server on the build dir and probe its sha into a SAO
  installer source — see the SAO runbook.

## Project Layout

- `src/` — React frontend.
  - `App.tsx` — chat UI, SAO sync controls, **Enroll with SAO** paste panel, three-mode status
    card.
- `src-tauri/` — Tauri shell + Rust Orion core.
  - `src/orion/bootstrap.rs` — anchor loader (config.json or env) + birth fetch.
  - `src/orion/birth.rs` — `GET /api/orion/birth` client.
  - `src/orion/identity.rs` — durable companion identity.
  - `src/orion/model.rs` — Id/Ego model router (`Deterministic`, `OllamaWithFallback`,
    `SaoProxyWithFallback`) + `SaoProxyProvider`.
  - `src/orion/sao.rs` — egress shipper + policy client (carries `clientVersion`).
  - `src/lib.rs` — Tauri commands including `apply_bundle_config` (powers the paste UI).
- `docs/` — architecture notes, target-state roadmap, SAO MVP client guide, status.
- `.github/workflows/` — CI (`ci.yml`) and installer release (`release-installer.yml`).

## Validation Gate

```powershell
npm ci
npm run build
cargo check  --manifest-path src-tauri\Cargo.toml --locked
cargo test   --manifest-path src-tauri\Cargo.toml --locked
cargo clippy --manifest-path src-tauri\Cargo.toml --locked --all-targets -- -D warnings
```
