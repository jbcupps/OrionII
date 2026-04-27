# ADR-003: NATS JetStream as the packaged durable EventBus transport

## Status

Accepted. 2026-04-27.

## Context

ADR-001 made OrionII's entity-internal bus the cognitive boundary. ADR-002 implemented an Apache Iggy adapter, but Windows product packaging is blocked until a vetted `iggy-server.exe` distribution exists. The message bus is still load-bearing, so the product needs a durable broker that can ship inside the MSI today.

NATS provides official Windows release archives, and JetStream is the file-backed persistence layer built into `nats-server`. This gives OrionII a local durable bus without changing the entity architecture: SAO remains external over HTTP, and Mentor/Id/Ego/local Superego continue to use `EventBus`.

## Decision

Add `NatsJetStreamBus` behind `src-tauri/src/orion/bus/EventBus`.

The product bundle default is:

```json
"bus_transport": { "kind": "nats_jetstream", "port": 4222 }
```

`nats_supervisor` starts `nats-server` with JetStream enabled, loopback-only binding, and a store directory at `{config_dir}/OrionII/nats`. Release builds run `scripts/build-installer.ps1`, which prepares `nats-server.exe` and enables Tauri `externalBin` only for the installer build. The default `tauri.conf.json` stays sidecar-free so `cargo check` works on clean developer machines.

Subjects map one-to-one to the canonical topic enum:

```text
orionii.{agent_id}.mentor.input
orionii.{agent_id}.id.stimulus
orionii.{agent_id}.id.reaction
orionii.{agent_id}.ego.deliberation
orionii.{agent_id}.ego.action
orionii.{agent_id}.superego.local.evaluation
orionii.{agent_id}.egress.outbound
orionii.{agent_id}.governance.inbound
```

The stream name is `ORIONII_{agent_id}` using the UUID simple form. `publish` writes through JetStream and waits for the server publish acknowledgement. `subscribe(topic)` returns the same `BusReceiver` shape as every other transport; a lazy durable pull consumer fans messages into a local tokio broadcast channel.

Failures on the NATS path fall back to `InMemoryBus` with a diagnostic log. The entity must stay alive even when the broker does not.

## Consequences

**+** OrionII has a packaged durable message bus on Windows now.

**+** The SAO bundle can request a durable transport without user JSON edits or broker setup.

**+** Subscriber call shape stays invariant: `bus.subscribe(topic)` and `rx.recv().await`.

**+** Iggy remains available as an adapter target, but no longer blocks the product milestone.

**-** NATS introduces another dependency and sidecar binary to package and verify.

**-** The first NATS implementation acknowledges after fanout into local subscribers, not after each participant finishes semantic handling. That matches the existing Iggy adapter's practical behavior, but richer per-participant replay can be added later with separate durable consumers.

## Verification

Required milestone gates:

1. `cargo check`, `cargo test --lib`, `cargo clippy --all-targets -- -D warnings`, and `npm run build` pass.
2. `npm run build:installer` produces an MSI that contains `nats-server`.
3. SAO bundle `config.json` and `deployment.json` contain `bus_transport.kind = "nats_jetstream"`.
4. Launch from a SAO-downloaded bundle; OrionII births through SAO and chat still completes through `orion://ego/action`.
5. Confirm `%APPDATA%/OrionII/nats` contains JetStream data after chat.
6. Restart OrionII and verify the NATS stream remains present.
7. Kill the `nats-server` child once and verify supervisor restart; kill repeatedly and verify a `GovernanceInbound` envelope with `kind = "broker-unstable"`.
