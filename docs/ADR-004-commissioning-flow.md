# ADR-004 — Interactive commissioning replaces silent birth

_Status: Accepted, 2026-04-28_

## Context

The pre-2026-04 birth handshake was a silent `GET /api/orion/birth` from
`bootstrap.rs` against the bundled JWT. If SAO returned 200 the entity
came up; on any other status the cockpit rendered "Enrollment needs
attention" with no path forward. Three failures masked the same root
cause:

- A 401 (revoked or rotated token) trapped the operator in an "anchor
  only" state that required re-issuing the bundle and reinstalling — a
  full MSI round-trip — even though the actual problem was a single
  expired bearer.
- The `soul_ref` carried on every `Envelope` was a surrogate
  (`orion_id:vN`) because no charter had ever been signed; the
  version-violence guardrail was vapourware.
- The agent's purpose, scope, and boundaries lived only inside SAO. An
  operator looking at OrionII saw an opaque "Agent 80e3b373…8b42" with no
  business-grade explanation of what it was for.

The user-experience concepts in the sibling **Abigail** project — staged
onboarding, fast-path templates, a single editable charter document,
visible cryptographic identity — solved the legibility problem but were
oriented around mystical/ceremonial framing and a fully local trust
chain. OrionII needs the same shape recast for a business audience and
anchored in SAO Vault rather than a local Hive.

## Decision

Replace silent birth with an interactive **commissioning** flow:

1. **Charter, not soul.** Rename the document concept from `soul.md` to
   `charter.md`. Tone is business-direct ("this entity is chartered to
   triage Calendar X for Organization Y"), not personality-driven.
2. **Two paths into the charter.** A Fast Template flow with six
   pre-built business roles (Calendar Assistant, Technical Document
   Reader/Writer, Email Triage & Drafting, Research Analyst, Project
   Coordinator, Compliance Reviewer) and a Q&A path that asks the
   operator to describe what they need and drafts a charter via the SAO
   LLM proxy.
3. **SAO Vault holds every private key.** Mentor and entity Ed25519
   keypairs are minted server-side and stored in SAO Vault with explicit
   `kind: "mentor"` / `kind: "entity"` discriminators. OrionII never
   sees private bytes. Public-key fingerprints surface in the
   commissioning UI for transparency; recovery is SAO-archive-driven.
4. **Charter ↔ envelope binding.** `bus::current_soul_ref(&Charter)`
   returns `blake3:<hex>` over the on-disk `charter.md` bytes. Every
   `Envelope::new` callsite reads through this helper, so a charter
   amendment is visible from the event log alone.
5. **Repair sub-modes.** When SAO returns 401 or local state is missing,
   the cockpit routes to a Repair stage with two clear options
   (Refresh credentials, Re-bind to existing agent) rather than the old
   dead-end notice. Token rotation is recoverable in-app — no MSI
   round-trip required.
6. **Bundle simplification.** `config.json` is now purely a credentials
   carrier: `sao_base_url` + `agent_token`. Every legacy field is parsed
   for back-compat but ignored; SAO's commissioning response is the
   authoritative source for charter, models, and bus transport.

The full handshake and endpoint contract live in
[sao-commissioning-contract.md](sao-commissioning-contract.md).

## Consequences

### Positive

- The user-visible failure mode in the screenshot ("HTTP 401: Invalid or
  expired token" with no recovery) is solved without an installer
  re-roll.
- `soul_ref` becomes content-addressed for the first time — the
  guardrail in ADR-001 is now enforced rather than aspirational.
- Charter amendments naturally flow over the bus
  (`Topic::GovernanceInbound`, `kind: "charter.update"`) parallel to the
  policy-refresh path that landed alongside this work, so the bus
  remains the integration boundary.
- Operators in a business setting get a visible, editable description of
  what the agent does. Compliance, audit, and onboarding stories are now
  documentable instead of buried in SAO.

### Negative / costs

- SAO grows three new endpoints (`commission/start`, `commission/finalize`,
  `commission/repair`) plus an extension to the existing `/api/orion/birth`
  read. Vault admin UI grows mentor/entity key surfacing per agent. This
  is sibling-repo work and gates the OrionII-side rollout.
- The Q&A path ships v0 single-shot. Multi-turn dialog with the
  `[CHARTER_READY]` end-token contract specified in the wire doc is
  deferred to v1.1 — the wire endpoint and `role: "commissioning"`
  discriminator are reused unchanged when it lands.
- Pre-2026-04 bundles still load (every legacy field has `#[serde(default)]`)
  but their values are ignored. Operators with old bundles enter the
  first-launch commissioning flow on next install; existing commissioned
  agents are unaffected because `charter.md` and `birth_certificate.json`
  on disk satisfy the `commissioned` state check.

### Architectural invariants preserved

- Tauri commands stay thin (`commission_*` bodies are ≤ ~30 lines and
  delegate to `commissioning_client.rs`).
- The bus remains the only path between participants. Commissioning
  uses HTTP for SAO and the bus for charter amendments — never both for
  the same operation.
- `SaoShipper` continues to be referenced only from `egress.rs`. The new
  `commissioning_client` module is a separate HTTP surface; it does not
  touch the egress queue.

## Alternatives considered

- **Local keypairs registered with SAO.** Rejected: a business-grade
  recovery story requires the vault on the SAO side anyway, and dual
  storage (local OS keychain + SAO archive) doubled the audit surface
  without adding meaningful safety.
- **Wizard inside the existing cockpit.** Rejected: collapsing the chat
  surface and the commissioning surface into one screen made both worse.
  The commissioning UI is its own gated route.
- **Ceremonial framing à la Abigail (Sovereign Birth Sequence,
  Emergence Ceremony).** Rejected for OrionII's business audience —
  copy is direct ("Agent commissioned") rather than reverential.
- **Multi-mentor at v1.** Out of scope. One mentor per OrionII install
  in v1; multi-mentor co-signing is a future ADR.
