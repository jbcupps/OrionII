# OrionII Coordination

This file points at the docs that govern the OrionII ↔ SAO integration.

- [`docs/sao-mvp-client.md`](docs/sao-mvp-client.md) — OrionII client behavior (bundle + env paths).
- `C:\Repo\SAO\docs\orion-sao-mvp.md` — shared API contract (`/api/orion/*`,
  `/api/llm/generate`, `/api/agents/:id/bundle`, `/api/agents/:id/events`, entity JWT shape).
- `C:\Repo\SAO\docs\runbooks\local-orion-sao-mvp.md` — full local end-to-end runbook (admin
  configures keys → user creates entity → bundle download → install → live chat → events visible).

Before calling the integration green, run the OrionII checks in `docs/sao-mvp-client.md` and the
SAO checks in the SAO runbook.

## Current implementation

- OrionII boots from a SAO-issued `config.json` when present (`%APPDATA%\OrionII\config.json` or
  next to the executable). Falls back to env vars in dev.
- Identity continuity: on first launch, `CompanionIdentity` adopts the SAO-assigned `agent_id`
  as its `orion_id`. Subsequent launches keep the persisted id (collisions are logged).
- Model router supports `Deterministic`, `OllamaWithFallback`, and `SaoProxyWithFallback`. The
  bundle path picks `SaoProxyWithFallback` so all real model calls flow through SAO and hit
  whichever provider the admin configured.
- Egress payloads now stamp `clientVersion` (OrionII semver) on every batch.
- Iggy and SurrealDB remain target architecture items, not in MVP.
- Local SAO bootstrap is a Compose/dev command, not the production Azure installer path.

## Identity story

The entity bearer is an OIDC-shaped JWT (issuer `sao`, `principal_type=non_human`,
`human_owner=<creating user_id>`, `entity_kind=orion`). The shape is intentionally portable so
future issuance from Microsoft Entra or another external IdP is a swap of the issuance and
verification path; the on-the-wire bearer contract stays the same.
