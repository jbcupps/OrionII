//! Persistence journal subscriber.
//!
//! The EventBus remains the integration boundary. This subscriber only
//! observes canonical topics and records the user-visible chat exchange into
//! the existing local message/memory store so cockpit counters reflect real
//! runtime activity. It also stamps an `EgoClock` whenever an `EgoAction`
//! lands so the cockpit can show the operator when the entity last replied
//! — silent spinners are confusing; a visible "last reply at" makes failure
//! modes legible.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use tauri::async_runtime::JoinHandle;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
use crate::orion::payloads::EgoActionPayload;
use crate::orion::persistence::{FilePersistence, Persistence};

/// Shared "last EgoAction observed" timestamp. Owned by `OrionCore`; the
/// journal subscriber writes to it, `companion_status` reads from it.
pub type EgoClock = Arc<Mutex<Option<DateTime<Utc>>>>;

pub fn new_ego_clock() -> EgoClock {
    Arc::new(Mutex::new(None))
}

pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    ego_clock: EgoClock,
) -> JoinHandle<()> {
    let mut mentor_rx = bus.subscribe(Topic::MentorInput);
    let mut ego_rx = bus.subscribe(Topic::EgoAction);

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                received = mentor_rx.recv() => {
                    match received {
                        Ok(env) => record_mentor_input(&persistence, env),
                        Err(RecvError::Lagged(skipped)) => {
                            warn!(target: "orion::journal", skipped, "lagged on MentorInput");
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
                received = ego_rx.recv() => {
                    match received {
                        Ok(env) => record_ego_action(&persistence, &ego_clock, env),
                        Err(RecvError::Lagged(skipped)) => {
                            warn!(target: "orion::journal", skipped, "lagged on EgoAction");
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

fn record_mentor_input(persistence: &Arc<Mutex<FilePersistence>>, env: Envelope) {
    let text = env
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return;
    }

    let correlation_id = env.correlation_id.unwrap_or_else(Uuid::new_v4);
    let message = Message::new(
        MessageKind::UserInput,
        Author::User,
        env.topic.as_str(),
        Priority::UserInput,
        correlation_id,
        correlation_id,
        None,
        Payload::UserInput { text },
    );
    record_message(persistence, &message);
}

fn record_ego_action(
    persistence: &Arc<Mutex<FilePersistence>>,
    ego_clock: &EgoClock,
    env: Envelope,
) {
    let correlation_id = env.correlation_id;
    let observed_at = env.occurred_at;

    let Ok(action) = serde_json::from_value::<EgoActionPayload>(env.payload) else {
        warn!(
            target: "orion::journal",
            correlation_id = ?correlation_id,
            "dropped malformed EgoAction payload"
        );
        return;
    };

    // Stamp the clock regardless of whether the body is empty; the operator
    // wants to know "the bus delivered a reply", even degraded.
    if let Ok(mut slot) = ego_clock.lock() {
        *slot = Some(observed_at);
    }

    let text = action.response_text.trim().to_string();
    if text.is_empty() {
        return;
    }

    let correlation_id = correlation_id.unwrap_or_else(Uuid::new_v4);
    let message = Message::new(
        MessageKind::UserOutput,
        Author::Ego,
        Topic::EgoAction.as_str(),
        Priority::Housekeeping,
        correlation_id,
        correlation_id,
        None,
        Payload::ChatOutput { text },
    );
    record_message(persistence, &message);
    info!(
        target: "orion::journal",
        correlation_id = ?Some(correlation_id),
        "journal_recorded ego_action"
    );
}

fn record_message(persistence: &Arc<Mutex<FilePersistence>>, message: &Message) {
    let mut p = persistence.lock().expect("persistence mutex poisoned");
    if let Err(error) = p.record_message(message) {
        error!(target: "orion::journal", %error, "failed to record message");
    }
}

/// Read the last-observed `Topic::EgoAction` timestamp. Returns `None` until
/// the journal subscriber has seen its first envelope.
pub fn last_ego_action_at(clock: &EgoClock) -> Option<DateTime<Utc>> {
    clock.lock().ok().and_then(|slot| *slot)
}
