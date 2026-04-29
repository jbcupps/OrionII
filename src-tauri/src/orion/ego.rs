use std::sync::Arc;
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use tokio::time::error::Elapsed;
use tracing::{error, info, warn};

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::ethics::{EthicsOverlay, EthicsOverlaySource};
use crate::orion::model::{ModelPrompt, ModelProvider, ModelRouter};
use crate::orion::payloads::{EgoActionPayload, IdReactionPayload};

/// Upper bound on the entire Ego model call. The underlying `reqwest`
/// client already has its own per-request timeout; this is defence in depth
/// so a wedged provider can never pin the Ego subscriber forever — at the
/// deadline we publish a degraded `EgoAction` and the chat surface keeps
/// flowing.
const EGO_MODEL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

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
        match tokio::time::timeout(
            EGO_MODEL_CALL_TIMEOUT,
            model.generate_ego_response(prompt, ethics),
        )
        .await
        {
            Ok(Ok(text)) => text,
            Ok(Err(error)) => degraded_text(&prompt.user_query, &error.to_string()),
            Err(Elapsed { .. }) => degraded_text(
                &prompt.user_query,
                &format!(
                    "model call timed out after {}s",
                    EGO_MODEL_CALL_TIMEOUT.as_secs()
                ),
            ),
        }
    }
}

fn degraded_text(user_query: &str, detail: &str) -> String {
    format!(
        "Orion is operating in degraded local cognition mode. I heard: \"{}\"\n\nModel error: {}",
        user_query, detail
    )
}

/// Spawn the Ego subscriber task.
pub fn spawn(bus: SharedBus, model: Arc<ModelRouter>) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::IdReaction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_id_reaction(&bus, &model, env).await,
                Err(RecvError::Lagged(skipped)) => {
                    warn!(target: "orion::ego", skipped, "lagged on IdReaction");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

async fn handle_id_reaction(bus: &SharedBus, model: &Arc<ModelRouter>, env: Envelope) {
    let Ok(reaction) = serde_json::from_value::<IdReactionPayload>(env.payload.clone()) else {
        warn!(
            target: "orion::ego",
            correlation_id = ?env.correlation_id,
            "dropped malformed IdReaction payload"
        );
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
    info!(
        target: "orion::ego",
        correlation_id = ?correlation_id,
        response_bytes = action.response_text.len(),
        "ego_action_published"
    );
    if let Err(error) = bus.publish(envelope).await {
        error!(
            target: "orion::ego",
            correlation_id = ?correlation_id,
            %error,
            "failed to publish EgoAction"
        );
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
        error!(
            target: "orion::ego",
            correlation_id = ?correlation_id,
            %error,
            "failed to publish EgressOutbound audit"
        );
    }
}
