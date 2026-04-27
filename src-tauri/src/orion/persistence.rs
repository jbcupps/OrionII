use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::orion::identity::IdentityState;
use crate::orion::memory::{DocumentChunk, MemoryRecord};
use crate::orion::message::Message;
use crate::orion::sao::{PolicyOverlay, SaoClientConfig, SaoEgressRecord, SaoShipper, ShipReport};

pub trait Persistence {
    fn record_message(&mut self, message: &Message) -> Result<(), PersistenceError>;
    fn message_count(&self) -> usize;
    fn identity(&self) -> &IdentityState;
    fn document_chunks(&self) -> &[DocumentChunk];
    fn memories(&self) -> &[MemoryRecord];
    fn policy(&self) -> &PolicyOverlay;
    fn enqueue_sao(&mut self, record: SaoEgressRecord) -> Result<(), PersistenceError>;
    fn sao_backlog_len(&self) -> usize;
    fn add_document_chunks(&mut self, chunks: Vec<DocumentChunk>) -> Result<(), PersistenceError>;
    fn ship_sao_egress(
        &mut self,
        config: Option<&SaoClientConfig>,
    ) -> Result<ShipReport, PersistenceError>;
    fn apply_sao_refresh(
        &mut self,
        remote_memories: Vec<MemoryRecord>,
        remote_policy: PolicyOverlay,
    ) -> Result<(), PersistenceError>;
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("failed to read local Orion state: {0}")]
    Read(#[source] io::Error),
    #[error("failed to write local Orion state: {0}")]
    Write(#[source] io::Error),
    #[error("failed to decode local Orion state: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("failed to encode local Orion state: {0}")]
    Encode(#[source] serde_json::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalState {
    pub identity: IdentityState,
    pub messages: Vec<Message>,
    pub memories: Vec<MemoryRecord>,
    pub document_chunks: Vec<DocumentChunk>,
    pub sao_egress: Vec<SaoEgressRecord>,
    pub policy: PolicyOverlay,
}

impl Default for LocalState {
    fn default() -> Self {
        Self {
            identity: IdentityState::bootstrap(),
            messages: Vec::new(),
            memories: Vec::new(),
            document_chunks: Vec::new(),
            sao_egress: Vec::new(),
            policy: PolicyOverlay::default(),
        }
    }
}

pub struct FilePersistence {
    path: PathBuf,
    state: LocalState,
}

impl FilePersistence {
    pub fn open_default() -> Self {
        let base = std::env::current_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(".orionii");

        Self::open(base).unwrap_or_else(|_| Self {
            path: fallback_state_path(),
            state: LocalState::default(),
        })
    }

    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        Self::open_with_identity(data_dir, None)
    }

    /// Like `open`, but if no state file exists yet, adopt `assigned_orion_id` as the
    /// fresh identity's `orion_id`. If the file already exists with a different id, the
    /// persisted id wins (and we log a warning to flag the collision).
    pub fn open_with_identity(
        data_dir: impl AsRef<Path>,
        assigned_orion_id: Option<uuid::Uuid>,
    ) -> Result<Self, PersistenceError> {
        let path = data_dir.as_ref().join("orion_state.json");

        if !path.exists() {
            let mut state = LocalState::default();
            if let Some(id) = assigned_orion_id {
                state.identity.identity.orion_id = id;
            }
            let persistence = Self { path, state };
            persistence.flush()?;
            return Ok(persistence);
        }

        let contents = fs::read_to_string(&path).map_err(PersistenceError::Read)?;
        let mut state: LocalState =
            serde_json::from_str(&contents).map_err(PersistenceError::Decode)?;
        state.identity.identity.mark_recovered();

        if let Some(assigned) = assigned_orion_id {
            if state.identity.identity.orion_id != assigned {
                eprintln!(
                    "[OrionII bootstrap] WARNING: bundle agent_id {} does not match persisted orion_id {}; keeping persisted",
                    assigned, state.identity.identity.orion_id
                );
            }
        }

        Ok(Self { path, state })
    }

    fn flush(&self) -> Result<(), PersistenceError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(PersistenceError::Write)?;
        }

        let contents =
            serde_json::to_string_pretty(&self.state).map_err(PersistenceError::Encode)?;
        fs::write(&self.path, contents).map_err(PersistenceError::Write)
    }
}

