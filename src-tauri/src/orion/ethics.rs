use serde::{Deserialize, Serialize};

use crate::orion::identity::EthicsLean;
use crate::orion::message::Message;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthicsOverlay {
    pub deontological: f32,
    pub virtue: f32,
    pub consequential: f32,
    pub guidance: Vec<String>,
    pub source: EthicsOverlaySource,
}

impl EthicsOverlay {
    pub fn scaffold(input: &Message, lean: &EthicsLean) -> Self {
        let mut guidance = vec![
            "Respect the worker's agency and local ownership.".to_string(),
            "Prefer truthful, reversible, and inspectable actions.".to_string(),
            "Escalate uncertainty instead of pretending to know.".to_string(),
        ];

        if input.ttl_cycles >= input.ttl_max {
            guidance.push(
                "Treat this thread as potentially stuck and avoid recursive action.".to_string(),
            );
        }

        Self {
            deontological: lean.deontological,
            virtue: lean.virtue,
            consequential: lean.consequential,
            guidance,
            source: EthicsOverlaySource::LocalScaffold,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EthicsOverlaySource {
    LocalScaffold,
    SaoPolicy,
}
