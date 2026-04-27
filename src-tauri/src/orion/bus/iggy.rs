//! Iggy-backed implementation of `EventBus`. Phase 2b transport.
//!
//! One stream per entity (`orionii.entity.{orion_id}`), one Iggy topic per
//! `Topic` enum variant. Topics are created on first connect and survive
//! restart — that's the durability story Phase 2b ships.
//!
//! ## Polling model
//!
//! Iggy's API is poll-based. We pre-spawn one polling task per topic that
//! pulls envelopes from the iggy server and re-broadcasts them locally
//! through a tokio broadcast channel. Subscribers get a fresh receiver on
//! that local channel, so `subscribe(topic)` stays cheap and sync. The
//! polling cadence is configured via the `IggyConsumer` builder
//! (`poll_interval`).
//!
//! ## Consumer groups
//!
//! Each polling task uses a consumer group named after the entity's
//! `orion_id`. iggy tracks the offset server-side, so when OrionII
//! restarts, the entity resumes where it left off. The bus survives
//! process death — that's the whole point of Phase 2b.
//!
//! ## Phase 2b limitations
//!
//! - Login uses the iggy-server bootstrap admin credentials by default
//!   (`iggy`/`iggy`). The proper PAT-mint flow is in `iggy_auth.rs` with a
//!   `TODO(phase-2b-pat-mint)` marker. Until that's wired, treat the
//!   bundled-Iggy path as dev-grade.
//! - The polling task does not yet emit `RecvError::Lagged` when the local
//!   broadcast channel overflows; it just logs. Phase 2.1 will surface
//!   lag through the same `RecvError::Lagged` shape `InMemoryBus` uses.
//! - On connection drop, the polling task exits and is not auto-respawned;
//!   the entire `IggyBus` is rebuilt by the supervisor when the sidecar
//!   restarts. Phase 2.1 will add per-task reconnect.

use std::sync::Arc;

use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use iggy::prelude::*;
use tauri::async_runtime::JoinHandle;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use uuid::Uuid;

use super::{BusError, BusReceiver, Envelope, EventBus, Topic};

const CHANNEL_CAPACITY: usize = 1024;
const PARTITIONS_COUNT: u32 = 1;
/// Polling cadence for the per-topic consumer task. 100ms keeps chat
/// latency low without saturating the iggy server.
const POLL_INTERVAL_MS: u64 = 100;
/// Batch size per poll. Iggy will return up to this many messages per
/// `poll_messages` round-trip.
const BATCH_SIZE: u32 = 64;

#[derive(Debug, Error)]
pub enum IggyBusError {
    #[error("iggy client error: {0}")]
    Iggy(String),
    #[error("could not parse identifier: {0}")]
    Identifier(String),
}

impl From<IggyError> for IggyBusError {
    fn from(value: IggyError) -> Self {
        IggyBusError::Iggy(format!("{value}"))
    }
}

pub struct IggyBus {
    client: Arc<IggyClient>,
    stream_name: String,
    fanout: DashMap<Topic, broadcast::Sender<Envelope>>,
    /// Held to abort polling tasks when the bus is dropped.
    _pollers: Vec<JoinHandle<()>>,
}

impl IggyBus {
    /// Connect to an iggy node, authenticate, and ensure the entity stream
    /// + 8 topics exist. Then spawn one polling task per topic.
    ///
    /// `endpoint` is `host:port` for TCP transport (e.g. `127.0.0.1:8090`).
    /// `username`/`password` use iggy's bootstrap admin pair until the
    /// PAT-mint flow lands (see `iggy_auth.rs` TODO).
    pub async fn connect(
        endpoint: &str,
        username: &str,
        password: &str,
        orion_id: Uuid,
    ) -> Result<Arc<Self>, IggyBusError> {
        let stream_name = format!("orionii.entity.{orion_id}");

        // Build + connect the underlying TCP client.
        let client = IggyClient::builder()
            .with_tcp()
            .with_server_address(endpoint.to_string())
            .build()
            .map_err(IggyBusError::from)?;
        client.connect().await.map_err(IggyBusError::from)?;
        client
            .login_user(username, password)
            .await
            .map_err(IggyBusError::from)?;

        let client = Arc::new(client);

        // Idempotent stream + topic creation.
        ensure_stream(&client, &stream_name).await?;
        let stream_id =
            Identifier::named(&stream_name).map_err(|e| IggyBusError::Identifier(e.to_string()))?;
        for topic in Topic::ALL {
            ensure_topic(&client, &stream_id, topic.as_str()).await?;
        }

        // Pre-create per-topic broadcast channels and pollers.
        let fanout: DashMap<Topic, broadcast::Sender<Envelope>> = DashMap::new();
        let mut pollers = Vec::new();
        let consumer_group = format!("orionii.entity.{orion_id}.consumer");
        for topic in Topic::ALL {
            let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
            fanout.insert(*topic, tx.clone());

            let handle = spawn_poller(
                client.clone(),
                stream_name.clone(),
                topic.as_str().to_string(),
                consumer_group.clone(),
                *topic,
                tx,
            );
            pollers.push(handle);
        }

        Ok(Arc::new(Self {
            client,
            stream_name,
            fanout,
            _pollers: pollers,
        }))
    }
}

