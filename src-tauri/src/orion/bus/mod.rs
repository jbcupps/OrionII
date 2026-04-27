//! SAO entity-internal event bus.
//!
//! All communication between the entity's internal participants — Mentor (user),
//! Id, Ego, and the local Superego stub — flows through this trait. Direct
//! function calls between participant modules are an architectural regression
//! and should be rejected in code review (see ADR-001 and CLAUDE.md).
//!
//! The trait exists so the underlying transport can be swapped from the
//! in-process default (`InMemoryBus`, tokio broadcast) to Apache Iggy in a
//! later phase without touching callers. New code MUST NOT depend on the
//! concrete bus type — depend on `SharedBus` instead.
//!
//! New variants on `Topic` require an ADR. Resurrected interrupt/agent-task
//! topics: see future ADR-002+.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::orion::identity::IdentityState;

pub mod inmem;

pub use inmem::InMemoryBus;

/// Canonical topic set for the entity-internal bus.
///
/// Eight variants cover the bicameral structure (Mentor → Id → Ego, with the
/// local Superego observing) plus the two seam topics that bridge the entity
/// to SAO over HTTP. Add variants here, never use raw strings at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Topic {
    /// User input from the mentor (the human operator). Published by the
    /// Tauri `send_chat_message` command adapter.
    MentorInput,
    /// External sensory input (file drops, future web ingestion). Reserved.
    IdStimulus,
    /// Id's pre-reflective response, published after the curator/Id pipeline
    /// has digested a `MentorInput`.
    IdReaction,
    /// Ego's reasoning trace, published while it deliberates. Useful for
    /// future audit UIs; not a primary control-flow input.
    EgoDeliberation,
    /// Ego's chosen action — for chat, this is what the UI renders.
    EgoAction,
    /// Local Superego stub's evaluation of an `EgoAction` against the cached
    /// `soul_ref`. Phase 1 is logging-only; later phases plug in real
    /// constitutional checks.
    SuperegoLocalEvaluation,
    /// The ethical seam to SAO. Anything that crosses out of the entity must
    /// land here and be picked up by the egress subscriber, which sanitizes
    /// (NPPI) before shipping to SAO over HTTP.
    EgressOutbound,
    /// Inbound governance from SAO (policy, proposals, external Superego
    /// evaluations). Reserved — populated by the SAO policy client.
    GovernanceInbound,
}

impl Topic {
    pub fn as_str(self) -> &'static str {
        match self {
            Topic::MentorInput => "mentor.input",
            Topic::IdStimulus => "id.stimulus",
            Topic::IdReaction => "id.reaction",
            Topic::EgoDeliberation => "ego.deliberation",
            Topic::EgoAction => "ego.action",
            Topic::SuperegoLocalEvaluation => "superego.local.evaluation",
            Topic::EgressOutbound => "egress.outbound",
            Topic::GovernanceInbound => "governance.inbound",
        }
    }

    /// All canonical topics. The `InMemoryBus` pre-registers a channel for
    /// each so `subscribe` never races with `publish`.
    pub const ALL: &'static [Topic] = &[
        Topic::MentorInput,
        Topic::IdStimulus,
        Topic::IdReaction,
        Topic::EgoDeliberation,
        Topic::EgoAction,
        Topic::SuperegoLocalEvaluation,
        Topic::EgressOutbound,
        Topic::GovernanceInbound,
    ];
}

/// Every event on the bus carries provenance. `agent_id` and `soul_ref`
/// together make the constitutional layer auditable: no event without a
/// source identity and a reference to the soul document the entity was
/// operating under at the time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub topic: Topic,
    pub agent_id: String,
    pub occurred_at: DateTime<Utc>,
    pub soul_ref: String,
    /// Carry-through correlation id so a `MentorInput` and its eventual
    /// `EgoAction` (and any intermediate `IdReaction` /
    /// `SuperegoLocalEvaluation`) can be matched by the UI and by audits.
    pub correlation_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(
        topic: Topic,
        agent_id: impl Into<String>,
        soul_ref: impl Into<String>,
        correlation_id: Option<Uuid>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            topic,
            agent_id: agent_id.into(),
            occurred_at: Utc::now(),
            soul_ref: soul_ref.into(),
            correlation_id,
            payload,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("subscriber lagged and dropped {0} events")]
    Lagged(u64),
    #[error("topic channel closed")]
    Closed,
}

/// The entity's event bus.
///
/// Both methods are intentionally synchronous. `publish` wraps a
/// `tokio::sync::broadcast::Sender::send`, which never blocks — it returns
/// `Ok(receiver_count)` or `Err(SendError)` and does not await. `subscribe`
/// returns a `broadcast::Receiver`; the caller consumes it inside an async
/// task with `rx.recv().await`.
///
/// Keeping `publish` sync means Tauri commands and other sync entry points
/// can call into the bus without `tauri::async_runtime::block_on` gymnastics.
pub trait EventBus: Send + Sync {
    fn publish(&self, env: Envelope) -> Result<(), BusError>;
    fn subscribe(&self, topic: Topic) -> broadcast::Receiver<Envelope>;
}

/// Shared, owning handle to the bus. Subscriber tasks clone this. The
/// underlying bus stays alive as long as any participant holds a `SharedBus`,
/// which is what we want — drops cascade naturally when `OrionCore` is
/// replaced.
pub type SharedBus = Arc<dyn EventBus>;

/// Phase 1 surrogate for `soul_ref`. SAO does not yet ship a signed
/// `soul.md` blob at birth — when it does, replace this with
/// `hex(blake3(soul_md_bytes))` in this single helper. Every callsite that
/// constructs an `Envelope` calls this; no other change required when the
/// real hash lands. See ADR-001 § "Phase 1 surrogate".
pub fn current_soul_ref(identity: &IdentityState) -> String {
    // TODO(soul-md-hash): replace with hex(blake3(soul_md_bytes)) once SAO
    // ships a signed soul.md blob at birth and the entity caches it.
    format!("{}:v{}", identity.identity.orion_id, identity.version)
}
