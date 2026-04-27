//! NATS JetStream-backed implementation of `EventBus`.
//!
//! This is the product durable-bus path. It runs against a local
//! `nats-server` sidecar with JetStream enabled by `nats_supervisor`.
//! OrionII still exposes the same `EventBus` surface to Mentor, Id, Ego,
//! local Superego, UI emission, and egress subscribers.
//!
//! Mapping:
//! - one JetStream stream per entity: `ORIONII_{orion_id_simple}`;
//! - one NATS subject per canonical topic:
//!   `orionii.{orion_id_simple}.{topic.as_str()}`;
//! - one durable pull consumer per topic, lazily started on first
//!   `subscribe(topic)`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_nats::jetstream::{self, consumer::pull, stream};
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::StreamExt;
use tauri::async_runtime::JoinHandle;
use thiserror::Error;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{BusError, BusReceiver, Envelope, EventBus, Topic};

const CHANNEL_CAPACITY: usize = 1024;
const MAX_STREAM_MESSAGES: i64 = 1_000_000;

#[derive(Debug, Error)]
pub enum NatsBusError {
    #[error("nats connection error: {0}")]
    Connect(String),
    #[error("jetstream error: {0}")]
    JetStream(String),
}

pub struct NatsJetStreamBus {
    jetstream: jetstream::Context,
    stream_name: String,
    subject_prefix: String,
    fanout: DashMap<Topic, broadcast::Sender<Envelope>>,
    active_pollers: Mutex<HashSet<Topic>>,
    /// Held so pollers abort when the bus is dropped during hot-swap.
    pollers: Mutex<Vec<JoinHandle<()>>>,
}

impl NatsJetStreamBus {
    pub async fn connect(endpoint: &str, orion_id: Uuid) -> Result<Arc<Self>, NatsBusError> {
        let client = async_nats::connect(endpoint)
            .await
            .map_err(|e| NatsBusError::Connect(e.to_string()))?;
        let jetstream = jetstream::new(client);

        let entity_key = orion_id.simple().to_string();
        let stream_name = format!("ORIONII_{entity_key}");
        let subject_prefix = format!("orionii.{entity_key}");
        let subjects = vec![format!("{subject_prefix}.>")];

        jetstream
            .get_or_create_stream(stream::Config {
                name: stream_name.clone(),
                subjects,
                storage: stream::StorageType::File,
                max_messages: MAX_STREAM_MESSAGES,
                description: Some(format!("OrionII entity bus for {orion_id}")),
                ..Default::default()
            })
            .await
            .map_err(|e| NatsBusError::JetStream(format!("create stream: {e}")))?;

        let fanout = DashMap::new();
        for topic in Topic::ALL {
            let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
            fanout.insert(*topic, tx);
        }

        Ok(Arc::new(Self {
            jetstream,
            stream_name,
            subject_prefix,
            fanout,
            active_pollers: Mutex::new(HashSet::new()),
            pollers: Mutex::new(Vec::new()),
        }))
    }

    fn subject_for(&self, topic: Topic) -> String {
        format!("{}.{}", self.subject_prefix, topic.as_str())
    }

    fn consumer_name(topic: Topic) -> String {
        format!("orionii_{}", topic.as_str().replace('.', "_"))
    }

    fn ensure_poller(&self, topic: Topic, fanout: broadcast::Sender<Envelope>) {
        let mut active = self
            .active_pollers
            .lock()
            .expect("nats active-poller mutex poisoned");
        if !active.insert(topic) {
            return;
        }

        let handle = spawn_poller(
            self.jetstream.clone(),
            self.stream_name.clone(),
            self.subject_for(topic),
            Self::consumer_name(topic),
            topic,
            fanout,
        );
        self.pollers
            .lock()
            .expect("nats poller mutex poisoned")
            .push(handle);
    }
}

impl Drop for NatsJetStreamBus {
    fn drop(&mut self) {
        if let Ok(mut handles) = self.pollers.lock() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }
    }
}

