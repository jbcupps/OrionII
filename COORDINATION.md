# OrionII Coordination

Pointers to the docs that govern the OrionII ↔ SAO integration.

- [`docs/sao-mvp-client.md`](docs/sao-mvp-client.md) — OrionII client behavior (bundle + env paths, birth flow, model router).
- [`docs/STATUS.md`](docs/STATUS.md) — current "what works today" snapshot.
- `C:\Repo\SAO\docs\STATUS.md` — canonical project status across both repos.
- `C:\Repo\SAO\docs\orion-sao-mvp.md` — shared API contract (`/api/orion/*`,
  `/api/llm/generate`, `/api/orion/birth`, `/api/agents/:id/bundle`,
  `/api/agents/:id/events`, entity JWT shape, installer-source registry).
- `C:\Repo\SAO\docs\runbooks\local-orion-sao-mvp.md` — full local end-to-end runbook.

Before declaring the integration green, run the OrionII checks in
[`docs/sao-mvp-client.md`](docs/sao-mvp-client.md) and the SAO checks in the SAO runbook.

## Current implementation

- **Anchor → birth**: OrionII boots from a SAO-issued `config.json` (only `sao_base_url` +
  `agent_token` are required), then calls `GET /api/orion/birth` to fetch live agent
  metadata, endpoints, scopes, current policy, and personality seed. Admin-side changes in
  SAO take effect on the next OrionII launch with no re-bundling.
- **Identity continuity**: on first launch, `CompanionIdentity` adopts the SAO-assigned
  `agent_id` as its `orion_id`. Subsequent launches keep the persisted id (collisions are
  logged).
- **In-app enrollment**: a yellow **Enroll with SAO** panel is visible until birth succeeds.
  Pasting the bundle JSON writes it to `%APPDATA%\OrionII\config.json` and hot-swaps the
  running OrionCore behind the Tauri Mutex — no restart needed. This remains a support/manual
  fallback; the SAO-downloaded ZIP now includes a double-click launcher that writes the config
  automatically for non-technical users.
- **Model router**: `Deterministic`, `OllamaWithFallback`, `SaoProxyWithFallback`. Bundle
  flow picks `SaoProxyWithFallback` so all real model calls flow through SAO and hit
  whichever provider/model the admin configured. Deterministic stub still serves when SAO is
  unreachable.
- **Status surface**: three explicit modes — `birthed`, `anchor only`, `offline` — surfaced
  in the desktop UI status card with owner / provider / id-model / ego-model / birthed-at.
- **Egress observability**: every batch carries `clientVersion` (OrionII semver), and chat
  flow publishes a correlated `egress.outbound` audit envelope after `ego.action`.
- **Durable bus transport**: OrionII can run its entity-internal bus on `in_memory`,
  `nats_jetstream`, `external_nats_jetstream`, or the experimental Iggy adapters via
  `config.json` `bus_transport`. Release MSI builds run `scripts/build-installer.ps1` to
  prepare/package the `nats-server` JetStream sidecar by default, and SAO remains outside
  this bus.
- **SurrealDB** remains a target architecture item, not in MVP.
- **Local SAO bootstrap** is a Compose/dev command, not the production Azure installer path.

## Identity story

The entity bearer is an OIDC-shaped JWT (`iss=sao`, `sub=<agent_id>`,
`principal_type=non_human`, `human_owner=<creating user_id>`, `entity_kind=orion`,
`scope=orion:policy orion:egress llm:generate`). The shape is intentionally portable so
future issuance from Microsoft Entra or another external IdP is a swap of the issuance and
verification path; the on-the-wire bearer contract stays the same.
