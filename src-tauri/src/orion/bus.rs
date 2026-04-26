use crate::orion::message::Message;
use crate::orion::topics;

pub trait LocalBus {
    fn publish(&mut self, message: Message) -> Result<(), BusError>;
    fn messages_for_topic(&self, topic: &str) -> Vec<Message>;
    fn replay_since(&self, offset: usize) -> Vec<Message>;
    fn len(&self) -> usize;
}

#[derive(Default)]
pub struct InProcessBus {
    messages: Vec<Message>,
}

impl LocalBus for InProcessBus {
    fn publish(&mut self, message: Message) -> Result<(), BusError> {
        validate_message(&message)?;
        self.messages.push(message);
        Ok(())
    }

    fn messages_for_topic(&self, topic: &str) -> Vec<Message> {
        self.messages
            .iter()
            .filter(|message| message.topic == topic)
            .cloned()
            .collect()
    }

    fn replay_since(&self, offset: usize) -> Vec<Message> {
        self.messages.iter().skip(offset).cloned().collect()
    }

    fn len(&self) -> usize {
        self.messages.len()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("unknown Orion topic: {0}")]
    UnknownTopic(String),
    #[error("message exceeded ttl_max and is considered stuck")]
    TtlExceeded,
}

pub fn validate_message(message: &Message) -> Result<(), BusError> {
    if !topics::ALL_TOPICS.contains(&message.topic.as_str()) {
        return Err(BusError::UnknownTopic(message.topic.clone()));
    }

    if message.ttl_cycles > message.ttl_max {
        return Err(BusError::TtlExceeded);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::orion::message::{Author, MessageKind, Payload, Priority};

    #[test]
    fn bus_replays_messages_after_offset() {
        let mut bus = InProcessBus::default();
        let session_id = Uuid::new_v4();
        let correlation_id = Uuid::new_v4();

        let first = crate::orion::message::Message::new(
            MessageKind::UserInput,
            Author::User,
            topics::USER_CHAT_INPUT,
            Priority::UserInput,
            session_id,
            correlation_id,
            None,
            Payload::UserInput {
                text: "hello".to_string(),
            },
        );
        let second = crate::orion::message::Message::new(
            MessageKind::UserOutput,
            Author::Ego,
            topics::USER_CHAT_OUTPUT,
            Priority::UserInput,
            session_id,
            correlation_id,
            Some(first.id),
            Payload::ChatOutput {
                text: "world".to_string(),
            },
        );

        bus.publish(first).unwrap();
        bus.publish(second.clone()).unwrap();

        assert_eq!(bus.replay_since(1), vec![second]);
    }

    #[test]
    fn bus_rejects_unknown_topics() {
        let message = crate::orion::message::Message::new(
            MessageKind::UserInput,
            Author::User,
            "unknown.topic",
            Priority::UserInput,
            Uuid::new_v4(),
            Uuid::new_v4(),
            None,
            Payload::UserInput {
                text: "hello".to_string(),
            },
        );

        assert!(matches!(
            validate_message(&message),
            Err(BusError::UnknownTopic(topic)) if topic == "unknown.topic"
        ));
    }
}
