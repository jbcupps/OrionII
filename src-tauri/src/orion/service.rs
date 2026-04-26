use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::orion::bootstrap::OrionBootstrap;
use crate::orion::bus::{BusError, InProcessBus, LocalBus};
use crate::orion::curator::CuratorRuntime;
use crate::orion::ego::EgoRuntime;
use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
use crate::orion::model::{ModelCallStatus, ModelRouter};
#[cfg(test)]
use crate::orion::model::{ModelConfig, ModelProviderKind};
use crate::orion::persistence::{FilePersistence, Persistence, PersistenceError};
use crate::orion::sao::{self, SaoClientConfig, SaoClientError, SaoEgressRecord, SaoEvent, SaoShipper};
use crate::orion::security::{ConstitutionalVerifier, SecurityHealth};
use crate::orion::skills::{DocumentSkill, OAuthSkillCatalog, OutlookSkill, SkillAuthorization};
use crate::orion::topics;

#[derive(Debug, Error)]
pub enum OrionError {
    #[error("message text cannot be empty")]
    EmptyMessage,
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    SaoClient(#[from] SaoClientError),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatExchange {
    pub input: Message,
    pub id_signal: Message,
    pub instruction: Message,
    pub output: Message,
    pub persisted_messages: usize,
    pub companion_id: Uuid,
    pub sao_backlog: usize,
    pub policy_version: u64,
    pub memory_count: usize,
    pub security: SecurityHealth,
    pub model_status: Vec<ModelCallStatus>,
}

pub struct OrionCore {
    bus: InProcessBus,
    persistence: FilePersistence,
    curator: CuratorRuntime,
    ego: EgoRuntime,
    model: ModelRouter,
    documents: DocumentSkill,
    outlook: OutlookSkill,
    oauth_catalog: OAuthSkillCatalog,
    verifier: ConstitutionalVerifier,
    sao_config: Option<SaoClientConfig>,
}

impl Default for OrionCore {
    fn default() -> Self {
        Self::from_bootstrap(OrionBootstrap::load())
    }
}

impl OrionCore {
    pub fn from_bootstrap(bootstrap: OrionBootstrap) -> Self {
        let mut oauth_catalog = OAuthSkillCatalog::default();
        oauth_catalog.register(SkillAuthorization::oauth(
            "external-documents",
            vec!["documents.read".to_string()],
        ));

        let persistence = match bootstrap.assigned_agent_id {
            Some(agent_id) => {
                let dir = std::env::current_dir()
                    .unwrap_or_else(|_| std::env::temp_dir())
                    .join(".orionii");
                FilePersistence::open_with_identity(dir, Some(agent_id))
                    .unwrap_or_else(|_| FilePersistence::default())
            }
            None => FilePersistence::default(),
        };

        let model = match (&bootstrap.sao, bootstrap.model.provider.clone()) {
            (Some(sao), crate::orion::model::ModelProviderKind::SaoProxyWithFallback) => {
                ModelRouter::with_sao_proxy(bootstrap.model.clone(), sao.clone())
            }
            _ => ModelRouter::new(bootstrap.model.clone()),
        };

        Self {
            bus: InProcessBus::default(),
            persistence,
            curator: CuratorRuntime::default(),
            ego: EgoRuntime,
            model,
            documents: DocumentSkill,
            outlook: OutlookSkill,
            oauth_catalog,
            verifier: ConstitutionalVerifier,
            sao_config: bootstrap.sao,
        }
    }

    #[cfg(test)]
    pub fn with_persistence(persistence: FilePersistence) -> Self {
        Self {
            bus: InProcessBus::default(),
            persistence,
            curator: CuratorRuntime::default(),
            ego: EgoRuntime,
            model: ModelRouter::new(ModelConfig {
                provider: ModelProviderKind::Deterministic,
                ..ModelConfig::default()
            }),
            documents: DocumentSkill,
            outlook: OutlookSkill,
            oauth_catalog: OAuthSkillCatalog::default(),
            verifier: ConstitutionalVerifier,
            sao_config: None,
        }
    }

