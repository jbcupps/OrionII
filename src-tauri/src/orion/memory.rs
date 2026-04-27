use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orion::message::{Author, Message, Payload};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub id: Uuid,
    pub session_id: Uuid,
    pub author: Author,
    pub content: String,
    pub promoted: bool,
    pub updated_since_sync: bool,
    pub created_at: DateTime<Utc>,
}

impl MemoryRecord {
    #[allow(dead_code)]
    pub fn from_message(message: &Message) -> Option<Self> {
        let content = match &message.payload {
            Payload::UserInput { text } | Payload::ChatOutput { text } => text.clone(),
            Payload::CuratedPrompt { user_query, .. } => user_query.clone(),
            Payload::AgentTask { description } => description.clone(),
            Payload::Status { text } => text.clone(),
            Payload::IdSignal {
                personality_signal, ..
            } => personality_signal.clone(),
            Payload::AuditEvent { action, .. } => action.clone(),
        };

        if content.trim().is_empty() {
            return None;
        }

        Some(Self {
            id: Uuid::new_v4(),
            session_id: message.session_id,
            author: message.author.clone(),
            content,
            promoted: false,
            updated_since_sync: true,
            created_at: Utc::now(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentChunk {
    pub id: Uuid,
    pub source_path: String,
    pub chunk_idx: usize,
    pub text: String,
    pub scope: DocumentScope,
    pub indexed_at: DateTime<Utc>,
}

impl DocumentChunk {
    pub fn new(source_path: impl Into<String>, chunk_idx: usize, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_path: source_path.into(),
            chunk_idx,
            text: text.into(),
            scope: DocumentScope::User,
            indexed_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DocumentScope {
    User,
    Shared,
    Restricted,
}
