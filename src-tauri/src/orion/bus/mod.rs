//! SAO entity-internal event bus.
//!
//! All communication between the entity's internal participants — Mentor (user),
//! Id, Ego, and the local Superego stub — flows through this trait. Direct
//! function calls between participant modules are an architectural regression
//! and should be rejected in code review (see ADR-001 and CLAUDE.md).
//!
//! The trait exists so the underlying transport can be swapped from the
//! in-process default (`InMemoryBus`, tokio broadcast) to a durable local
//! broker (`NatsJetStreamBus`, or the experimental `IggyBus`) without
//! touching callers. New code MUST NOT depend on the concrete bus type —
//! depend on `SharedBus` and the `EventBus` trait instead.
//!
//! `publish` is async because durable broker backends publish over a local
//! network connection. `subscribe` stays sync but returns a `BusReceiver` wrapper whose
//! `recv` is async on every backend. Subscribers always look like:
//!
//! ```ignore
//! let mut rx = bus.subscribe(Topic::EgoAction);
//! loop {
//!     match rx.recv().await {
//!         Ok(env) => handle(env).await,
//!         Err(RecvError::Lagged(n)) => eprintln!("dropped {n}"),
//!         Err(RecvError::Closed) => break,
//!     }
//! }
//! ```
//!
//! New variants on `Topic` require an ADR. Resurrected interrupt/agent-task
//! topics: see future ADR-002+.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::orion::identity::IdentityState;

pub mod iggy;
pub mod inmem;
pub mod nats;

pub use iggy::IggyBus;
pub use inmem::InMemoryBus;
pub use nats::NatsJetStreamBus;

/// Canonical topic set for the entity-internal bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Topic {
    MentorInput,
    IdStimulus,
    IdReaction,
    EgoDeliberation,
    EgoAction,
    SuperegoLocalEvaluation,
    EgressOutbound,
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

/// Every event on the bus carries provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub topic: Topic,
    pub agent_id: String,
    pub occurred_at: DateTime<Utc>,
    pub soul_ref: String,
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
    #[error("topic channel closed")]
    Closed,
}

/// Receive-side errors. Mirrors the broadcast crate's shape so subscribers
/// can keep the same `match` arms across transports.
#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    /// The subscriber fell behind the channel buffer and `n` envelopes were
    /// dropped. The receiver is still usable; subsequent `recv()` calls
    /// return the most-recent values still in the buffer.
    #[error("subscriber lagged and dropped {0} envelopes")]
    Lagged(u64),
    /// The publisher half of the channel has been dropped; no more events
    /// will arrive.
    #[error("topic channel closed")]
    Closed,
}

impl From<broadcast::error::RecvError> for RecvError {
    fn from(value: broadcast::error::RecvError) -> Self {
        match value {
            broadcast::error::RecvError::Lagged(n) => RecvError::Lagged(n),
            broadcast::error::RecvError::Closed => RecvError::Closed,
        }
    }
}

/// Subscriber-side handle. Wraps either a tokio broadcast receiver from
/// `InMemoryBus`, or a per-topic broadcast receiver fed by the durable
/// broker polling task. Subscribers don't need to know which.
///
/// Both variants are tokio broadcast receivers — what differs is who feeds
/// them. The `InMemory` variant is fed by the publisher directly; the
/// broker variants are fed by per-topic polling tasks pulling from the
/// local sidecar.
pub struct BusReceiver {
    inner: BusReceiverInner,
}

enum BusReceiverInner {
    InMemory(broadcast::Receiver<Envelope>),
    Iggy(broadcast::Receiver<Envelope>),
    Nats(broadcast::Receiver<Envelope>),
}

impl BusReceiver {
    pub fn from_broadcast(rx: broadcast::Receiver<Envelope>) -> Self {
        Self {
            inner: BusReceiverInner::InMemory(rx),
        }
    }

    pub fn from_iggy(rx: broadcast::Receiver<Envelope>) -> Self {
        Self {
            inner: BusReceiverInner::Iggy(rx),
        }
    }

    pub fn from_nats(rx: broadcast::Receiver<Envelope>) -> Self {
        Self {
            inner: BusReceiverInner::Nats(rx),
        }
    }

    pub async fn recv(&mut self) -> Result<Envelope, RecvError> {
        match &mut self.inner {
            BusReceiverInner::InMemory(rx) => rx.recv().await.map_err(RecvError::from),
            BusReceiverInner::Iggy(rx) => rx.recv().await.map_err(RecvError::from),
            BusReceiverInner::Nats(rx) => rx.recv().await.map_err(RecvError::from),
        }
    }
}

/// The entity's event bus.
#[async_trait]
pub trait EventBus: Send + Sync {
    async fn publish(&self, env: Envelope) -> Result<(), BusError>;
    fn subscribe(&self, topic: Topic) -> BusReceiver;
}

/// Shared, owning handle to the bus.
pub type SharedBus = Arc<dyn EventBus>;

/// Phase 1 surrogate for `soul_ref`. SAO does not yet ship a signed
/// `soul.md` blob at birth — when it does, replace this with
/// `hex(blake3(soul_md_bytes))` in this single helper.
pub fn current_soul_ref(identity: &IdentityState) -> String {
    // TODO(soul-md-hash): replace with hex(blake3(soul_md_bytes)) once SAO
    // ships a signed soul.md blob at birth and the entity caches it.
    format!("{}:v{}", identity.identity.orion_id, identity.version)
}
