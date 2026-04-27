//! In-process bus over `tokio::sync::broadcast`. Phase 1/2a default.
//!
//! Replaced by durable broker implementations in packaged builds (see ADR-003).
//! Keep this file small. Anything that grows here probably belongs
//! upstream of the bus, not inside it.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::broadcast;

use super::{BusError, BusReceiver, Envelope, EventBus, Topic};

/// Channel buffer size per topic. broadcast is bounded; lagging subscribers
/// receive `RecvError::Lagged(n)` rather than blocking the publisher.
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

#[async_trait]
impl EventBus for InMemoryBus {
    async fn publish(&self, env: Envelope) -> Result<(), BusError> {
        let topic = env.topic;
        let tx = self.senders.get(&topic).ok_or(BusError::Closed)?;
        // broadcast::send returns Err only when there are zero receivers.
        // That's a "published into the void" case which we treat as
        // success — the act of publishing is the contract, not delivery.
        let _ = tx.send(env);
        Ok(())
    }

    fn subscribe(&self, topic: Topic) -> BusReceiver {
        let rx = self
            .senders
            .get(&topic)
            .expect("topic pre-registered in InMemoryBus::new")
            .subscribe();
        BusReceiver::from_broadcast(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::bus::RecvError;
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
        bus.publish(env(Topic::MentorInput, "hello")).await.unwrap();

        let mut rx = bus.subscribe(Topic::MentorInput);
        bus.publish(env(Topic::MentorInput, "second"))
            .await
            .unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.topic, Topic::MentorInput);
        assert_eq!(received.payload["body"], "second");
    }

    #[tokio::test]
    async fn two_subscribers_each_receive_a_published_envelope() {
        let bus = InMemoryBus::new();
        let mut rx1 = bus.subscribe(Topic::EgoAction);
        let mut rx2 = bus.subscribe(Topic::EgoAction);

        bus.publish(env(Topic::EgoAction, "broadcast"))
            .await
            .unwrap();

        let a = rx1.recv().await.unwrap();
        let b = rx2.recv().await.unwrap();
        assert_eq!(a.payload["body"], "broadcast");
        assert_eq!(b.payload["body"], "broadcast");
    }

    #[tokio::test]
    async fn lagged_subscriber_returns_lagged_error_then_resumes() {
        let (tx, _) = broadcast::channel::<Envelope>(2);
        let mut rx = BusReceiver::from_broadcast(tx.subscribe());

        for i in 0..5 {
            let _ = tx.send(env(Topic::IdReaction, &format!("{i}")));
        }

        match rx.recv().await {
            Err(RecvError::Lagged(skipped)) => {
                assert!(skipped > 0, "expected dropped count > 0");
            }
            other => panic!("expected Lagged, got {:?}", other),
        }

        let next = rx.recv().await.unwrap();
        assert_eq!(next.topic, Topic::IdReaction);
    }
}