#[async_trait]
impl EventBus for IggyBus {
    async fn publish(&self, env: Envelope) -> Result<(), BusError> {
        let topic = env.topic;
        let stream_id = Identifier::named(&self.stream_name)
            .map_err(|e| BusError::Transport(format!("stream id: {e}")))?;
        let topic_id = Identifier::named(topic.as_str())
            .map_err(|e| BusError::Transport(format!("topic id: {e}")))?;

        let bytes = serde_json::to_vec(&env)
            .map_err(|e| BusError::Transport(format!("serialize envelope: {e}")))?;
        let message = IggyMessage::builder()
            .payload(bytes.into())
            .build()
            .map_err(|e| BusError::Transport(format!("build IggyMessage: {e}")))?;

        let mut messages = vec![message];
        let partitioning = Partitioning::balanced();
        self.client
            .send_messages(&stream_id, &topic_id, &partitioning, &mut messages)
            .await
            .map_err(|e| BusError::Transport(format!("send_messages: {e}")))?;
        Ok(())
    }

    fn subscribe(&self, topic: Topic) -> BusReceiver {
        let tx = self
            .fanout
            .get(&topic)
            .expect("topic pre-registered in IggyBus::connect");
        BusReceiver::from_iggy(tx.subscribe())
    }
}

async fn ensure_stream(client: &Arc<IggyClient>, name: &str) -> Result<(), IggyBusError> {
    let id = Identifier::named(name).map_err(|e| IggyBusError::Identifier(e.to_string()))?;
    if client
        .get_stream(&id)
        .await
        .map_err(IggyBusError::from)?
        .is_none()
    {
        client
            .create_stream(name)
            .await
            .map_err(IggyBusError::from)?;
    }
    Ok(())
}

async fn ensure_topic(
    client: &Arc<IggyClient>,
    stream_id: &Identifier,
    name: &str,
) -> Result<(), IggyBusError> {
    let topic_id = Identifier::named(name).map_err(|e| IggyBusError::Identifier(e.to_string()))?;
    if client
        .get_topic(stream_id, &topic_id)
        .await
        .map_err(IggyBusError::from)?
        .is_none()
    {
        client
            .create_topic(
                stream_id,
                name,
                PARTITIONS_COUNT,
                CompressionAlgorithm::None,
                None,
                IggyExpiry::ServerDefault,
                MaxTopicSize::ServerDefault,
            )
            .await
            .map_err(IggyBusError::from)?;
    }
    Ok(())
}

fn spawn_poller(
    client: Arc<IggyClient>,
    stream: String,
    topic: String,
    consumer_group: String,
    enum_topic: Topic,
    fanout: broadcast::Sender<Envelope>,
) -> JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let consumer_builder = match client.consumer_group(&consumer_group, &stream, &topic) {
            Ok(b) => b,
            Err(error) => {
                eprintln!("[iggy-bus] could not build consumer for {topic}: {error}");
                return;
            }
        };
        let mut consumer = consumer_builder
            .auto_commit(AutoCommit::When(AutoCommitWhen::PollingMessages))
            .auto_join_consumer_group()
            .create_consumer_group_if_not_exists()
            .batch_length(BATCH_SIZE)
            .poll_interval(IggyDuration::new(Duration::from_millis(POLL_INTERVAL_MS)))
            .polling_strategy(PollingStrategy::next())
            .build();

        if let Err(error) = consumer.init().await {
            eprintln!("[iggy-bus] consumer init failed for {topic}: {error}");
            return;
        }

        loop {
            match consumer.next().await {
                Some(Ok(received)) => {
                    let bytes = received.message.payload.as_ref();
                    match serde_json::from_slice::<Envelope>(bytes) {
                        Ok(env) => {
                            // Sanity: the envelope's topic must match the
                            // consumer's topic. If it doesn't, the iggy
                            // server has events on the wrong stream/topic
                            // shape — log and drop.
                            if env.topic != enum_topic {
                                eprintln!(
                                    "[iggy-bus] envelope topic {:?} did not match consumer topic {:?}; dropping",
                                    env.topic, enum_topic
                                );
                                continue;
                            }
                            let _ = fanout.send(env);
                        }
                        Err(error) => {
                            eprintln!(
                                "[iggy-bus] dropped malformed envelope on topic {topic}: {error}"
                            );
                        }
                    }
                }
                Some(Err(error)) => {
                    eprintln!("[iggy-bus] consumer error on {topic}: {error}");
                    // Brief backoff and continue; the iggy client retries
                    // its own connection internally for transient errors.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                None => {
                    // Stream ended (connection closed). Exit; the bus's
                    // owner (OrionCore) will rebuild on supervisor restart.
                    eprintln!("[iggy-bus] consumer stream ended for {topic}");
                    return;
                }
            }
        }
    })
}
