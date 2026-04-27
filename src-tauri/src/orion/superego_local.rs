//! Local Superego stub.
//!
//! Subscribes to `Topic::EgoAction` and publishes
//! `Topic::SuperegoLocalEvaluation`. Phase 1 is logging-only — every
//! evaluation just records the `soul_ref` the entity was operating under.
//! That alone is the load-bearing property: it proves a Superego is on the
//! bus, watching, and that future ethical evaluation logic can plug in
//! without changing the architecture.
//!
//! Real constitutional checks (against soul.md, ethics.md, personality.md)
//! land in a later ticket. Until then, the variant carries `accepted: true`
//! to keep the seam simple.
//!
//! The *external* Superego — the one that publishes governance from outside
//! the entity — lives in SAO and arrives via `Topic::GovernanceInbound`.
//! This module is intentionally only the local stub.

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperegoLocalEvaluation {
    pub accepted: bool,
    pub soul_ref: String,
    pub note: String,
}

pub fn spawn(bus: SharedBus) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::EgoAction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => publish_evaluation(&bus, env).await,
                Err(RecvError::Lagged(skipped)) => {
                    eprintln!("[superego-local] lagged on EgoAction, skipped {skipped} envelopes");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

async fn publish_evaluation(bus: &SharedBus, env: Envelope) {
    let evaluation = SuperegoLocalEvaluation {
        accepted: true,
        soul_ref: env.soul_ref.clone(),
        note: format!(
            "phase-1 stub: accepted ego.action under soul_ref={}",
            env.soul_ref
        ),
    };
    let value = serde_json::to_value(&evaluation).unwrap_or(serde_json::Value::Null);
    let envelope = Envelope::new(
        Topic::SuperegoLocalEvaluation,
        env.agent_id,
        env.soul_ref,
        env.correlation_id,
        value,
    );
    if let Err(error) = bus.publish(envelope).await {
        eprintln!("[superego-local] failed to publish evaluation: {error}");
    }
}
