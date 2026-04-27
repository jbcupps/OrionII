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
- `src-tauri/src/lib.rs` — Tauri command bodies are thin adapters. `send_chat_message` publishes to `mentor.input` and returns a correlation id; the chat reply arrives later on the `orion://ego/action` Tauri event.
- `src/App.tsx` — `useEffect` listens for `orion://ego/action` and appends the orion message to the transcript. Do not move chat output back into the command return.

## Bus transport

OrionII can run the entity bus on an in-memory broadcast channel
(default for dev/offline), a bundled NATS JetStream sidecar (product
durable path), or the earlier Iggy adapters (experimental until a
reliable Windows `iggy-server.exe` distribution exists). The transport is
selected per-entity in `config.json` via the `bus_transport` field — see
ADR-003.

```jsonc
// In-memory (default; same as Phase 1)
"bus_transport": { "kind": "in_memory" }

// Bundled nats-server sidecar with JetStream (durable product path)
"bus_transport": { "kind": "nats_jetstream", "port": 4222 }

// Bundled iggy-server sidecar (experimental)
"bus_transport": { "kind": "bundled_iggy", "port": 8090 }

// External NATS JetStream node (advanced)
"bus_transport": { "kind": "external_nats_jetstream", "endpoint": "nats://127.0.0.1:4222" }

// External iggy node (advanced)
"bus_transport": { "kind": "external_iggy", "endpoint": "tcp://127.0.0.1:8090", "pat": "iggy:iggy" }
```

When `nats_jetstream` is selected, OrionII spawns a `nats-server` child
process with JetStream enabled, bound to `127.0.0.1`, and stores data in
`{config_dir}/OrionII/nats`. Release MSI builds use
`scripts/build-installer.ps1` to download/prepare and package that
sidecar through Tauri `externalBin`; development/release can point
`ORIONII_NATS_SERVER` at a prebuilt binary or set
`ORIONII_NATS_SERVER_URL` plus `ORIONII_NATS_SERVER_SHA256`. The
supervisor restarts the sidecar on crash up to 3 times in 60s, then
publishes a `GovernanceInbound` envelope with `kind: "broker-unstable"`.

The Iggy path remains available for adapter work, but it is no longer the
default product packaging path on Windows.

Personal Access Tokens live at `{config_dir}/OrionII/iggy_pat` (mode
600). The `rotate_iggy_token` Tauri command rotates them. Phase 2b's
PAT-mint flow is a stand-in (`TODO(phase-2b-pat-mint)` in
`iggy_auth.rs`); the real server-side mint + revoke is Phase 2.1.

Any failure on a durable broker path falls back to the in-memory bus with
a diagnostic log. The entity stays alive even when the broker doesn't.

## Phase 1 / 2a / 2b known limitations

- The egress sanitizer is a key-name-redaction stub (`secret|token|key|password`). Real NPPI policy is a follow-up — but it plugs into the same seam.
- `soul_ref` is a surrogate (`orion_id:vN`). When SAO ships a signed `soul.md` blob at birth, swap to `hex(blake3(...))` in `bus::current_soul_ref` — every callsite already uses the helper.
- The default packaged durable bus is NATS JetStream, bundled by
  `npm run build:installer`, not plain `tauri build`.
- The Iggy PAT auth uses iggy's bootstrap admin pair (`iggy:iggy`) cast
  as a PAT-shaped string — dev-grade only. Real PAT mint + OS keychain
  integration is deferred unless Iggy becomes product-default again.
- Official Apache Iggy Windows server binaries are not available yet; the
  optional Iggy release path still requires a vetted prebuilt
  `iggy-server.exe` path/URL and can SHA-256 verify it.
