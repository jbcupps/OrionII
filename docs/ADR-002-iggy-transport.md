# ADR-002: Iggy as the durable EventBus transport

## Status

Accepted. 2026-04-27.

## Context

ADR-001 made the entity's bicameral structure visible in code shape via the `EventBus` trait. Phase 1 / 2a shipped the trait with a single in-process implementation (`InMemoryBus`, tokio broadcast). The bus survives within a session, but every restart of OrionII evaporates in-flight envelopes — pending `EgressOutbound`, mid-flight `EgoAction`, replay history. That is incompatible with the entity-continuity claim ADR-001 makes: if the bus dies with the process, the auditable event log is only as durable as the current run.

Phase 2b makes the bus persistent. Apache Iggy is a Rust-native message-streaming platform; we run an `iggy-server` sidecar locally, and OrionII speaks to it over TCP using the `iggy` client crate. Topics map 1:1 onto Iggy topics inside one stream named after the entity (`orionii.entity.{orion_id}`). Envelopes are JSON-serialized so they remain debuggable from `iggy-cli`.

This is not a perf change. It is a continuity change: when the OrionII process dies, the in-flight envelopes are still on disk in iggy's data directory; when OrionII restarts, its consumer-group offset resumes where it left off, and the egress subscriber picks up unshipped events. `soul_ref` provenance becomes auditable across restarts, not just within a session.

## Decision

### Bus selection

`OrionBootstrap` gains a `bus_transport: BusTransport` field, defaulting to `InMemory`:

```rust
pub enum BusTransport {
    InMemory,
    BundledIggy { port: u16 },
    ExternalIggy { endpoint: String, pat: String },
}
```

`OrionCore::build_async` reads it and constructs the corresponding `SharedBus` via the new `select_bus` helper. Subscribers (`id`, `ego`, `superego_local`, `egress`, ui-emitter) are unchanged — they always operate against `SharedBus = Arc<dyn EventBus>`.

Any failure on the iggy path falls back to `InMemoryBus` with a diagnostic log. The entity stays alive even when the broker doesn't.

### Sidecar lifecycle

`orion::iggy_supervisor::IggySupervisor` owns the iggy-server child process. Responsibilities:

- Locate the binary in this order: `ORIONII_IGGY_SERVER` env var → `iggy-server*` next to the OrionII executable (Tauri externalBin layout) → `iggy-server` on `PATH`.
- Spawn with `IGGY_SYSTEM_PATH={config_dir}/OrionII/iggy/` and `IGGY_TCP_ADDRESS=127.0.0.1:{port}`.
- Wait for the TCP port to accept connections (`STARTUP_TIMEOUT = 15s`).
- Supervise: on unexpected child exit, restart up to 3 times in 60 seconds. After that, give up and publish a `Topic::GovernanceInbound` envelope with `kind: "broker-unstable"` so the UI can surface the failure mode.
- On `Drop` (e.g. `apply_bundle_config` hot-swap or app shutdown), `start_kill()` the child synchronously; `kill_on_drop(true)` ensures cleanup if the process panics before reaching `Drop`.

The data directory at `{config_dir}/OrionII/iggy/` survives restart, isolates per OS user, and is **not** touched by `apply_bundle_config` — that command only manages SAO-side config.

### Topic mapping

One stream per entity (`orionii.entity.{orion_id}`), one Iggy topic per `Topic` enum variant — 8 topics total. All created idempotently on `IggyBus::connect` if missing. Partition count: 1 per topic. Consumer group: `orionii.entity.{orion_id}.consumer`, auto-joined and auto-created. The polling task uses `AutoCommit::When(AutoCommitWhen::PollingMessages)` so offsets advance every poll round-trip.

### Polling vs in-process broadcast

Iggy's API is poll-based, but `EventBus::subscribe` returns `BusReceiver` which exposes async `recv()`. We bridge the two with one polling task per topic, started in `IggyBus::connect`. Each polling task pulls envelopes from iggy and re-broadcasts them through a per-topic `tokio::broadcast::Sender`. `subscribe(topic)` returns a fresh receiver of that local sender wrapped in `BusReceiver::Iggy`.

Subscriber call shape is identical to the in-memory path:

```rust
let mut rx = bus.subscribe(Topic::EgoAction);
loop {
    match rx.recv().await {
        Ok(env) => handle(env).await,
        Err(RecvError::Lagged(n)) => eprintln!("dropped {n}"),
        Err(RecvError::Closed) => break,
    }
}
```

### PAT auth — Phase 2b reality vs Phase 2.1 target

The plan called for Personal Access Token auth. **Phase 2b ships a stand-in**:

- The token store is `{config_dir}/OrionII/iggy_pat`, a JSON file with mode 600 on Unix and the per-user `%APPDATA%` ACL on Windows.
- On first run, `iggy_auth::provision_first_run` returns the iggy-server bootstrap admin pair (`iggy:iggy`) cast as a PAT-shaped string. This is dev-grade only.
- The proper PAT-mint flow (admin login → `client.create_personal_access_token(...)` → revoke admin password) is `TODO(phase-2b-pat-mint)` in `iggy_auth.rs`.

Phase 2.1 work:
- Replace the stand-in with the real PAT-mint flow.
- Swap the file backend for an OS keychain (Tauri stronghold or `keyring` crate). The `keyring 4` crate currently pulls in turso-core (a SQLite-derived store) which requires a C toolchain that's not on most dev Windows boxes — that's why Phase 2b deferred it.
- Wire `rotate_iggy_token` to actually mint a new server-side PAT and revoke the old one.