impl Default for FilePersistence {
    fn default() -> Self {
        Self::open_default()
    }
}

impl Persistence for FilePersistence {
    fn record_message(&mut self, message: &Message) -> Result<(), PersistenceError> {
        self.state.messages.push(message.clone());

        if let Some(memory) = MemoryRecord::from_message(message) {
            self.state.memories.push(memory);
        }

        self.flush()
    }

    fn message_count(&self) -> usize {
        self.state.messages.len()
    }

    fn identity(&self) -> &IdentityState {
        &self.state.identity
    }

    fn document_chunks(&self) -> &[DocumentChunk] {
        &self.state.document_chunks
    }

    fn memories(&self) -> &[MemoryRecord] {
        &self.state.memories
    }

    fn policy(&self) -> &PolicyOverlay {
        &self.state.policy
    }

    fn enqueue_sao(&mut self, record: SaoEgressRecord) -> Result<(), PersistenceError> {
        self.state.sao_egress.push(record);
        self.flush()
    }

    fn sao_backlog_len(&self) -> usize {
        self.state
            .sao_egress
            .iter()
            .filter(|record| matches!(record.state, crate::orion::sao::SaoEgressState::Pending))
            .count()
    }

    fn add_document_chunks(&mut self, chunks: Vec<DocumentChunk>) -> Result<(), PersistenceError> {
        self.state.document_chunks.extend(chunks);
        self.flush()
    }

    fn ship_sao_egress(
        &mut self,
        config: Option<&SaoClientConfig>,
    ) -> Result<ShipReport, PersistenceError> {
        let shipper = match config {
            Some(c) => SaoShipper::with_config(Some(c.clone())),
            None => SaoShipper::default(),
        };
        let report = shipper.ship_pending(
            &mut self.state.sao_egress,
            self.state.identity.identity.orion_id,
        );
        self.state
            .sao_egress
            .retain(|record| !matches!(record.state, crate::orion::sao::SaoEgressState::Acked));
        self.flush()?;
        Ok(report)
    }

    fn apply_sao_refresh(
        &mut self,
        remote_memories: Vec<MemoryRecord>,
        remote_policy: PolicyOverlay,
    ) -> Result<(), PersistenceError> {
        let merged = crate::orion::sao::merge_sao_refresh(
            self.state.identity.clone(),
            self.state.memories.clone(),
            remote_memories,
            remote_policy,
        );
        self.state.identity = merged.identity;
        self.state.memories = merged.memories;
        self.state.policy = merged.policy;
        self.flush()
    }
}

fn fallback_state_path() -> PathBuf {
    std::env::temp_dir()
        .join("orionii")
        .join("orion_state_fallback.json")
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
    use crate::orion::topics;

    #[test]
    fn file_persistence_recovers_existing_identity() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let created = FilePersistence::open(&dir).unwrap();
        let identity_id = created.identity().identity.orion_id;
        drop(created);

        let recovered = FilePersistence::open(&dir).unwrap();

        assert_eq!(recovered.identity().identity.orion_id, identity_id);
        assert!(recovered.identity().identity.recovered_at.is_some());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn file_persistence_records_messages_and_memory() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let mut persistence = FilePersistence::open(&dir).unwrap();
        let message = Message::new(
            MessageKind::UserInput,
            Author::User,
            topics::MENTOR_INPUT,
            Priority::UserInput,
            Uuid::new_v4(),
            Uuid::new_v4(),
            None,
            Payload::UserInput {
                text: "remember this".to_string(),
            },
        );

        persistence.record_message(&message).unwrap();

        assert_eq!(persistence.message_count(), 1);
        assert_eq!(persistence.memories().len(), 1);

        let _ = fs::remove_dir_all(dir);
    }
}
