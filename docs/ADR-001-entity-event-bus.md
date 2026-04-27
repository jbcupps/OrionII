# ADR-001: Entity-internal event bus is the integration boundary

## Status

Accepted. 2026-04-26.

## Context

OrionII is the runtime of the entity. The entity's bicameral structure — Mentor (the human operator), Id, Ego, with a local Superego stub — is an *internal* property of this runtime. SAO is external: it is the mentor-governance surface, the constitutional issuer (signs `soul.md` at birth), the external Superego, and the egress sink. SAO does not participate in the entity's interior.

If Mentor, Id, and Ego communicate through direct function calls inside OrionII, the entity is not a thing — it is a chat passthrough with extra structure. The bus is what makes the entity an entity.

The seam between the entity bus and SAO is HTTP, and is intentional. The entity is free on its own bus. Governance reaches the entity only through `Topic::GovernanceInbound` (policy pulls from SAO, driven by the policy client in `orion/sao.rs`) and observes the entity only through `Topic::EgressOutbound` (sanitized events shipped to SAO via `POST /api/orion/egress`). NPPI sanitization sits on `EgressOutbound` (Phase 1: key-name redaction; full NPPI in a follow-up).

## Decision

All inter-component communication inside OrionII flows through the `EventBus` trait in `src-tauri/src/orion/bus/`. Phase 1 default is `InMemoryBus` (tokio broadcast). A later phase will swap to a local Iggy node without touching callers.

The eight canonical topics are:

| Topic | Purpose |
|---|---|
| `mentor.input` | User input from the human operator |
| `id.stimulus` | External sensory input (file drops, future web ingestion) |
| `id.reaction` | Id's pre-reflective response after the curator pipeline |
| `ego.deliberation` | Ego's reasoning trace |
| `ego.action` | Ego's chosen output (what the UI renders) |
| `superego.local.evaluation` | Local Superego stub's evaluation against cached `soul_ref` |
| `egress.outbound` | Sanitized event ready to ship to SAO |
| `governance.inbound` | Policy / proposal / external-Superego evaluation pulled from SAO |

New topics require an ADR. Resurrected interrupt/agent-task topics (dropped from the legacy `topics.rs` during this consolidation) come back as ADR-002+ when those features re-land.

The Mentor adapter, Id, Ego, and local Superego are bus participants. None of them may import from any other directly. The UI subscribes to `ego.action` (via the `orion://ego.action` Tauri event) for chat output; it does not consume the `send_chat_message` command return for the assistant reply.

### Phase 1 surrogate: `Envelope.soul_ref`

Every `Envelope` carries a `soul_ref` field — the hash of the SAO-signed `soul.md` the entity is currently operating under. SAO does not yet ship a signed `soul.md` blob at birth, so Phase 1 uses a surrogate: `format!("{}:v{}", orion_id, identity_version)`, computed by `bus::current_soul_ref(&IdentityState)`.

When SAO begins shipping a signed `soul.md` at birth and the entity caches it, replace the surrogate with `hex(blake3(soul_md_bytes))` in that single helper. No other change required — every callsite that constructs an `Envelope` calls `current_soul_ref()`. This is the version-violence guardrail expressed as a property of every event on the bus.

## Consequences

**+** The bicameral structure is visible in code shape, not just docs. A coding agent reading the repo sees publish/subscribe as the unit of work, not "add another Tauri command."

**+** Auditability: every internal event carries `agent_id`, `occurred_at`, and `soul_ref`, making version violence detectable from the event log alone.

**+** The entity ↔ SAO seam is a single sanitization point (`egress.outbound` subscriber in `orion/egress.rs`), not scattered across the codebase. The Phase 1 sanitizer redacts `secret|token|key|password` keys; richer NPPI policy plugs in here without changing where it sits.

**+** Phase 2 swap to Iggy is a back-end change in `bus/mod.rs`'s implementation, not a rewrite. No caller depends on the concrete bus type — all callers depend on the `SharedBus = Arc<dyn EventBus>` type alias.

**+** Hot-swapping `OrionCore` (via `apply_bundle_config`) drops the old bus's senders, and old subscribers exit cleanly via `RecvError::Closed`. New subscribers run on the new bus. Explicit `JoinHandle::abort()` in `OrionCore::Drop` keeps shutdown deterministic.

**−** One day of upfront refactor. Took the form of: collapsing the 11-topic legacy vocabulary to the canonical 8; wrapping `FilePersistence` and `ModelRouter` in `Arc<Mutex<…>>` / `Arc<…>` to share between subscribers; inverting the UI to consume `ego.action` events instead of awaiting command returns.

**−** The integration test in `service.rs` (`mentor_input_round_trips_to_ego_action`) is currently `#[ignore]`d. The blocker is `reqwest::blocking::Client` inside `OllamaModelProvider` and `SaoProxyProvider` — it carries an internal tokio runtime that panics when dropped inside an outer async context. Resolving this means converting those providers to `reqwest`'s async client, which is **out of scope** for this ADR but is the most natural follow-up.

## Out of scope (separate tickets)

- Real NPPI sanitizer logic (Phase 1 ships a key-name redaction stub).
- Real local Superego evaluation logic (Phase 1 stub records the `soul_ref` and accepts everything).
- Iggy transport (Phase 2; trait makes it a back-end swap).
- SAO-side bus (separate repo, separate decision).
- Real `soul.md` hashing on disk (Phase 1 uses the orion_id + identity-version surrogate).
- Async model layer (currently blocking `reqwest`; follow-up unblocks the integration test).
- `mentor.rs` adapter module (the Tauri command body in `lib.rs` is already the adapter; promoting it to its own file is cosmetic — defer until there's more than one mentor entry point).

## Verification

End-to-end, in order:

1. `cargo check` in `src-tauri/` — compiles clean. The topic rename surfaces every legacy callsite as a compile error; all are fixed.
2. `cargo test --lib` — 21 passing, 1 ignored (documented in `service.rs`).
3. `npm run build` — TypeScript + Vite build clean.
4. `npm run tauri dev` — type "hello" in the chat composer:
   - User message appears immediately (from `send_chat_message` ack).
   - Orion response appears via the `orion://ego.action` listener, **not** the command return.
   - Console shows the egress subscriber attempting `POST /api/orion/egress` (will 404 in dev — that's expected; the seam is what we verify).
5. Architecture visibility: `git grep -nE "use crate::orion::(id|ego|curator|superego_local|egress)" src-tauri/src` returns matches only inside `service.rs::build()` (the spawn calls). Direct cross-participant imports outside that file are the regression signal.
