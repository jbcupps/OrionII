# OrionII

OrionII is a Windows-first Tauri + React desktop companion for the Phoenix Project Orion line. It
runs locally with durable JSON-backed identity, a bicameral Id/Ego pipeline, document indexing,
and either a local Ollama fallback or a SAO-hosted LLM proxy depending on how it was provisioned.

## Stack

- React 19, TypeScript, Vite for the frontend.
- Rust + Tauri 2 for the desktop shell.
- Local Orion core under `src-tauri/src/orion/` (identity, persistence, model router, SAO client).
- SAO integration via:
  - `POST /api/llm/generate` (proxied LLM calls — keys never leave SAO)
  - `GET /api/orion/policy` (governance pull)
  - `POST /api/orion/egress` (sanitized event ship)

## Two ways to run OrionII

### 1. Bundle-driven — what real users get

A SAO admin configures provider keys; a SAO user creates an entity and clicks **Download bundle**.
The ZIP contains:

- `OrionII-Setup.msi` — this app's installer
- `config.json` — the entity's identity token and chosen LLM defaults
- `README-FIRST-RUN.txt` — install steps

The user runs the MSI and drops `config.json` into `%APPDATA%\OrionII\`. On launch OrionII adopts
the SAO-assigned identity and routes all model calls through the SAO LLM proxy. See
[docs/sao-mvp-client.md](docs/sao-mvp-client.md) for the bundle contract.

### 2. Dev mode — for working on OrionII itself

Install Node, npm, and the Rust toolchain, then:

```bash
npm ci
npm run tauri dev
```

To run with SAO sync enabled in dev (no bundle), set the env vars:

```powershell
$env:SAO_BASE_URL          = "http://localhost:3100"
$env:SAO_DEV_BEARER_TOKEN  = "<token-from-sao-mint-dev-token>"
$env:SAO_AGENT_ID          = "<optional-sao-agent-id>"
npm run tauri dev
```

In this mode the bearer is a user JWT (no per-entity scoping) and the model layer talks directly
to local Ollama. Useful for iterating without rebuilding the MSI.

## Building the installer

The SAO bundle endpoint serves a real MSI. Produce one with:

```powershell
npm ci
npm run tauri build -- --bundles msi
```

Output: `src-tauri/target/release/bundle/msi/OrionII_<version>_x64_en-US.msi`.

Tell SAO where it lives so the bundle endpoint can serve it (see SAO runbook).

## Project Layout

- `src/` — React frontend.
- `src-tauri/` — Tauri shell and Rust Orion core.
  - `src-tauri/src/orion/bootstrap.rs` — config loader (bundle-aware).
  - `src-tauri/src/orion/identity.rs` — durable companion identity.
  - `src-tauri/src/orion/model.rs` — Id/Ego model router (`Deterministic`,
    `OllamaWithFallback`, `SaoProxyWithFallback`).
  - `src-tauri/src/orion/sao.rs` — egress shipper and policy client.
- `docs/` — architecture notes, target-state roadmap, SAO MVP client guide.
- `.github/workflows/` — CI (`ci.yml`) and installer release (`release-installer.yml`).

## Validation Gate

```powershell
npm ci
npm run build
cargo check  --manifest-path src-tauri\Cargo.toml --locked
cargo test   --manifest-path src-tauri\Cargo.toml --locked
cargo clippy --manifest-path src-tauri\Cargo.toml --locked --all-targets -- -D warnings
```
