use serde::{Deserialize, Serialize};

use crate::orion::identity::EthicsLean;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthicsOverlay {
    pub deontological: f32,
    pub virtue: f32,
    pub consequential: f32,
    pub guidance: Vec<String>,
    pub source: EthicsOverlaySource,
}

/// Inputs to the ethics scaffold beyond the identity's lean weights.
///
/// Phase 1 only consults the user query for shape — but keeping a struct
/// here makes adding context (recent egress traffic, current SAO policy
/// version, etc.) a non-breaking change.
pub struct EthicsScaffoldInput<'a> {
    pub user_query: &'a str,
}

impl EthicsOverlay {
    pub fn scaffold(_input: EthicsScaffoldInput<'_>, lean: &EthicsLean) -> Self {
        let guidance = vec![
            "Respect the worker's agency and local ownership.".to_string(),
            "Prefer truthful, reversible, and inspectable actions.".to_string(),
            "Escalate uncertainty instead of pretending to know.".to_string(),
        ];

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
