use serde::{Deserialize, Serialize};

/// The Id subscriber publishes this onto `Topic::IdReaction`; the Ego
/// subscriber consumes it without importing the Id participant module.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdSignal {
    pub identity_version: u64,
    pub personality_signal: String,
    pub drives: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdReactionPayload {
    pub user_query: String,
    pub system_prompt: String,
    pub context_summary: String,
    pub id_signal: IdSignal,
    pub ethics_guidance: Vec<String>,
}

/// The Ego subscriber publishes this onto `Topic::EgoAction`. The UI emitter,
/// local Superego, and egress audit path consume the shared DTO from here
/// instead of importing the Ego participant directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgoActionPayload {
    pub user_query: String,
    pub response_text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperegoLocalEvaluation {
    pub accepted: bool,
    pub soul_ref: String,
    pub note: String,
}
