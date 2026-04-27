use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tokio::sync::broadcast;

use crate::orion::bus::{Envelope, SharedBus, Topic};
use crate::orion::ethics::{EthicsOverlay, EthicsOverlaySource};
use crate::orion::id::IdReactionPayload;
use crate::orion::model::{ModelPrompt, ModelProvider, ModelRouter};

#[derive(Default)]
pub struct EgoRuntime;

impl EgoRuntime {
    /// Run the model and return the response text. The Ego subscriber wraps
    /// this in an `EgoActionPayload`; tests can call this directly with a
    /// fake `ModelProvider`.
    pub fn respond_text(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
        model: &impl ModelProvider,
    ) -> String {
        model
            .generate_ego_response(prompt, ethics)
            .unwrap_or_else(|error| {
                format!(
                    "Orion is operating in degraded local cognition mode. I heard: \"{}\"\n\nModel error: {}",
                    prompt.user_query, error
                )
            })
    }
}

/// The shape an Ego subscriber publishes onto `Topic::EgoAction`. The UI
/// emitter forwards this to the React app via a Tauri event; the local
/// Superego subscriber inspects it for governance evaluation; the egress
/// subscriber sanitizes and ships it to SAO.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgoActionPayload {
    pub user_query: String,
    pub response_text: String,
}

/// Spawn the Ego subscriber task.
///
/// Subscribes to `Topic::IdReaction`, runs the configured model, and
/// publishes `Topic::EgoAction`. This is the participant the UI ultimately
/// observes — but the UI never reaches in here. It listens to the bus.
pub fn spawn(bus: SharedBus, model: Arc<ModelRouter>) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::IdReaction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_id_reaction(&bus, &model, env).await,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    eprintln!(
                        "[ego-subscriber] lagged on IdReaction, skipped {skipped} envelopes"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn handle_id_reaction(bus: &SharedBus, model: &Arc<ModelRouter>, env: Envelope) {
    let Ok(reaction) = serde_json::from_value::<IdReactionPayload>(env.payload.clone()) else {
        eprintln!("[ego-subscriber] dropped malformed IdReaction payload");
        return;
    };

    let agent_id = env.agent_id.clone();
    let soul_ref = env.soul_ref.clone();
    let correlation_id = env.correlation_id;

    let prompt = ModelPrompt {
        system_prompt: reaction.system_prompt.clone(),
        user_query: reaction.user_query.clone(),
        context: reaction.context_summary.clone(),
    };
    let ethics = EthicsOverlay {
        deontological: 0.34,
        virtue: 0.33,
        consequential: 0.33,
        guidance: reaction.ethics_guidance.clone(),
        source: EthicsOverlaySource::LocalScaffold,
    };

    // The model call may make a blocking HTTP request (Ollama / SAO proxy).
    // `block_in_place` keeps the work on the current worker without
    // spawning a detached task that could outlive an aborted parent.
    let response_text = tokio::task::block_in_place(|| {
        EgoRuntime.respond_text(&prompt, &ethics, model.as_ref())
    });

    let action = EgoActionPayload {
        user_query: reaction.user_query,
        response_text,
    };
    let value = serde_json::to_value(&action).unwrap_or(serde_json::Value::Null);

    let envelope = Envelope::new(
        Topic::EgoAction,
        agent_id,
        soul_ref,
        correlation_id,
        value,
    );
    if let Err(error) = bus.publish(envelope) {
        eprintln!("[ego-subscriber] failed to publish EgoAction: {error}");
    }
}
