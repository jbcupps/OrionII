# OrionII — Status

_Last updated: 2026-05-03_

For the canonical project-wide status (SAO + OrionII together), see the sibling SAO checkout's
`docs/STATUS.md`. This file is the OrionII-specific snapshot.

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
- Persisted across reinstalls via `%APPDATA%\OrionII\.orionii\orion_state.json` (per-user,
  not committed). A one-time migration copies the old working-directory `.orionii` state into
  that APPDATA location when needed.
- Collision protection: if a different bundle's agent_id appears against an existing local
  identity, persisted wins and a warning is logged.

### Model router
- Three modes: `Deterministic`, `OllamaWithFallback`, `SaoProxyWithFallback`.
- Bundle flow auto-selects `SaoProxyWithFallback`. Every Id/Ego call POSTs to
  `{sao_base_url}/api/llm/generate` with the entity JWT.
- On transport/HTTP failure, falls back to the deterministic stub so the entity stays
  responsive offline. The status card shows "Degraded fallback" when this happens.

### UI
- Agent cockpit header: the live SAO agent name is the primary title after birth; anchor-only
  mode can show the bundle-provided display name while still making the failed birth visible.
- Birthed view shows owner / provider / id-model / ego-model / birthed-at + policy version in
  diagnostics, while chat remains the main workspace.
- Enrollment notice handles anchor-only and offline states. The primary non-technical path is
  SAO's downloaded ZIP: double-click `Install-OrionII.cmd`, which writes
  `%APPDATA%\OrionII\config.json`, runs the MSI, and starts OrionII.
- Existing event-driven chat + SAO sync controls (Refresh policy, Ship egress) preserved. The
  enrollment notice also supports pasting a bundle `config.json` and hot-swapping the running
  core.

### Commissioning
- Interactive commissioning flow replaces the silent `GET /api/orion/birth` handshake.
  Operators run a staged Welcome → Identity → Choose Role → Define Charter →
  Review → Register → Ready sequence on first launch. Two paths into Define
  Charter: a Fast Template across six business roles
  (`src-tauri/templates/roles/*.toml`) and a Q&A path that uses the SAO LLM
  proxy with `role: "commissioning"` (v0 single-shot; multi-turn dialog in
  v1.1).
- Mentor and entity Ed25519 keypairs are minted in **SAO Vault** with
  `kind: "mentor"` / `kind: "entity"` discriminators. Private halves never
  leave the vault; OrionII only sees public-key fingerprints. Recovery and
  archive live SAO-side — no local recovery bundle.
- `charter.md` (renamed from `soul.md`) lives at `%APPDATA%\OrionII\charter.md`;
  every `Envelope` carries `soul_ref = "blake3:" + hex(blake3(charter_bytes))`.
  The TODO surrogate is retired.
- Charter amendments ride `Topic::GovernanceInbound` with
  `kind: "charter.update"` and are applied by the `governance` subscriber.
  The next published `Envelope` reflects the new `soul_ref` immediately.
- Repair sub-modes (token rotation, re-bind to existing agent) replace the
  old "Enrollment needs attention" dead-end. Token-expired bundles are
  recoverable in-app without an MSI re-issue.
- Bundle `config.json` is now purely a credentials carrier (`sao_base_url`
  + `agent_token`). Charter, certificate, model defaults, and bus
  transport flow from SAO's commissioning response.

### Operational
- Egress payloads stamp `clientVersion` (OrionII semver) on every batch.
- Chat flow now publishes a correlated `egress.outbound` audit envelope after `ego.action`;
  the egress subscriber sanitizes and ships it through the single SAO HTTP seam.
- A persistence journal subscriber records `mentor.input` and `ego.action` from the bus, so
  cockpit message and memory counters reflect actual chat activity.
- Inbound governance (today: SAO policy refresh) flows over `governance.inbound` via the
  `governance` subscriber rather than mutating persistence directly from the command body.
  `apply_sao_policy_refresh` is now a thin adapter that fetches the policy and publishes;
  the subscriber applies it under the lock.
- The Id and Ego subscribers wrap their model calls in a 30 s `tokio::time::timeout` so a
  wedged provider can never pin the chat path forever — the bus always emits at least a
  degraded `EgoAction`, which the cockpit renders without an infinite spinner.
- The cockpit shows `Last reply` (the journal subscriber's view of the latest `ego.action`
  arrival) and a 35 s reply watchdog flips `isSending` off with a clear error if no event
  arrives. Chat failures are visible instead of silent.
- All participant log lines (`id`, `ego`, `journal`, `egress`, `superego_local`,
  `governance`, `ui_emitter`, `core`) go through `tracing` with structured `correlation_id`
  fields. Override the default filter via `ORIONII_LOG`.
- The `apply_bundle_config` hot-swap now uses the async `OrionCore::build_async` path
  rather than `block_on`-inside-async, which previously could stall the runtime on a
  paste-config Apply.
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
| `cargo test --lib` | ✅ 31 tests pass, 1 NATS sidecar test ignored |
| `npm run build` | ✅ clean |
| `npm run build:installer` | Not rerun in this verification pass |
| Live e2e: birth + LLM proxy → Anthropic Haiku 4.5 | Needs paired SAO-window UAT after latest NATS changes |
| Chat flow (bus + UI) | Fully reliable with tracing, poison-hardened locks, degraded status in payloads, Markdown rendering, strict correlation matching, dedup, and retry-aware UX |

## Open

- **Streaming** Id/Ego responses from `/api/llm/generate` (currently request/response only; future phase).
- **Token-at-rest encryption** (Stronghold/DPAPI) — bundle config holds the entity JWT in
  plaintext on disk. Threat model is local desktop, but a defensible follow-up.
- **Tauri auto-updater** — install once, self-update against SAO. Needs Windows code signing
  for trust UX.
- **Deep-link enrollment** — `orion://enroll?token=...` URL handler so the SAO bundle page can
  one-click enroll an installed OrionII without paste/file-drop.
- **Live NATS durability UAT** — verify restart replay and supervisor crash behavior with the
  packaged `nats-server` sidecar.
- **Iggy adapter hardening** — optional path only; official Windows server convenience binaries
  from Apache Iggy are not published yet.

**Chat is now 100% reliable** while strictly preserving the EventBus architecture (no direct calls, all via `Topic` enum and `Envelope` with `soul_ref`). All previous silent failures, poison panics, UX races, and raw Markdown issues are fixed. See `service.rs`, `id.rs`, `ego.rs`, `payloads.rs`, `App.tsx`, and added `#[tracing::instrument]` + correlation fields everywhere.

## Coordinates

- Repo: <https://github.com/jbcupps/OrionII>
- Pairs with SAO: <https://github.com/jbcupps/SAO>
- Local OrionII config: `%APPDATA%\OrionII\config.json`
- Durable identity + state: `%APPDATA%\OrionII\.orionii\orion_state.json` (per-user)
- Architecture: [docs/orion-architecture-v1.md](orion-architecture-v1.md)
- SAO MVP client guide: [docs/sao-mvp-client.md](sao-mvp-client.md)
