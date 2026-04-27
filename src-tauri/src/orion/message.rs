use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: Uuid,
    pub correlation_id: Uuid,
    pub parent_msg_id: Option<Uuid>,
    pub kind: MessageKind,
    pub author: Author,
    pub topic: String,
    pub timestamp: DateTime<Utc>,
    pub ttl_cycles: u32,
    pub ttl_max: u32,
    pub priority: Priority,
    pub session_id: Uuid,
    pub payload: Payload,
}

impl Message {
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn new(
        kind: MessageKind,
        author: Author,
        topic: impl Into<String>,
        priority: Priority,
        session_id: Uuid,
        correlation_id: Uuid,
        parent_msg_id: Option<Uuid>,
        payload: Payload,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            correlation_id,
            parent_msg_id,
            kind,
            author,
            topic: topic.into(),
            timestamp: Utc::now(),
            ttl_cycles: 0,
            ttl_max: 12,
            priority,
            session_id,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageKind {
    UserInput,
    UserInterrupt,
    UserOutput,
    IdSignal,
    CuratedPrompt,
    EgoDirective,
    AgentAssigned,
    AgentProgress,
    AgentCompleted,
    AgentFailed,
    Checkpoint,
    AuditEvent,
    MemoryPromotion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Author {
    User,
    Curator,
    Id,
    Ego,
    Sao,
    Agent(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    UserInterrupt,
    UserInput,
    AgentResult,
    Housekeeping,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum Payload {
    UserInput {
        text: String,
    },
    CuratedPrompt {
        system_prompt: String,
        user_query: String,
        context_summary: String,
    },
    IdSignal {
        identity_version: u64,
        personality_signal: String,
        drives: Vec<String>,
    },
    ChatOutput {
        text: String,
    },
    AgentTask {
        description: String,
    },
    AuditEvent {
        action: String,
        sanitized: bool,
    },
    Status {
        text: String,
    },
}
