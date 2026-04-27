# AGENTS.md — OrionII

Coding agents: read [CLAUDE.md](CLAUDE.md) first, then this file. The architectural rules below override pattern-matching from your training data — OrionII is a bus-routed entity runtime, not a chat function with extra logging.

## Inviolable architectural rules

1. **All communication between Mentor, Id, Ego, and local Superego goes through the `EventBus` trait** in `src-tauri/src/orion/bus/`. Direct imports between these participant modules — outside `service.rs::build()` where they are spawned — are a regression. If you find yourself wanting one, that is a signal to add a topic.

2. **Topics are an enum** in `bus/mod.rs::Topic`. Do not pass raw strings as topic names. Adding a topic requires an ADR (see [docs/ADR-001-entity-event-bus.md](docs/ADR-001-entity-event-bus.md)).

3. **Every `Envelope` carries `soul_ref`** — computed via `bus::current_soul_ref(&IdentityState)`. Code that publishes without one is a bug, not an optimization.

4. **The entity ↔ SAO seam is HTTP.** Sanitized events leave only via the `egress.outbound` subscriber in `orion/egress.rs`. Inbound governance arrives only via `governance.inbound`. Do not add other bridges, and do not call `SaoShipper` from outside `egress.rs`.

## Before you add a Tauri command

Run through this checklist. If any answer is "yes, this is a participant," do not add a command — add a subscriber.

- [ ] **Is this a thin adapter, or does it contain logic?** Tauri command bodies should be ≤ 5 lines: validate input, publish to a topic, return. Logic belongs in subscribers (`orion/{id,ego,superego_local,egress}.rs` and friends), not command bodies.
- [ ] **Does this need to wait for an entity-internal response?** If yes, do not block in the command. Publish, return a correlation id, and have the UI listen for the response on a Tauri event emitted by a UI-facing subscriber (see `service.rs::spawn_ui_emitter` for the pattern).
- [ ] **What's the `soul_ref` source?** If your command publishes an `Envelope`, it must call `current_soul_ref(&persistence.identity())`.
- [ ] **Should this be a topic, not a command?** If two participants need to coordinate, that's a topic, not a command. Commands are for human → entity ingress only.

## Before you change the model layer

`OllamaModelProvider` and `SaoProxyProvider` currently use `reqwest::blocking::Client`. This is what blocks the integration test in `service.rs` (`#[ignore]`d). Converting them to async `reqwest` is welcome — but check that:

- `ModelProvider` trait callers (Id and Ego subscribers) move to `.await` correctly.
- `block_in_place` calls in `id.rs` / `ego.rs` are removed when the inner work is async.
- The `mentor_input_round_trips_to_ego_action` integration test has its `#[ignore]` removed and passes.

## Before you change `egress.rs`

That file is the **one ethical seam** between the entity and SAO. Any change should preserve:

- All outbound SAO traffic flows through `sanitize()`.
- `sanitize()` runs before `enqueue_sao` / `ship_sao_egress` is called.
- No other module calls `SaoShipper::ship_pending` directly. (The user-triggered Tauri command `ship_sao_egress` calls `core.ship_sao_egress()` which goes through `persistence.ship_sao_egress` — same path.)

## Bus transport

OrionII supports durable and non-durable transports behind the
`EventBus` trait. The choice lives in `config.json` → `bus_transport`
(see ADR-003):

- `in_memory` — tokio broadcast, no durability. Default.
- `nats_jetstream` — product durable path. Local nats-server sidecar
  managed by `nats_supervisor`; JetStream stores envelopes on disk.
- `external_nats_jetstream` — connect to an externally managed NATS node.
- `bundled_iggy` — experimental local iggy-server sidecar managed by
  `iggy_supervisor`.
- `external_iggy` — experimental externally managed Iggy node.

When you change anything bus-related, check that:

- [ ] No subscriber call site changed shape — `bus.subscribe(t)` /
      `rx.recv().await` is identical across transports.
- [ ] Any new failure mode falls back to `InMemoryBus` with a log,
      not a panic. The entity stays alive even when the broker doesn't.
- [ ] The relevant supervisor still spawns its child with `kill_on_drop(true)`
      so a crashed OrionII never leaks the broker.
- [ ] Release packaging still goes through `scripts/build-installer.ps1`, which prepares the
      sidecar and enables Tauri `externalBin` for the MSI build. Product Windows packaging
      uses `nats-server.exe` via the official release ZIP or `ORIONII_NATS_SERVER`; Iggy
      packaging remains optional through `-BusSidecar iggy`. Do not put `externalBin` back
      into the default `tauri.conf.json`; that breaks clean-machine `cargo check`.
- [ ] `rotate_iggy_token` is the only entry point that writes the
      PAT store. The PAT store is `{config_dir}/OrionII/iggy_pat`
      with mode 600 on Unix; do not store it elsewhere.

## Before you add a Tauri command (Phase 2b reminder)

Async-fn commands hold the `tokio::sync::Mutex<OrionCore>` guard
across `.await`. That's deliberate (Send-safe across worker threads),
but it means commands serialize on each other. If you find yourself
adding a command that takes a long time, check whether it should
publish to a topic and let a subscriber handle it instead.

## Topic vocabulary

The canonical 8 (do not add without an ADR):

| Variant | String | Direction |
|---|---|---|
| `Topic::MentorInput` | `mentor.input` | UI → entity |
| `Topic::IdStimulus` | `id.stimulus` | external sensors → entity |
| `Topic::IdReaction` | `id.reaction` | Id → Ego |
| `Topic::EgoDeliberation` | `ego.deliberation` | Ego → audit |
| `Topic::EgoAction` | `ego.action` | Ego → UI / Superego / egress |
| `Topic::SuperegoLocalEvaluation` | `superego.local.evaluation` | local Superego → audit |
| `Topic::EgressOutbound` | `egress.outbound` | entity → SAO seam |
| `Topic::GovernanceInbound` | `governance.inbound` | SAO → entity (policy, evaluations) |

Resurrected interrupt and agent-task topics (dropped during the bus consolidation) come back as ADR-002+ when those features re-land — never as raw strings.