    pub fn send_chat_message(&mut self, text: String) -> Result<ChatExchange, OrionError> {
        let text = text.trim();

        if text.is_empty() {
            return Err(OrionError::EmptyMessage);
        }

        self.model.clear_statuses();

        let session_id = Uuid::new_v4();
        let correlation_id = Uuid::new_v4();
        let input = Message::new(
            MessageKind::UserInput,
            Author::User,
            topics::USER_CHAT_INPUT,
            Priority::UserInput,
            session_id,
            correlation_id,
            None,
            Payload::UserInput {
                text: text.to_string(),
            },
        );

        self.publish_and_record(input.clone())?;

        let document_context = self
            .curator
            .retrieve_documents(text, self.persistence.document_chunks());
        let curated = self.curator.curate(
            &input,
            self.persistence.identity(),
            &document_context,
            &self.model,
        );
        let id_signal = Message::new(
            MessageKind::IdSignal,
            Author::Id,
            topics::EGO_INSTRUCTIONS,
            Priority::UserInput,
            input.session_id,
            input.correlation_id,
            Some(input.id),
            Payload::IdSignal {
                identity_version: curated.id_signal.identity_version,
                personality_signal: curated.id_signal.personality_signal.clone(),
                drives: curated.id_signal.drives.clone(),
            },
        );
        self.publish_and_record(id_signal.clone())?;

        let instruction = curated.instruction;
        self.publish_and_record(instruction.clone())?;

        if text.to_lowercase().contains("outlook") {
            let authorization = self.outlook.authorization();
            let task = self.outlook.search_task(
                input.session_id,
                input.correlation_id,
                instruction.id,
                text,
            );
            self.publish_and_record(task)?;
            let audit = sao::audit_message(
                input.session_id,
                input.correlation_id,
                instruction.id,
                format!(
                    "authorized {} with scopes {}",
                    authorization.skill_name,
                    authorization.scopes.join(",")
                ),
            );
            self.publish_and_record(audit)?;
        }

        if self
            .oauth_catalog
            .scopes_for("external-documents")
            .is_some()
        {
            let audit = sao::audit_message(
                input.session_id,
                input.correlation_id,
                instruction.id,
                "external-documents OAuth scope available: documents.read",
            );
            self.publish_and_record(audit)?;
        }

        let output = self.ego.respond(&instruction, &curated.ethics, &self.model);
        self.publish_and_record(output.clone())?;
        debug_assert!(topics::ALL_TOPICS.contains(&output.topic.as_str()));
        let _output_topic_messages = self.bus.messages_for_topic(topics::USER_CHAT_OUTPUT);
        let _replayed_messages = self.bus.replay_since(0);

        let audit = sao::audit_message(
            output.session_id,
            output.correlation_id,
            output.id,
            format!(
                "ego responded to user chat with {} local messages",
                self.bus.len()
            ),
        );
        self.publish_and_record(audit)?;

        self.persistence.enqueue_sao(SaoEgressRecord::pending(
            SaoEvent::IdentitySync {
                orion_id: self.persistence.identity().identity.orion_id,
                version: self.persistence.identity().version,
            }
            .sanitized(),
        ))?;
        let security = self.verifier.verify("local constitutional scaffold", None);

        Ok(ChatExchange {
            input,
            id_signal,
            instruction,
            output,
            persisted_messages: self.persistence.message_count(),
            companion_id: self.persistence.identity().identity.orion_id,
            sao_backlog: self.persistence.sao_backlog_len(),
            policy_version: self.persistence.policy().version,
            memory_count: self.persistence.memories().len(),
            security,
            model_status: self.model.statuses(),
        })
    }

    pub fn index_document(
        &mut self,
        source_path: String,
        contents: String,
    ) -> Result<usize, OrionError> {
        let chunks = self.documents.chunk_document(source_path, &contents);
        let count = chunks.len();
        self.persistence.add_document_chunks(chunks)?;
        Ok(count)
    }

    pub fn apply_sao_policy_refresh(&mut self, rules: Vec<String>) -> Result<u64, OrionError> {
        let shipper = SaoShipper::with_config(self.sao_config.clone());
        let policy = match shipper.fetch_policy() {
            Ok(policy) => policy,
            Err(SaoClientError::NotConfigured) => {
                let current = self.persistence.policy();
                crate::orion::sao::PolicyOverlay {
                    version: current.version + 1,
                    source: "local-fallback".to_string(),
                    rules,
                    updated_at: chrono::Utc::now(),
                }
            }
            Err(error) => return Err(error.into()),
        };
        self.persistence.apply_sao_refresh(Vec::new(), policy)?;
        Ok(self.persistence.policy().version)
    }

    pub fn ship_sao_egress(&mut self) -> Result<crate::orion::sao::ShipReport, OrionError> {
        Ok(self.persistence.ship_sao_egress(self.sao_config.as_ref())?)
    }

    pub fn sao_config(&self) -> Option<&SaoClientConfig> {
        self.sao_config.as_ref()
    }

    fn publish_and_record(&mut self, message: Message) -> Result<(), OrionError> {
        self.bus.publish(message.clone())?;

        if let Payload::AuditEvent { action, .. } = &message.payload {
            self.persistence.enqueue_sao(SaoEgressRecord::pending(
                SaoEvent::AuditAction {
                    action: action.clone(),
                    correlation_id: message.correlation_id,
                }
                .sanitized(),
            ))?;
        }

        self.persistence.record_message(&message)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn chat_preserves_causality_and_enqueues_sao() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let mut core = OrionCore::with_persistence(persistence);

        let exchange = core
            .send_chat_message("Help me plan the day".to_string())
            .unwrap();

        assert_eq!(
            exchange.input.correlation_id,
            exchange.id_signal.correlation_id
        );
        assert_eq!(
            exchange.input.correlation_id,
            exchange.instruction.correlation_id
        );
        assert_eq!(
            exchange.input.correlation_id,
            exchange.output.correlation_id
        );
        assert!(exchange.persisted_messages >= 5);
        assert!(exchange.sao_backlog >= 2);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn indexed_documents_are_used_as_local_context() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let mut core = OrionCore::with_persistence(persistence);

        core.index_document(
            "worker-notes.md".to_string(),
            "The quarterly review packet is stored in the blue folder.".to_string(),
        )
        .unwrap();
        let exchange = core
            .send_chat_message("Where is the quarterly review packet?".to_string())
            .unwrap();

        match exchange.instruction.payload {
            Payload::CuratedPrompt {
                context_summary, ..
            } => assert!(context_summary.contains("blue folder")),
            _ => panic!("expected curated prompt"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
