# OrionII — Status

_Last updated: 2026-04-27_

For the canonical project-wide status (SAO + OrionII together), see
[`C:\Repo\SAO\docs\STATUS.md`](https://github.com/jbcupps/SAO/blob/feat/orion-entity-bundle-llm-proxy/docs/STATUS.md).
This file is the OrionII-specific snapshot.

## Where we are

The desktop entity reliably boots from a SAO-issued bundle, dynamically self-configures via
the birth event, routes Id/Ego prompts through SAO's LLM proxy, and runs Mentor, Id, Ego, local
Superego, UI emission, and SAO egress through the entity-internal `EventBus`. NATS JetStream is
now the packaged durable bus transport behind `bus_transport`; release MSI builds run
`scripts/build-installer.ps1` to prepare and package the `nats-server` sidecar through Tauri
`externalBin`. The earlier Iggy adapter remains experimental.

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
- **Enroll with SAO** yellow panel — legacy/manual fallback for pasting bundle JSON. The primary
  non-technical path is SAO's downloaded ZIP: double-click `Install-OrionII.cmd`, which writes
  `%APPDATA%\OrionII\config.json`, runs the MSI, and starts OrionII.
- Existing chat + SAO sync controls (Refresh policy, Ship egress) preserved.

### Operational
- Egress payloads stamp `clientVersion` (OrionII semver) on every batch.
- Chat flow now publishes a correlated `egress.outbound` audit envelope after `ego.action`;
  the egress subscriber sanitizes and ships it through the single SAO HTTP seam.
- Bus transport is selectable with `config.json` `bus_transport`: `in_memory`,
  `nats_jetstream`, `external_nats_jetstream`, `bundled_iggy`, or `external_iggy`. Release
  builds run `scripts/build-installer.ps1` so bundled installs have the local NATS JetStream
  sidecar while normal developer `cargo check` remains sidecar-free.
- Compatible with the SAO self-serve installer-source registry — installs end-to-end without
  any host-side MSI staging.

## Verification

| Gate | Status |
|---|---|
| `cargo check` | ✅ clean |
| `cargo clippy --all-targets -- -D warnings` | ✅ clean |
| `cargo test --lib` | ✅ 24 tests pass, 0 ignored |
| `npm run build` | ✅ clean |
| `npm run build:installer` | Not rerun in this verification pass |
| Live e2e: birth + LLM proxy → Anthropic Haiku 4.5 | Needs paired SAO-window UAT after latest NATS changes |

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
- **Live NATS durability UAT** — verify restart replay and supervisor crash behavior with the
  packaged `nats-server` sidecar.
- **Iggy adapter hardening** — optional path only; official Windows server convenience binaries
  from Apache Iggy are not published yet.

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
