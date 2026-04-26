use serde::{Deserialize, Serialize};

use crate::orion::identity::IdentityState;
use crate::orion::model::ModelProvider;

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
