# CLAUDE.md — OrionII

This is the runtime of **the entity**. Read this before changing code.

## What OrionII is

A Tauri + React + Rust desktop companion that hosts a bicameral runtime: a Mentor (the human operator), an Id, an Ego, and a local Superego stub. These are not modules that call each other — they are participants on an entity-internal **event bus** in `src-tauri/src/orion/bus/`. The bus is what makes the entity an entity. See [docs/ADR-001-entity-event-bus.md](docs/ADR-001-entity-event-bus.md).

## What SAO is (and where it lives)

**SAO is external** to the entity. It runs in a sibling repo and acts as the mentor-governance surface, the constitutional issuer (signs `soul.md` at birth), the external Superego, and the egress sink. SAO does **not** participate in the entity's interior bus. The seam between the entity bus and SAO is HTTP, intentionally — the entity is free on its own bus, and only sanitized events cross out via the `egress.outbound` subscriber in `src-tauri/src/orion/egress.rs`. Inbound governance from SAO arrives via `governance.inbound` (driven by the policy client in `src-tauri/src/orion/sao.rs`).

## Inviolable architectural rules — entity runtime

1. **All communication between Mentor, Id, Ego, and local Superego goes through the `EventBus` trait** in `src-tauri/src/orion/bus/`. Direct imports between these modules outside `service.rs::build()` are a regression. If you find yourself wanting one, that is a signal to add a topic.

2. **Topics are an enum** in `bus/mod.rs::Topic`. Do not pass raw strings as topic names at call sites. The string constants in `topics.rs` exist for legacy persisted-state lookups, not for new code. **Adding a topic requires an ADR** (the canonical 8 topics live in ADR-001; new variants are ADR-002+).

3. **Every `Envelope` carries a `soul_ref`** — the hash of the SAO-signed `soul.md` the entity is operating under, computed via `bus::current_soul_ref(&IdentityState)`. Code that publishes without one is a bug. This field is what makes version-violence auditable from the event log alone.

4. **The entity ↔ SAO seam is HTTP.** Sanitized events leave only via the `egress.outbound` subscriber. Inbound governance arrives only via the `governance.inbound` publisher. Do not add other bridges. Do not call `SaoShipper` from outside `egress.rs`.

## Dev commands

```bash
# Rust
cd src-tauri && cargo check
cd src-tauri && cargo test --lib

# Frontend
npm install
npm run build              # tsc + vite build
npm run tauri dev          # full app
```

## Key files

- `src-tauri/src/orion/bus/mod.rs` — `EventBus` trait, `Topic` enum, `Envelope`, `SharedBus`, `current_soul_ref`.
- `src-tauri/src/orion/bus/inmem.rs` — Phase 1 in-process implementation over `tokio::sync::broadcast`.
- `src-tauri/src/orion/{id,ego,superego_local,egress}.rs` — bus participants. Each exposes a `spawn(...)` function that returns a `JoinHandle`.
- `src-tauri/src/orion/service.rs` — `OrionCore` constructs the bus and spawns the participants in `OrionCore::build()`. This is the only place participant modules are imported together.
- `src-tauri/src/lib.rs` — Tauri command bodies are thin adapters. `send_chat_message` publishes to `mentor.input` and returns a correlation id; the chat reply arrives later on the `orion://ego.action` Tauri event.
- `src/App.tsx` — `useEffect` listens for `orion://ego.action` and appends the orion message to the transcript. Do not move chat output back into the command return.

## Phase 1 known limitations

- The integration test in `service.rs` (`mentor_input_round_trips_to_ego_action`) is `#[ignore]`d because `OllamaModelProvider` / `SaoProxyProvider` use `reqwest::blocking::Client`, whose nested tokio runtime panics on drop inside an async test. Converting those to async `reqwest` is the natural unblocker.
- The egress sanitizer is a key-name-redaction stub (`secret|token|key|password`). Real NPPI policy is a follow-up — but it plugs into the same seam.
- `soul_ref` is a surrogate (`orion_id:vN`). When SAO ships a signed `soul.md` blob at birth, swap to `hex(blake3(...))` in `bus::current_soul_ref` — every callsite already uses the helper.
