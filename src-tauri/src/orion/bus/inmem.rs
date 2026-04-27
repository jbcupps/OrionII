//! In-process bus over `tokio::sync::broadcast`. Phase 1 default.
//!
//! Replaced by an Iggy-backed implementation in a later phase. Keep this
//! file small. Anything that grows here probably belongs upstream of the
//! bus, not inside it.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;

use super::{BusError, Envelope, EventBus, Topic};

/// Channel buffer size per topic. broadcast is bounded; lagging subscribers
/// receive `RecvError::Lagged(n)` rather than blocking the publisher. 1024
/// is comfortable for chat-volume traffic and small enough that runaway
/// publishes are visible quickly.
const CHANNEL_CAPACITY: usize = 1024;

pub struct InMemoryBus {
    senders: DashMap<Topic, broadcast::Sender<Envelope>>,
}

impl InMemoryBus {
    pub fn new() -> Arc<Self> {
        let bus = Self {
            senders: DashMap::new(),
        };
        for topic in Topic::ALL {
            let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
            bus.senders.insert(*topic, tx);
        }
        Arc::new(bus)
    }
}

impl EventBus for InMemoryBus {
    fn publish(&self, env: Envelope) -> Result<(), BusError> {
        let topic = env.topic;
        let tx = self
            .senders
            .get(&topic)
            .ok_or(BusError::Closed)?;
        // broadcast::send returns Err only when there are zero receivers.
        // That's a "published into the void" case which we treat as
        // success — the act of publishing is the contract, not delivery.
        let _ = tx.send(env);
        Ok(())
    }

    fn subscribe(&self, topic: Topic) -> broadcast::Receiver<Envelope> {
        self.senders
            .get(&topic)
            .expect("topic pre-registered in InMemoryBus::new")
            .subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(topic: Topic, body: &str) -> Envelope {
        Envelope::new(
            topic,
            "test-agent",
            "test-soul:v1",
            None,
            json!({ "body": body }),
        )
    }

    #[tokio::test]
    async fn publish_before_subscribe_does_not_panic() {
        let bus = InMemoryBus::new();
        // No subscribers — should be Ok.
        bus.publish(env(Topic::MentorInput, "hello")).unwrap();

        // Now subscribe and confirm a fresh publish reaches the new subscriber.
        let mut rx = bus.subscribe(Topic::MentorInput);
        bus.publish(env(Topic::MentorInput, "second")).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.topic, Topic::MentorInput);
        assert_eq!(received.payload["body"], "second");
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_a_published_envelope() {
        let bus = InMemoryBus::new();
        let mut rx1 = bus.subscribe(Topic::EgoAction);
        let mut rx2 = bus.subscribe(Topic::EgoAction);

        bus.publish(env(Topic::EgoAction, "broadcast")).unwrap();

        let a = rx1.recv().await.unwrap();
        let b = rx2.recv().await.unwrap();
        assert_eq!(a.payload["body"], "broadcast");
        assert_eq!(b.payload["body"], "broadcast");
    }

    #[tokio::test]
    async fn lagged_subscriber_returns_lagged_error_then_resumes() {
        let bus = InMemoryBus::new();
        // Build a tiny bus to make lag easy to exercise.
        let (tx, mut rx) = broadcast::channel::<Envelope>(2);

        // Fill past capacity; receiver hasn't drained yet.
        for i in 0..5 {
            let _ = tx.send(env(Topic::IdReaction, &format!("{i}")));
        }

        // First recv() will surface a Lagged error documenting how many
        // events were dropped. The receiver remains usable for subsequent
        // recvs.
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                assert!(skipped > 0, "expected dropped count > 0");
            }
            other => panic!("expected Lagged, got {:?}", other),
        }

        // Subsequent recv should yield a real envelope (the most-recent
        // values still in the buffer).
        let next = rx.recv().await.unwrap();
        assert_eq!(next.topic, Topic::IdReaction);
    }
}
