# OrionII — Status

_Last updated: 2026-04-26_

For the canonical project-wide status (SAO + OrionII together), see
[`C:\Repo\SAO\docs\STATUS.md`](https://github.com/jbcupps/SAO/blob/feat/orion-entity-bundle-llm-proxy/docs/STATUS.md).
This file is the OrionII-specific snapshot.

## Where we are

The desktop entity reliably boots from a SAO-issued bundle, dynamically self-configures via
the birth event, and routes Id/Ego prompts through SAO's LLM proxy. Verified live against
Anthropic Haiku 4.5 — the OrionII chat panel shows real Claude responses.

## Shipped

### Bootstrap
- **Anchor**: `config.json` with only `sao_base_url` + `agent_token` required (everything
  else is optional fallback). Looked up at `%APPDATA%\OrionII\config.json` then next to the
  exe, with env-var fallback (`SAO_BASE_URL` + `SAO_DEV_BEARER_TOKEN`) for the dev path.
- **Birth**: `GET /api/orion/birth` on every launch. Response overrides bundle defaults so
  admin changes (provider switch, model swap, policy update) take effect on the next OrionII
  boot with no re-bundling.

### Identity
- `CompanionIdentity` adopts the SAO-assigned `agent_id` as its `orion_id` on first launch.
- Persisted across reinstalls via `%APPDATA%\OrionII\.orionii\orion_state.json` (per-machine,
  not committed).
- Collision protection: if a different bundle's agent_id appears against an existing local
  identity, persisted wins and a warning is logged.

### Model router
- Three modes: `Deterministic`, `OllamaWithFallback`, `SaoProxyWithFallback`.
- Bundle flow auto-selects `SaoProxyWithFallback`. Every Id/Ego call POSTs to
  `{sao_base_url}/api/llm/generate` with the entity JWT.
- On transport/HTTP failure, falls back to the deterministic stub so the entity stays
  responsive offline. The status card shows "Degraded fallback" when this happens.

### UI
- Three-mode status header: **birthed** (live SAO), **anchor only** (config loaded but birth
  failed), **offline** (no anchor).
- Birthed view shows owner / provider / id-model / ego-model / birthed-at + policy version.
- **Enroll with SAO** yellow panel — pastes the bundle JSON, validates, writes it, hot-swaps
  OrionCore. Disappears once birthed.
- Existing chat + SAO sync controls (Refresh policy, Ship egress) preserved.

### Operational
- Egress payloads stamp `clientVersion` (OrionII semver) on every batch.
- Compatible with the SAO self-serve installer-source registry — installs end-to-end without
  any host-side MSI staging.

## Verification

| Gate | Status |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | ✅ clean |
| `cargo test` | ✅ 18 tests pass |
| `npm run build` | ✅ clean |
| `npm run tauri build -- --bundles msi` | ✅ produces working MSI |
| Live e2e: birth + LLM proxy → Anthropic Haiku 4.5 | ✅ real responses in chat |

## Open

- **Markdown rendering** in the chat output bubble — Claude returns `**bold**`/lists; today
  they show as raw asterisks. One-line drop-in for `react-markdown`.
- **Streaming** Id/Ego responses from `/api/llm/generate` (currently request/response only).
- **Token-at-rest encryption** (Stronghold/DPAPI) — bundle config holds the entity JWT in
  plaintext on disk. Threat model is local desktop, but a defensible follow-up.
- **Tauri auto-updater** — install once, self-update against SAO. Needs Windows code signing
  for trust UX.
- **Deep-link enrollment** — `orion://enroll?token=...` URL handler so the SAO bundle page can
  one-click enroll an installed OrionII without paste/file-drop.
- **First-run UX polish** — make the Enroll panel even more discoverable (e.g., focus the
  textarea on launch when offline).

## Open PRs

- [#1 fix/tauri-bundle-icon](https://github.com/jbcupps/OrionII/pull/1) — wires
  `bundle.icon` so the MSI build doesn't fail on a missing .ico (one-line tactical PR).
- [#2 feat/dynamic-bootstrap-and-paste-ui](https://github.com/jbcupps/OrionII/pull/2) — birth
  client + paste UI + status modes. Stacked on #1; merge #1 first.

## Coordinates

- Repo: <https://github.com/jbcupps/OrionII>
- Pairs with SAO: <https://github.com/jbcupps/SAO> ([PR #18](https://github.com/jbcupps/SAO/pull/18))
- Local OrionII config: `%APPDATA%\OrionII\config.json`
- Durable identity + state: `<exe-dir>\.orionii\orion_state.json` (per-machine)
- Architecture: [docs/orion-architecture-v1.md](orion-architecture-v1.md)
- SAO MVP client guide: [docs/sao-mvp-client.md](sao-mvp-client.md)
