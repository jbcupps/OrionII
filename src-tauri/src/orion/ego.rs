use crate::orion::ethics::EthicsOverlay;
use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
use crate::orion::model::{ModelPrompt, ModelProvider};
use crate::orion::topics;

#[derive(Default)]
pub struct EgoRuntime;

impl EgoRuntime {
    pub fn respond(
        &self,
        instruction: &Message,
        ethics: &EthicsOverlay,
        model: &impl ModelProvider,
    ) -> Message {
        let prompt = match &instruction.payload {
            Payload::CuratedPrompt {
                system_prompt,
                user_query,
                context_summary,
            } => ModelPrompt {
                system_prompt: system_prompt.clone(),
                user_query: user_query.clone(),
                context: context_summary.clone(),
            },
            _ => ModelPrompt {
                system_prompt: String::new(),
                user_query: String::new(),
                context: String::new(),
            },
        };

        let response = model.generate_ego_response(&prompt, ethics).unwrap_or_else(|error| {
            format!(
                "Orion is operating in degraded local cognition mode. I heard: \"{}\"\n\nModel error: {}",
                prompt.user_query, error
            )
        });

        Message::new(
            MessageKind::UserOutput,
            Author::Ego,
            topics::USER_CHAT_OUTPUT,
            Priority::UserInput,
            instruction.session_id,
            instruction.correlation_id,
            Some(instruction.id),
            Payload::ChatOutput { text: response },
        )
    }
}
