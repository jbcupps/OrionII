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
