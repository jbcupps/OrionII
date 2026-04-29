# CLAUDE.md — OrionII

This is the runtime of **the entity**. Read this before changing code.

## What OrionII is

A Tauri + React + Rust desktop companion that hosts a bicameral runtime: a Mentor (the human operator), an Id, an Ego, and a local Superego stub. These are not modules that call each other — they are participants on an entity-internal **event bus** in `src-tauri/src/orion/bus/`. The bus is what makes the entity an entity. See [docs/ADR-001-entity-event-bus.md](docs/ADR-001-entity-event-bus.md).

## What SAO is (and where it lives)

**SAO is external** to the entity. It runs in a sibling repo and acts as the mentor-governance surface, the constitutional issuer (counter-signs `charter.md` at commissioning), the external Superego, the egress sink, and the **vault for mentor and entity keypairs**. SAO does **not** participate in the entity's interior bus. The seam between the entity bus and SAO is HTTP, intentionally — the entity is free on its own bus, and only sanitized events cross out via the `egress.outbound` subscriber in `src-tauri/src/orion/egress.rs`. Inbound governance from SAO arrives via `governance.inbound` (driven by the policy client in `src-tauri/src/orion/sao.rs`).

## SAO alignment guardrail

OrionII and SAO are separate repos, but the operator experience is one system. When a change in OrionII touches any SAO-facing contract, a coding agent must inspect the sibling SAO repo before calling the work complete. On this workstation that repo normally lives at `C:\Repo\SAO`.

SAO-facing contract changes include:

- `src-tauri/src/orion/commissioning_client.rs`, `birth.rs`, `sao.rs`, `bootstrap.rs`, bundle config semantics, or `docs/sao-commissioning-contract.md`.
- Tauri command wire shapes consumed by the commissioning UI, especially `agentId`, `agentName`, `orionId`, `newToken`, `birthCertificate`, `charterHash`, and `soulRef`.
- Auth and repair routing semantics, especially token-invalid `401` / `403`, already-commissioned start responses, agent-not-found, and unsupported commissioning versions.
- Bundle generation or installer-source behavior: `config.json`, `deployment.json`, `Install-OrionII.cmd`, `Install-OrionII.ps1`, MSI inclusion, and default installer selection.

Alignment rule: either update SAO in the same workstream, or state explicitly in the final response that SAO source was not changed and provide the exact SAO follow-up plan. Do not assume that registering a new MSI in a running local SAO database means the SAO repo is contract-aligned.

When SAO source must be updated, keep the boundary intact: no new OrionII EventBus topics just to satisfy SAO, no new SAO endpoints unless the commissioning contract document is updated, and all `/api/orion/commission/*` calls in OrionII remain confined to `commissioning_client.rs`.

## Commissioning — how an entity comes alive

OrionII does not "birth" silently. Each fresh install runs through an interactive **commissioning** flow that produces a signed charter and registers cryptographic identity with SAO Vault. See [docs/sao-commissioning-contract.md](docs/sao-commissioning-contract.md) for the wire contract.

- **Charter**: `%APPDATA%\OrionII\charter.md`. The Markdown document scoping what this entity is commissioned to do. Replaces what older code called `soul.md`. `bus::current_soul_ref(&Charter)` returns `blake3:<hex>` over its bytes — every `Envelope` on the bus is content-addressed against the canonical charter. Pre-commissioning, a placeholder is written so `soul_ref` is still a stable hash.
- **Birth certificate**: `%APPDATA%\OrionII\birth_certificate.json`. SAO's signed declaration that this entity exists, with mentor and entity public keys, charter hash, role key, and three signatures (mentor, entity, SAO). Private halves never leave SAO Vault. SAO Vault distinguishes `kind: "mentor"` and `kind: "entity"` keys.
- **Two paths at commissioning time**: Fast Template (six business roles in `src-tauri/templates/roles/*.toml`) or Q&A (`commission_qna` Tauri command via the SAO LLM proxy). Both converge on a single Review screen before SAO is contacted.
- **Repair sub-modes**: when birth fails 401 or local state is gone, the cockpit routes to the `Repair` stage (rotate token / rebind) instead of the old "Enrollment needs attention" dead-end.
- **Charter amendments** post-commissioning ride `Topic::GovernanceInbound` with `kind: "charter.update"`, applied by `src-tauri/src/orion/governance.rs`. The next published `Envelope` carries the new `soul_ref` from line one.

