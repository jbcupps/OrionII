use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use tokio::time::error::Elapsed;
use tracing::{error, info, warn};

use crate::orion::bus::{current_soul_ref, Envelope, RecvError, SharedBus, Topic};
use crate::orion::charter::SharedCharter;
use crate::orion::curator::{CuratedRaw, CuratorRuntime};
use crate::orion::model::ModelRouter;
use crate::orion::payloads::IdReactionPayload;
use crate::orion::persistence::{FilePersistence, Persistence};

/// Upper bound on the entire Id curation pass (which may call the model
/// layer for the personality consult). Defence in depth on top of the
/// per-request reqwest timeout — a wedged provider must never pin the Id
/// subscriber forever, otherwise no IdReaction reaches Ego.
const ID_CURATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn the Id subscriber task.
///
/// Subscribes to `Topic::MentorInput`, runs the curator + Id pipeline against
/// the persisted identity and document context, and publishes
/// `Topic::IdReaction`. The Ego subscriber takes it from there.
pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    model: Arc<ModelRouter>,
    charter: SharedCharter,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::MentorInput);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_mentor_input(&bus, &persistence, &model, &charter, env).await,
                Err(RecvError::Lagged(skipped)) => {
                    warn!(target: "orion::id", skipped, "lagged on MentorInput");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

async fn handle_mentor_input(
    bus: &SharedBus,
    persistence: &Arc<Mutex<FilePersistence>>,
    model: &Arc<ModelRouter>,
    charter: &SharedCharter,
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

    let agent_id = env.agent_id.clone();
    let correlation_id = env.correlation_id;

    info!(
        target: "orion::id",
        correlation_id = ?correlation_id,
        query_bytes = user_query.len(),
        "mentor_input_received"
    );

    let curator = CuratorRuntime::default();

    // Lock briefly to clone the identity and retrieve document context.
    // Release before .await on the model.
    let (identity, document_context) = {
        let p = persistence.lock().expect("persistence mutex poisoned");
        let identity = p.identity().clone();
        let document_context = curator.retrieve_documents(&user_query, p.document_chunks());
        (identity, document_context)
    };
    let soul_ref = {
        let c = charter.read().expect("charter rwlock poisoned");
        current_soul_ref(&c)
    };

    let curated = match tokio::time::timeout(
        ID_CURATION_TIMEOUT,
        curator.curate_raw(&user_query, &identity, &document_context, model.as_ref()),
    )
    .await
    {
        Ok(curated) => curated,
        Err(Elapsed { .. }) => {
            warn!(
                target: "orion::id",
                correlation_id = ?correlation_id,
                "curation timed out; using degraded scaffold"
            );
            degraded_curated(&user_query, &identity)
        }
    };

    let payload = IdReactionPayload {
        user_query,
        system_prompt: curated.system_prompt,
        context_summary: curated.context_summary,
        id_signal: curated.id_signal,
        ethics_guidance: curated.ethics_guidance,
    };

    let value = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
    let reaction = Envelope::new(Topic::IdReaction, agent_id, soul_ref, correlation_id, value);
    info!(
        target: "orion::id",
        correlation_id = ?correlation_id,
        "id_reaction_published"
    );
    if let Err(error) = bus.publish(reaction).await {
        error!(
            target: "orion::id",
            correlation_id = ?correlation_id,
            %error,
            "failed to publish IdReaction"
        );
    }
}

/// Synthesise a curator output without calling the model layer. Used when
/// curation hits the timeout deadline — the Ego subscriber still gets a
/// well-formed `IdReaction` so an `EgoAction` is always produced.
fn degraded_curated(
    user_query: &str,
    identity: &crate::orion::identity::IdentityState,
) -> CuratedRaw {
    use crate::orion::ethics::{EthicsOverlay, EthicsScaffoldInput};
    use crate::orion::payloads::IdSignal;

    let ethics = EthicsOverlay::scaffold(EthicsScaffoldInput { user_query }, &identity.ethics_lean);
    let id_signal = IdSignal {
        identity_version: identity.version,
        personality_signal: format!(
            "{} remains {}. Id curation degraded due to timeout.",
            identity.personality.name, identity.personality.stance
        ),
        drives: identity.drives.clone(),
    };
    let system_prompt = format!(
        "{}\n\nId signal: {}\n\nEthics: {}",
        identity.personality.continuity_note,
        id_signal.personality_signal,
        ethics.guidance.join(" ")
    );

    CuratedRaw {
        id_signal,
        system_prompt,
        context_summary: "No matching local document context.".to_string(),
        ethics_guidance: ethics.guidance,
    }
}
