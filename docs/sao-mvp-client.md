# SAO MVP Client

This document describes how OrionII connects to SAO at `C:\Repo\SAO`.

There are two integration paths:

1. **Bundle-driven (production-shaped)** — User downloads a SAO-issued bundle that drops a
   `config.json` with an entity JWT next to (or above) the installer. OrionII reads it on launch,
   adopts the SAO-assigned identity, and routes all model calls through `/api/llm/generate`.
2. **Env-driven (legacy / dev)** — `SAO_BASE_URL` + `SAO_DEV_BEARER_TOKEN` env vars wired to a
   user JWT. Useful for iterating on OrionII without rebuilding the MSI.

The bundle path is preferred for any flow you want to mirror what real users experience.

## Bundle-driven path

### Bundle contents

```
config.json            -- anchor: SAO base URL + entity identity token
OrionII-Setup.msi      -- Tauri installer (Windows)
README-FIRST-RUN.txt   -- install steps for the user
```

### `config.json` shape

Only `sao_base_url` and `agent_token` are required — they are the anchor OrionII needs to
reach SAO and call birth. Everything else is fallback for offline mode + back-compat with
older clients.

```json
{
  "sao_base_url":      "http://localhost:3100",
  "agent_id":          "1c9d0fb8-0b2c-4c1e-99a7-...",
  "agent_name":        "abigail-main",
  "agent_token":       "eyJhbGciOiJIUzI1NiJ9...<JWT>",
  "client_version_min": "0.1.0",
  "fallback": {
    "default_provider":  "anthropic",
    "default_id_model":  "claude-haiku-4-5-20251001",
    "default_ego_model": "claude-haiku-4-5-20251001"
  },
  "default_provider":  "anthropic",
  "default_id_model":  "claude-haiku-4-5-20251001",
  "default_ego_model": "claude-haiku-4-5-20251001"
}
```

### How the user gets the config in place

Two equivalent paths:

1. **Filesystem drop** — copy `config.json` to `%APPDATA%\OrionII\config.json` (or co-locate
   it with `OrionII.exe`) before launching.
2. **In-app paste UI** — launch OrionII first; the yellow **Enroll with SAO** panel is
   visible until birth succeeds. Paste the JSON, click **Apply config**. OrionII validates,
   writes the file to `%APPDATA%\OrionII\config.json`, and hot-swaps the running OrionCore —
   no restart. Backed by the Tauri command `apply_bundle_config(json)` in `lib.rs`.

`agent_token` is an OIDC-shaped JWT (`principal_type=non_human`, `human_owner=<user_id>`,
`scope=orion:policy orion:egress llm:generate`). Treat it like any API key — local-disk only,
not committable.

### Where OrionII looks for it

In order:

1. `%APPDATA%\OrionII\config.json` — canonical location; the install README tells the user to drop
   it here.
2. `<exe-dir>\config.json` — co-located with the executable, useful for portable runs.
3. **Env-var fallback** (legacy path below).

### What the bundle changes at runtime

- **Identity adoption** — On first launch (no APPDATA `orion_state.json` yet),
  `CompanionIdentity` adopts the bundle's `agent_id` as its `orion_id`. If a state file already
  exists with a different id, the persisted id wins and a warning is logged (collision = user
  reinstalled into a different bundle). Older working-directory `.orionii` state is copied into
  `%APPDATA%\OrionII\.orionii\orion_state.json` once when the APPDATA file is missing.
- **Model router** — `ModelProviderKind::SaoProxyWithFallback` is selected. Id/Ego prompts go to
  `POST {sao_base_url}/api/llm/generate` with `Authorization: Bearer <agent_token>`. The body
  carries `provider` and `model`; SAO dispatches to the right upstream (OpenAI / Anthropic / Grok
  / Gemini / Ollama). The entity never holds upstream keys and never makes a direct call to a
  provider — SAO is always in the middle. On transport failure the deterministic fallback
  responds so the entity stays useful offline.
- **Egress + policy** — `SaoShipper` is built with the bundle's bearer/agent_id/base_url.

## Env-driven path (legacy / dev)

OrionII reads these from the environment for the legacy MVP flow:

- `SAO_BASE_URL` — SAO API origin, for example `http://localhost:3100`.
- `SAO_DEV_BEARER_TOKEN` — user JWT minted by `sao-server mint-dev-token`.
- `SAO_AGENT_ID` — optional SAO agent id to include with egress batches.

If `SAO_BASE_URL` or `SAO_DEV_BEARER_TOKEN` is missing AND no `config.json` is found, OrionII stays
offline-safe: policy refresh uses local defaults, egress records remain pending, and the model
router falls back to local Ollama (or deterministic if Ollama is unreachable).

To mint a dev token:

```powershell
cd C:\Repo\SAO
$env:POSTGRES_PASSWORD = "local-dev-only-change-me"
$env:SAO_JWT_SECRET = "local-dev-only-change-me"
$env:SAO_LOCAL_BOOTSTRAP = "true"
$token = docker compose -f docker\docker-compose.yml run --rm sao sao-server mint-dev-token | Select-Object -Last 1
```

## Egress contract

OrionII batches pending `SaoEgressRecord` values to `POST /api/orion/egress`. Each request now
also carries `clientVersion` (the OrionII semver) so SAO can pin which client revisions are in
the field. Each record id is the idempotency key; a record is marked `Acked` only when SAO
returns `acked` or `duplicate` for that id. Acked records are compacted from local state after
the ship completes so `orion_state.json` does not grow unbounded.

## Retry behavior

- Missing configuration does not ack records.
- Network failures, non-success HTTP, malformed responses do not ack records.
- Successful per-event acknowledgements are persisted locally by the caller.

## Development commands

```powershell
cd C:\Repo\OrionII
npm ci
npm run build
cargo check --manifest-path src-tauri\Cargo.toml --locked
cargo test --manifest-path src-tauri\Cargo.toml --locked
cargo clippy --manifest-path src-tauri\Cargo.toml --locked --all-targets -- -D warnings

# Build the MSI used by the SAO bundle endpoint
npm run tauri build -- --bundles msi
```

## Related SAO docs

- `C:\Repo\SAO\docs\STATUS.md` — canonical project-wide status.
- `C:\Repo\SAO\docs\orion-sao-mvp.md` — shared API contract (now includes the
  `/api/orion/birth` shape and the installer-source registry).
- `C:\Repo\SAO\docs\runbooks\local-orion-sao-mvp.md` — full local end-to-end runbook,
  including admin installer-source flow and the `SAO_VAULT_PASSPHRASE` auto-unseal step.
- `C:\Repo\SAO\scripts\local-mvp-smoke.ps1` — smoke test (extended to exercise provider + bundle).
- [`docs/STATUS.md`](STATUS.md) — OrionII-side status snapshot.
