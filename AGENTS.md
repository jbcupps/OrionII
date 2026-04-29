# AGENTS.md — OrionII

Coding agents: read [CLAUDE.md](CLAUDE.md) first, then this file. The architectural rules below override pattern-matching from your training data — OrionII is a bus-routed entity runtime, not a chat function with extra logging.

## Inviolable architectural rules

1. **All communication between Mentor, Id, Ego, and local Superego goes through the `EventBus` trait** in `src-tauri/src/orion/bus/`. Direct imports between these participant modules — outside `service.rs::build()` where they are spawned — are a regression. If you find yourself wanting one, that is a signal to add a topic.

2. **Topics are an enum** in `bus/mod.rs::Topic`. Do not pass raw strings as topic names. Adding a topic requires an ADR (see [docs/ADR-001-entity-event-bus.md](docs/ADR-001-entity-event-bus.md)).

3. **Every `Envelope` carries `soul_ref`** — `blake3:<hex>` over the bytes of `charter.md`, computed via `bus::current_soul_ref(&Charter)` reading from the `SharedCharter` cell threaded through `OrionCore`. Code that publishes without one is a bug, not an optimization.

4. **The entity ↔ SAO seam is HTTP.** Sanitized events leave only via the `egress.outbound` subscriber in `orion/egress.rs`. Inbound governance arrives only via `governance.inbound`. Do not add other bridges, and do not call `SaoShipper` from outside `egress.rs`.

