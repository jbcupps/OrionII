use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tokio::sync::broadcast;

use crate::orion::bus::{current_soul_ref, Envelope, SharedBus, Topic};
use crate::orion::curator::CuratorRuntime;
use crate::orion::identity::IdentityState;
use crate::orion::model::{ModelProvider, ModelRouter};
use crate::orion::persistence::{FilePersistence, Persistence};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdSignal {
    pub identity_version: u64,
    pub personality_signal: String,
    pub drives: Vec<String>,
}

#[derive(Default)]
pub struct IdRuntime;

impl IdRuntime {
    pub fn consult(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
        model: &impl ModelProvider,
    ) -> IdSignal {
        let personality_signal =
            model
                .consult_id(identity, query, context)
                .unwrap_or_else(|error| {
                    format!(
                        "{} remains {}. Local Id model degraded: {}.",
                        identity.personality.name, identity.personality.stance, error
                    )
                });

        IdSignal {
            identity_version: identity.version,
            personality_signal,
            drives: identity.drives.clone(),
        }
    }
}

/// The shape an Id subscriber publishes onto `Topic::IdReaction`. The Ego
/// subscriber deserializes this and turns it into a model prompt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdReactionPayload {
    pub user_query: String,
    pub system_prompt: String,
    pub context_summary: String,
    pub id_signal: IdSignal,
    pub ethics_guidance: Vec<String>,
}

/// Spawn the Id subscriber task.
///
/// Subscribes to `Topic::MentorInput`, runs the curator + Id pipeline against
/// the persisted identity and document context, and publishes
/// `Topic::IdReaction`. The Ego subscriber takes it from there.
///
/// This is a *participant* on the bus, not an implementation detail of any
/// other module. Direct calls into `CuratorRuntime` or `IdRuntime` from
/// outside this file are an architectural regression — see CLAUDE.md.
pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    model: Arc<ModelRouter>,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::MentorInput);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_mentor_input(&bus, &persistence, &model, env).await,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    eprintln!(
                        "[id-subscriber] lagged on MentorInput, skipped {skipped} envelopes"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Bus dropped — exit cleanly.
                    break;
                }
            }
        }
    })
}

async fn handle_mentor_input(
    bus: &SharedBus,
    persistence: &Arc<Mutex<FilePersistence>>,
    model: &Arc<ModelRouter>,
    env: Envelope,
) {
    let user_query = env
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if user_query.trim().is_empty() {
        return;
    }

    // Sync model + persistence work runs via `block_in_place`, which keeps
    // the work on the current async worker but tells the runtime to spin
    // up a replacement worker so other subscribers stay responsive.
    // Unlike `spawn_blocking`, this does not create a detached task that
    // could outlive an aborted parent — important when `OrionCore::drop`
    // aborts subscribers on hot-swap.
    let agent_id = env.agent_id.clone();
    let correlation_id = env.correlation_id;

    let curator = CuratorRuntime::default();

    let (soul_ref, identity, document_context) = {
        let p = persistence.lock().expect("persistence mutex poisoned");
        let identity = p.identity().clone();
        let document_context = curator.retrieve_documents(&user_query, p.document_chunks());
        let soul_ref = current_soul_ref(&identity);
        (soul_ref, identity, document_context)
    };

    let curated = tokio::task::block_in_place(|| {
        curator.curate_raw(&user_query, &identity, &document_context, model.as_ref())
    });

    let payload = IdReactionPayload {
        user_query,
        system_prompt: curated.system_prompt,
        context_summary: curated.context_summary,
        id_signal: curated.id_signal,
        ethics_guidance: curated.ethics_guidance,
    };

    let value = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
    let reaction = Envelope::new(
        Topic::IdReaction,
        agent_id,
        soul_ref,
        correlation_id,
        value,
    );
    if let Err(error) = bus.publish(reaction) {
        eprintln!("[id-subscriber] failed to publish IdReaction: {error}");
    }
}
