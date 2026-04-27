use std::sync::Arc;

use tauri::async_runtime::JoinHandle;

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::ethics::{EthicsOverlay, EthicsOverlaySource};
use crate::orion::model::{ModelPrompt, ModelProvider, ModelRouter};
use crate::orion::payloads::{EgoActionPayload, IdReactionPayload};

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
        model: &dyn ModelProvider,
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
        agent_id.clone(),
        soul_ref.clone(),
        correlation_id,
        value,
    );
    if let Err(error) = bus.publish(envelope).await {
        eprintln!("[ego-subscriber] failed to publish EgoAction: {error}");
    }

    let outbound = Envelope::new(
        Topic::EgressOutbound,
        agent_id,
        soul_ref,
        correlation_id,
        serde_json::json!({
            "action": Topic::EgoAction.as_str(),
            "sourceTopic": Topic::EgoAction.as_str(),
            "responseBytes": action.response_text.len(),
        }),
    );
    if let Err(error) = bus.publish(outbound).await {
        eprintln!("[ego-subscriber] failed to publish EgressOutbound audit: {error}");
    }
}