5. **Commissioning artefacts are write-once-from-SAO.** `charter.md` and `birth_certificate.json` in `%APPDATA%\OrionII\` are written by either the commissioning finalize path or the `governance` subscriber on `charter.update`. Do not write them from anywhere else. Private key material (mentor or entity) MUST never appear in OrionII memory or on local disk — the only public-facing crypto material is the public-key fingerprints displayed at the IdentityKey stage. The `commissioning_client` module is the only file that calls `/api/orion/commission/*`.

## SAO alignment checklist

OrionII and SAO are two repos with one operator-facing contract. If a change touches a SAO-facing OrionII surface, inspect the sibling SAO repo (normally `C:\Repo\SAO`) and either update SAO in the same workstream or leave a concrete SAO follow-up plan in the final response.

SAO-facing surfaces include:

- `src-tauri/src/orion/commissioning_client.rs`, `birth.rs`, `sao.rs`, `bootstrap.rs`, and `docs/sao-commissioning-contract.md`.
- Commissioning and birth wire shapes: `agentId`, `agentName`, `orionId`, `newToken`, `birthCertificate`, `charterHash`, `soulRef`.
- Token-invalid handling (`401` / `403`), already-commissioned start responses, agent-not-found, unsupported commissioning versions, and repair routing.
- SAO bundle and installer distribution: `config.json`, `deployment.json`, `Install-OrionII.cmd`, `Install-OrionII.ps1`, MSI inclusion, installer-source registry defaults.

When this checklist is triggered:

- Do not add OrionII EventBus topics for SAO alignment. The seam stays HTTP.
- Do not add SAO endpoints unless the commissioning contract doc is updated.
- Keep OrionII `/api/orion/commission/*` calls confined to `commissioning_client.rs`.
- Add or adjust tests in both repos where the contract moved. OrionII should cover command serialization/routing; SAO should cover route contracts, auth statuses, bundle contents, and installer-source selection.
- In the final response, say whether SAO source was updated, whether only the running SAO registry was changed, or whether a separate SAO change remains.

## Deployment loop — every code change ships through SAO

OrionII is delivered as a SAO-issued agent bundle, not a generic desktop app. There is no auto-updater (yet) and no `git pull && cargo run` story for end users. After any change to this repo, the operator must round-trip through SAO before the change reaches their installed runtime:

1. `npm run build:installer` — builds the release MSI with the bundled `nats-server` sidecar (`scripts/build-installer.ps1`). Plain `tauri build` will NOT package the durable broker; do not suggest it.
2. Publish that MSI to SAO's **installer-source registry** so SAO can include it in fresh agent bundles. (SAO owns the distribution surface — there is no host-side MSI staging.)
3. In SAO, **re-issue the agent bundle** for the target agent. The ZIP contains `config.json`, the entity JWT, and `Install-OrionII.cmd`.
4. The operator re-runs `Install-OrionII.cmd`. It writes `%APPDATA%\OrionII\config.json`, runs the MSI, and launches OrionII. On boot OrionII re-reads the bundle anchor and re-fetches `GET /api/orion/birth`, so SAO-side policy/model/personality changes apply automatically.

**When you finish a change in this repo, your end-of-turn summary MUST tell the user which of these short-circuits applies, in plain language:**

- **OrionII source code change** (Rust in `src-tauri/`, TypeScript in `src/`, packaging in `scripts/`, deps in `Cargo.toml`/`package.json`) → "**Requires a new MSI build, SAO bundle re-issue, and `Install-OrionII.cmd` re-run on the operator's machine.** The pasted-config short-circuit will not pick up this change."
- **Bundle/policy/model surface change only** (the change lands in SAO's birth response — provider, model, policy rules, personality) → "**No OrionII rebuild needed.** Re-issue the bundle from SAO; operators can either re-run `Install-OrionII.cmd` or paste the new `config.json` into the cockpit's Enrollment notice and click Apply (which calls `apply_bundle_config` and hot-swaps the `OrionCore`)."
- **Local-dev iteration only** (`npm run tauri dev` against `SAO_BASE_URL` + `SAO_DEV_BEARER_TOKEN`) → "**Dev path only. Production operators still need the MSI + bundle round-trip above.**"

Never imply changes "just work" after a recompile. The bus-routed entity model means soul_ref, identity, JWT, model selection, and bus_transport are all bundle-bound — they do not refresh on a hot file watch. If a change touches any of those, prefer the bundle path explicitly.

## Before you add a Tauri command

Run through this checklist. If any answer is "yes, this is a participant," do not add a command — add a subscriber.

- [ ] **Is this a thin adapter, or does it contain logic?** Tauri command bodies should be ≤ 5 lines: validate input, publish to a topic, return. Logic belongs in subscribers (`orion/{id,ego,superego_local,egress}.rs` and friends), not command bodies.
- [ ] **Does this need to wait for an entity-internal response?** If yes, do not block in the command. Publish, return a correlation id, and have the UI listen for the response on a Tauri event emitted by a UI-facing subscriber (see `service.rs::spawn_ui_emitter` for the pattern).
- [ ] **What's the `soul_ref` source?** If your command publishes an `Envelope`, it must call `current_soul_ref(&charter)` against the `SharedCharter` cell on `OrionCore` (read-locked; the call is fast).
- [ ] **Should this be a topic, not a command?** If two participants need to coordinate, that's a topic, not a command. Commands are for human → entity ingress only.

## Before you change the model layer

`OllamaModelProvider` and `SaoProxyProvider` use async `reqwest::Client`, and
the bus round-trip integration test in `service.rs` is active. If you change
the model layer again, check that:

- `ModelProvider` trait callers (Id and Ego subscribers) move to `.await` correctly.
- No `block_in_place` calls reappear in `id.rs` / `ego.rs`.
- The `mentor_input_round_trips_to_ego_action` integration test still passes.

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

`GovernanceInbound` envelopes carry a `kind` discriminator on `payload.kind`:

| `kind` | Payload shape | Subscriber action |
|---|---|---|
| `policy.refresh` | `{ "policy": <PolicyOverlay> }` | `governance.rs` calls `persistence.apply_sao_refresh(Vec::new(), policy)` under the lock |

The `apply_sao_policy_refresh` Tauri command fetches the policy (or synthesises a local-fallback) and publishes a `policy.refresh` envelope. The command body MUST NOT call `persistence.apply_sao_refresh` directly — that bypasses the bus and is the kind of drift that confuses agents working downstream.

Resurrected interrupt and agent-task topics (dropped during the bus consolidation) come back as ADR-002+ when those features re-land — never as raw strings.