The `rotate_iggy_token` Tauri command is shipped now and exercises the file path; it will keep its same signature once the real mint lands.

### Sidecar binary distribution

The binaries themselves are **not vendored** in this repo. `src-tauri/binaries/README.md` documents the manual install procedure (`cargo install iggy-server` or download from GitHub releases). The supervisor's `locate_binary()` picks them up from PATH, env-var override, or the OrionII install directory.

Phase 2.1 work:
- Vendor `iggy-server-{target-triple}` binaries (Win/macOS-x64/macOS-arm64/Linux) in `src-tauri/binaries/`.
- Register them in `tauri.conf.json` `bundle.externalBin` so `tauri build` packages them.
- Add a `build.rs` that downloads + SHA-256-verifies them on first build, refusing to ship if checksums don't match.

## Consequences

**+** The entity-internal bus survives process death. Restarting OrionII picks up where it left off — pending egress, in-flight ego.action, etc. — provided the iggy data directory is intact. That is what makes "the entity persists" a real claim about the runtime.

**+** The bus log itself becomes auditable from outside OrionII via `iggy-cli`. `soul_ref` on every envelope, durable across restarts, makes version violence detectable from the streamed event log alone.

**+** The `EventBus` trait's swap is a back-end change. No subscriber, command, or UI code changed. `EventBus`, `BusReceiver`, `Envelope`, `RecvError` keep their Phase 1 shape.

**+** Failure modes are graceful: any iggy-path error falls back to the in-memory bus with a diagnostic log. The entity remains operable in degraded mode if the broker is broken.

**−** Two follow-up tickets (Phase 2.1):
1. PAT mint + OS keychain integration.
2. Sidecar binary vendoring with checksums.

**−** Native deps. The `iggy` client crate at `0.10` pulls in QUIC, rustls, tokio-tungstenite, and a few other heavy crates. We dropped `keyring 4` because of its turso-core SQLite-derived backend requiring a C toolchain — Phase 2.1 keychain work picks an alternative (likely Tauri stronghold or `keyring 3`).

**−** Polling adds latency between produce and consume — bounded by the `POLL_INTERVAL_MS = 100` configured in `bus/iggy.rs`. For chat that's fine; for sub-100ms control loops it would matter.

**−** Hot-swap (apply_bundle_config) currently tears down and rebuilds the entire OrionCore including the iggy-server child. That's correct but heavyweight — the supervisor sends SIGKILL to the child, the new core starts a new sidecar. Phase 2.1 may want to keep the sidecar across hot-swaps and only re-authenticate.

## Out of scope (deferred to later)

- Real PAT mint + OS keychain (Phase 2.1).
- Vendored sidecar binaries + `build.rs` checksum download (Phase 2.1).
- Per-task reconnect on consumer stream end (today the entire IggyBus rebuilds when the supervisor restarts the sidecar).
- Real NPPI sanitizer in `egress::sanitize` (still a key-name redaction stub from Phase 1).
- Real local Superego evaluation (still the accept-everything stub from Phase 1).
- Real `soul.md` hashing (still the `orion_id:vN` surrogate; SAO doesn't ship a signed `soul.md` blob yet).
- Stream replay UI (Iggy enables it; the audit-replay viewer is its own ticket).

## Verification

1. **Compile clean**: `cargo check` clean as of this commit.
2. **In-memory path still passes**: `cargo test --lib` — 24 tests pass, 0 ignored.
3. **Iggy compile path verified**: `bus/iggy.rs`, `iggy_auth.rs`, `iggy_supervisor.rs` all compile-clean with the iggy 0.10 client; the iggy crate's own dependencies (turso, openssl-src) are *not* pulled in by us — only the client surface.
4. **End-to-end smoke** (requires user-side iggy-server, deferred to user verification):
   - Install iggy-server (`cargo install iggy-server` or release binary).
   - Set `bus_transport: { kind: "bundled_iggy" }` in `config.json`.
   - `npm run tauri dev`. Type "hello": user message + orion reply, same UX as in-memory.
   - `iggy-cli message poll --stream orionii.entity.{orion_id} --topic ego.action` shows the published envelope.
   - **Restart** OrionII. Confirm previous envelopes are replayable on the iggy node.
   - Force-kill OrionII mid-chat. Restart. Confirm pending `EgressOutbound` envelopes are still on the bus and the egress subscriber resumes shipping them.
5. **Supervisor robustness** (user-side):
   - `kill -9` the iggy-server child while OrionII is running. Confirm restart within 2s.
   - Trigger 4 kills in 60s. Confirm `GovernanceInbound` envelope with `kind: "broker-unstable"` appears.
6. **Token rotation** (user-side, dev-grade until Phase 2.1):
   - Invoke `rotate_iggy_token`. Confirm `iggy_pat` is rewritten and the chat keeps working.
7. **Architecture invariants**:
   - `git grep -nE "SaoShipper" src-tauri/src` matches only `egress.rs`, `service.rs::ship_sao_egress`, `sao.rs` (the type definition + its tests). Rule #4 still holds.
   - `git grep -nE "use crate::orion::(id|ego|curator|superego_local|egress|iggy_supervisor)::" src-tauri/src` matches only inside `service.rs::build_async` (the spawn + supervisor-handle imports) and the curator↔id internal split. No new participant-to-participant logic imports.
