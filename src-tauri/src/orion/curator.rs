use crate::orion::ethics::{EthicsOverlay, EthicsScaffoldInput};
use crate::orion::identity::IdentityState;
use crate::orion::model::ModelProvider;
use crate::orion::payloads::IdSignal;
use crate::orion::skills::DocumentSkill;

#[derive(Default)]
pub struct CuratorRuntime {
    id: IdRuntime,
    documents: DocumentSkill,
}

#[derive(Default)]
struct IdRuntime;

impl IdRuntime {
    async fn consult(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
        model: &dyn ModelProvider,
    ) -> IdSignal {
        let personality_signal = model
            .consult_id(identity, query, context)
            .await
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

/// Raw curated payload — the data shape the bus pipeline carries on
/// `Topic::IdReaction`. Subscribers consume this directly; there are no
/// `Message` round-trips inside the bus.
pub struct CuratedRaw {
    pub id_signal: IdSignal,
    pub system_prompt: String,
    pub context_summary: String,
    pub ethics_guidance: Vec<String>,
}

impl CuratorRuntime {
    pub async fn curate_raw(
        &self,
        user_query: &str,
        identity: &IdentityState,
        document_context: &str,
        model: &dyn ModelProvider,
    ) -> CuratedRaw {
        let id_signal = self
            .id
            .consult(identity, user_query, document_context, model)
            .await;
        let ethics =
            EthicsOverlay::scaffold(EthicsScaffoldInput { user_query }, &identity.ethics_lean);
        let context_summary = if document_context.is_empty() {
            "No matching local document context.".to_string()
        } else {
            document_context.to_string()
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
            context_summary,
            ethics_guidance: ethics.guidance,
        }
    }

    pub fn retrieve_documents(
        &self,
        query: &str,
        chunks: &[crate::orion::memory::DocumentChunk],
    ) -> String {
        self.documents.retrieve_context(query, chunks)
    }
}