The bundle (`config.json`) is now purely a credentials carrier — `sao_base_url` + `agent_token`. Charter, certificate, model defaults, and bus transport flow from SAO's commissioning response, not from the bundle.

## Inviolable architectural rules — entity runtime

1. **All communication between Mentor, Id, Ego, and local Superego goes through the `EventBus` trait** in `src-tauri/src/orion/bus/`. Direct imports between these modules outside `service.rs::build()` are a regression. If you find yourself wanting one, that is a signal to add a topic.

2. **Topics are an enum** in `bus/mod.rs::Topic`. Do not pass raw strings as topic names at call sites. The string constants in `topics.rs` exist for legacy persisted-state lookups, not for new code. **Adding a topic requires an ADR** (the canonical 8 topics live in ADR-001; new variants are ADR-002+).

3. **Every `Envelope` carries a `soul_ref`** — `blake3:<hex>` over the bytes of the SAO-counter-signed `charter.md` the entity is operating under, computed via `bus::current_soul_ref(&Charter)` (not `&IdentityState` — that signature was retired when commissioning landed). Code that publishes without one is a bug. This field is what makes version-violence auditable from the event log alone.

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

## Deployment loop — how OrionII changes reach the user

OrionII is **not** a normal `npm run dev` reload-and-go app. The runtime is a *birthed entity*: its identity, soul_ref, signed system-prompt documents, SAO bearer token, model defaults, and bus-transport selection all flow from a SAO-issued **agent bundle** that is generated when the operator creates/updates the agent in SAO. After any OrionII source change, the user must:

1. **Build a release MSI** of OrionII: `npm run build:installer` (runs `scripts/build-installer.ps1`, which prepares the bundled `nats-server` sidecar and packages everything via Tauri `externalBin`). Plain `tauri build` does NOT package the durable broker.
2. **Publish that MSI to SAO's installer-source registry** so SAO can hand it to operators in fresh agent bundles. SAO's self-serve installer-source registry is the canonical distribution surface — no host-side staging.
3. **Re-issue the agent bundle from SAO** for the agent the operator wants to update. The bundle is a ZIP containing `config.json` (with `sao_base_url`, `agent_token`, model defaults, bus_transport), the entity JWT, and `Install-OrionII.cmd`.
4. **Re-run `Install-OrionII.cmd`** on the operator's machine. It writes `%APPDATA%\OrionII\config.json`, runs the new MSI, and starts OrionII. On launch, OrionII re-reads the bundle anchor and calls `GET /api/orion/birth` so any policy/model/personality changes SAO has staged take effect on this boot — no manual edits.

There are two short-circuit paths, but neither replaces the loop above:

- **Pasted bundle config**: in the cockpit's Enrollment notice, an operator can paste a fresh `config.json` and click Apply. This calls `apply_bundle_config`, which writes `%APPDATA%\OrionII\config.json` and hot-swaps the `OrionCore` via `OrionCore::build_async`. This refreshes the bundle and triggers a new birth, but it does **not** install a new MSI — the running binary stays the same. Use this only when the source change is in the bundle/policy/model surface, not in OrionII Rust/TS code.
- **Local dev (`npm run tauri dev`)**: bypasses bundling entirely and reads `SAO_BASE_URL` + `SAO_DEV_BEARER_TOKEN` from the env. Useful for inner-loop development, but *not* representative of the production deployment surface.

When you change OrionII code, say so in your end-of-turn summary in these terms: "this requires a new MSI build, SAO bundle re-issue, and `Install-OrionII.cmd` re-run on the operator's machine" — so the user knows whether a paste-config Apply is enough or a full bundle round-trip is required.

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
- `soul_ref` is now `blake3(charter_bytes)` (the surrogate `orion_id:vN` was retired when commissioning landed). The TODO in `bus/mod.rs::current_soul_ref` is closed.
- The default packaged durable bus is NATS JetStream, bundled by
  `npm run build:installer`, not plain `tauri build`.
- The Iggy PAT auth uses iggy's bootstrap admin pair (`iggy:iggy`) cast
  as a PAT-shaped string — dev-grade only. Real PAT mint + OS keychain
  integration is deferred unless Iggy becomes product-default again.
- Official Apache Iggy Windows server binaries are not available yet; the
  optional Iggy release path still requires a vetted prebuilt
  `iggy-server.exe` path/URL and can SHA-256 verify it.