#[async_trait]
impl EventBus for NatsJetStreamBus {
    async fn publish(&self, env: Envelope) -> Result<(), BusError> {
        let topic = env.topic;
        let subject = self.subject_for(topic);
        let bytes = serde_json::to_vec(&env)
            .map_err(|e| BusError::Transport(format!("serialize envelope: {e}")))?;

        let ack = self
            .jetstream
            .publish(subject, bytes.into())
            .await
            .map_err(|e| BusError::Transport(format!("nats publish: {e}")))?;
        ack.await
            .map_err(|e| BusError::Transport(format!("nats publish ack: {e}")))?;
        Ok(())
    }

    fn subscribe(&self, topic: Topic) -> BusReceiver {
        let tx = self
            .fanout
            .get(&topic)
            .expect("topic pre-registered in NatsJetStreamBus::connect")
            .clone();
        let rx = tx.subscribe();
        self.ensure_poller(topic, tx);
        BusReceiver::from_nats(rx)
    }
}

fn spawn_poller(
    jetstream: jetstream::Context,
    stream_name: String,
    subject: String,
    consumer_name: String,
    enum_topic: Topic,
    fanout: broadcast::Sender<Envelope>,
) -> JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let stream = match jetstream.get_stream(&stream_name).await {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("[nats-bus] could not bind stream {stream_name}: {error}");
                return;
            }
        };

        let consumer = match stream
            .get_or_create_consumer(
                &consumer_name,
                pull::Config {
                    durable_name: Some(consumer_name.clone()),
                    filter_subject: subject.clone(),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(consumer) => consumer,
            Err(error) => {
                eprintln!(
                    "[nats-bus] could not create consumer {consumer_name} for {subject}: {error}"
                );
                return;
            }
        };

        let mut messages = match consumer.messages().await {
            Ok(messages) => messages,
            Err(error) => {
                eprintln!("[nats-bus] could not open message stream for {subject}: {error}");
                return;
            }
        };

        while let Some(next) = messages.next().await {
            match next {
                Ok(message) => match serde_json::from_slice::<Envelope>(&message.payload) {
                    Ok(env) if env.topic == enum_topic => {
                        let _ = fanout.send(env);
                        if let Err(error) = message.ack().await {
                            eprintln!("[nats-bus] ack failed for {subject}: {error}");
                        }
                    }
                    Ok(env) => {
                        eprintln!(
                                "[nats-bus] envelope topic {:?} did not match consumer topic {:?}; dropping",
                                env.topic, enum_topic
                            );
                        let _ = message.ack().await;
                    }
                    Err(error) => {
                        eprintln!(
                            "[nats-bus] dropped malformed envelope on subject {subject}: {error}"
                        );
                        let _ = message.ack().await;
                    }
                },
                Err(error) => {
                    eprintln!("[nats-bus] consumer error on {subject}: {error}");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }

        eprintln!("[nats-bus] consumer stream ended for {subject}");
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[tokio::test]
    #[ignore = "requires a running nats-server with JetStream enabled"]
    async fn publish_round_trips_through_jetstream() {
        let endpoint =
            std::env::var("ORIONII_NATS_TEST_ENDPOINT").unwrap_or_else(|_| "127.0.0.1:4222".into());
        let bus = NatsJetStreamBus::connect(&endpoint, Uuid::new_v4())
            .await
            .expect("connect to nats jetstream");
        let mut rx = bus.subscribe(Topic::EgoAction);
        let correlation_id = Uuid::new_v4();
        let expected = Envelope::new(
            Topic::EgoAction,
            "agent-live-nats",
            "soul:test",
            Some(correlation_id),
            json!({ "message": "through jetstream" }),
        );

        bus.publish(expected.clone())
            .await
            .expect("publish envelope through nats");

        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("receive envelope before timeout")
            .expect("receive envelope");
        assert_eq!(received.topic, Topic::EgoAction);
        assert_eq!(received.agent_id, expected.agent_id);
        assert_eq!(received.soul_ref, expected.soul_ref);
        assert_eq!(received.correlation_id, Some(correlation_id));
        assert_eq!(received.payload, expected.payload);
    }
}
