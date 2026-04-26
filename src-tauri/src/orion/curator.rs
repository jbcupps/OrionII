use crate::orion::ethics::EthicsOverlay;
use crate::orion::id::{IdRuntime, IdSignal};
use crate::orion::identity::IdentityState;
use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
use crate::orion::model::ModelProvider;
use crate::orion::skills::DocumentSkill;
use crate::orion::topics;

#[derive(Default)]
pub struct CuratorRuntime {
    id: IdRuntime,
    documents: DocumentSkill,
}

pub struct CuratedTurn {
    pub id_signal: IdSignal,
    pub ethics: EthicsOverlay,
    pub instruction: Message,
}

impl CuratorRuntime {
    pub fn curate(
        &self,
        input: &Message,
        identity: &IdentityState,
        document_context: &str,
        model: &impl ModelProvider,
    ) -> CuratedTurn {
        let user_query = match &input.payload {
            Payload::UserInput { text } => text.clone(),
            _ => String::new(),
        };

        let id_signal = self
            .id
            .consult(identity, &user_query, document_context, model);
        let ethics = EthicsOverlay::scaffold(input, &identity.ethics_lean);
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

        let instruction = Message::new(
            MessageKind::CuratedPrompt,
            Author::Curator,
            topics::EGO_INSTRUCTIONS,
            Priority::UserInput,
            input.session_id,
            input.correlation_id,
            Some(input.id),
            Payload::CuratedPrompt {
                system_prompt,
                user_query,
                context_summary,
            },
        );

        CuratedTurn {
            id_signal,
            ethics,
            instruction,
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
