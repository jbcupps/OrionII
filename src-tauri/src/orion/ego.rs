use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::ethics::{EthicsOverlay, EthicsOverlaySource};
use crate::orion::id::IdReactionPayload;
use crate::orion::model::{ModelPrompt, ModelProvider, ModelRouter};

#[derive(Default)]
pub struct EgoRuntime;

impl EgoRuntime {
    /// Run the model and return the response text. The Ego subscriber wraps
    /// this in an `EgoActionPayload`; tests can call this directly with a
    /// fake `ModelProvider`.
    pub async fn respond_text(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
        model: &(dyn ModelProvider),
    ) -> String {
        model
            .generate_ego_response(prompt, ethics)
            .await
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
pub fn spawn(bus: SharedBus, model: Arc<ModelRouter>) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::IdReaction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_id_reaction(&bus, &model, env).await,
                Err(RecvError::Lagged(skipped)) => {
                    eprintln!("[ego-subscriber] lagged on IdReaction, skipped {skipped} envelopes");
                }
                Err(RecvError::Closed) => break,
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

    let response_text = EgoRuntime
        .respond_text(&prompt, &ethics, model.as_ref())
        .await;

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
    if let Err(error) = bus.publish(envelope).await {
        eprintln!("[ego-subscriber] failed to publish EgoAction: {error}");
    }
}
