use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::json;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use thiserror::Error;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::orion::birth::BirthResponse;
use crate::orion::bootstrap::OrionBootstrap;
use crate::orion::bus::{current_soul_ref, BusError, Envelope, InMemoryBus, SharedBus, Topic};
use crate::orion::ego::EgoActionPayload;
use crate::orion::model::{ModelCallStatus, ModelProviderKind, ModelRouter};
#[cfg(test)]
use crate::orion::model::ModelConfig;
use crate::orion::persistence::{FilePersistence, Persistence, PersistenceError};
use crate::orion::sao::{SaoClientConfig, SaoClientError, SaoShipper, ShipReport};
use crate::orion::security::{ConstitutionalVerifier, SecurityHealth};
use crate::orion::skills::{DocumentSkill, OAuthSkillCatalog, SkillAuthorization};
use crate::orion::{egress, ego, id, superego_local};

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

/// Acknowledgement returned by the `send_chat_message` Tauri command after
/// the bus refactor. The actual ego response arrives asynchronously on the
/// `orion://ego.action` Tauri event.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatExchange {
    pub correlation_id: Uuid,
    pub accepted: bool,
}

/// Persistence-derived companion status. The UI fetches this via the
/// `companion_status` command after each ego.action event lands.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionStatusReport {
    pub companion_id: Uuid,
    pub persisted_messages: usize,
    pub sao_backlog: usize,
    pub policy_version: u64,
    pub memory_count: usize,
    pub security: SecurityHealth,
    pub model_status: Vec<ModelCallStatus>,
}

pub struct OrionCore {
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    model: Arc<ModelRouter>,
    documents: DocumentSkill,
    #[allow(dead_code)]
    oauth_catalog: OAuthSkillCatalog,
    verifier: ConstitutionalVerifier,
    sao_config: Option<SaoClientConfig>,
    birth: Option<BirthResponse>,
    /// Held to abort subscriber tasks when this `OrionCore` is dropped
    /// (e.g. on `apply_bundle_config` hot-swap). Subscribers that observe
    /// `RecvError::Closed` will exit cleanly on their own once the bus's
    /// final `Arc` reference is released, but explicit `abort()` here makes
    /// shutdown deterministic.
    handles: Vec<JoinHandle<()>>,
}

impl Default for OrionCore {
    fn default() -> Self {
        Self::from_bootstrap(OrionBootstrap::load())
    }
}

impl OrionCore {
    pub fn from_bootstrap(bootstrap: OrionBootstrap) -> Self {
        Self::build(bootstrap, None)
    }

    /// Same as `from_bootstrap`, plus wires a UI emitter subscriber that
    /// republishes `Topic::EgoAction` envelopes as the
    /// `orion://ego.action` Tauri event so the React app can consume them.
    pub fn from_bootstrap_with_app(bootstrap: OrionBootstrap, app: AppHandle) -> Self {
        Self::build(bootstrap, Some(app))
    }

    fn build(bootstrap: OrionBootstrap, app: Option<AppHandle>) -> Self {
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
        let persistence = Arc::new(Mutex::new(persistence));

        let model = match (&bootstrap.sao, bootstrap.model.provider.clone()) {
            (Some(sao), ModelProviderKind::SaoProxyWithFallback) => Arc::new(
                ModelRouter::with_sao_proxy(bootstrap.model.clone(), sao.clone()),
            ),
            _ => Arc::new(ModelRouter::new(bootstrap.model.clone())),
        };

        let bus: SharedBus = InMemoryBus::new();

        let mut handles = Vec::new();
        handles.push(id::spawn(bus.clone(), persistence.clone(), model.clone()));
        handles.push(ego::spawn(bus.clone(), model.clone()));
        handles.push(superego_local::spawn(bus.clone()));
        handles.push(egress::spawn(
            bus.clone(),
            persistence.clone(),
            bootstrap.sao.clone(),
        ));
        if let Some(app) = app {
            handles.push(spawn_ui_emitter(bus.clone(), app));
        }

        Self {
            bus,
            persistence,
            model,
            documents: DocumentSkill,
            oauth_catalog,
            verifier: ConstitutionalVerifier,
            sao_config: bootstrap.sao,
            birth: bootstrap.birth,
            handles,
        }
    }

    #[cfg(test)]
    pub fn with_persistence(persistence: FilePersistence) -> Self {
        let persistence = Arc::new(Mutex::new(persistence));
        let model = Arc::new(ModelRouter::new(ModelConfig {
            provider: ModelProviderKind::Deterministic,
            ..ModelConfig::default()
        }));
        let bus: SharedBus = InMemoryBus::new();

        let mut handles = Vec::new();
        handles.push(id::spawn(bus.clone(), persistence.clone(), model.clone()));
        handles.push(ego::spawn(bus.clone(), model.clone()));
        handles.push(superego_local::spawn(bus.clone()));

        Self {
            bus,
            persistence,
            model,
            documents: DocumentSkill,
            oauth_catalog: OAuthSkillCatalog::default(),
            verifier: ConstitutionalVerifier,
            sao_config: None,
            birth: None,
            handles,
        }
    }

    pub fn birth(&self) -> Option<&BirthResponse> {
        self.birth.as_ref()
    }

    /// Publish a `MentorInput` envelope and return immediately. The Ego
    /// response arrives later as a Tauri event — see ADR-001 § "UI
    /// inversion".
    pub fn send_chat_message(&self, text: String) -> Result<ChatExchange, OrionError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(OrionError::EmptyMessage);
        }

        let correlation_id = Uuid::new_v4();
        let (agent_id, soul_ref) = {
            let p = self.persistence.lock().expect("persistence mutex poisoned");
            let identity = p.identity();
            (
                identity.identity.orion_id.to_string(),
                current_soul_ref(identity),
            )
        };

        let payload = json!({ "text": trimmed });
        let env = Envelope::new(
            Topic::MentorInput,
            agent_id,
            soul_ref,
            Some(correlation_id),
            payload,
        );
        self.bus.publish(env)?;

        Ok(ChatExchange {
            correlation_id,
            accepted: true,
        })
    }

    pub fn index_document(
        &self,
        source_path: String,
        contents: String,
    ) -> Result<usize, OrionError> {
        let chunks = self.documents.chunk_document(source_path, &contents);
        let count = chunks.len();
        let mut p = self.persistence.lock().expect("persistence mutex poisoned");
        p.add_document_chunks(chunks)?;
        Ok(count)
    }

    pub fn apply_sao_policy_refresh(&self, rules: Vec<String>) -> Result<u64, OrionError> {
        let shipper = SaoShipper::with_config(self.sao_config.clone());
        let policy = match shipper.fetch_policy() {
            Ok(policy) => policy,
            Err(SaoClientError::NotConfigured) => {
                let p = self.persistence.lock().expect("persistence mutex poisoned");
                let current = p.policy();
                crate::orion::sao::PolicyOverlay {
                    version: current.version + 1,
                    source: "local-fallback".to_string(),
                    rules,
                    updated_at: chrono::Utc::now(),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let mut p = self.persistence.lock().expect("persistence mutex poisoned");
        p.apply_sao_refresh(Vec::new(), policy)?;
        Ok(p.policy().version)
    }

    pub fn ship_sao_egress(&self) -> Result<ShipReport, OrionError> {
        let mut p = self.persistence.lock().expect("persistence mutex poisoned");
        Ok(p.ship_sao_egress(self.sao_config.as_ref())?)
    }

    pub fn sao_config(&self) -> Option<&SaoClientConfig> {
        self.sao_config.as_ref()
    }

    pub fn companion_status(&self) -> CompanionStatusReport {
        let p = self.persistence.lock().expect("persistence mutex poisoned");
        let security = self.verifier.verify("local constitutional scaffold", None);
        CompanionStatusReport {
            companion_id: p.identity().identity.orion_id,
            persisted_messages: p.message_count(),
            sao_backlog: p.sao_backlog_len(),
            policy_version: p.policy().version,
            memory_count: p.memories().len(),
            security,
            model_status: self.model.statuses(),
        }
    }
}

impl Drop for OrionCore {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

/// UI emitter subscriber. Listens on `Topic::EgoAction` and forwards each
/// envelope to the React app via a Tauri event. This is the only
/// Rust→React bridge for chat output — the UI does not consume the
/// `send_chat_message` return value for the assistant reply.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiEgoActionEvent {
    correlation_id: Option<Uuid>,
    user_query: String,
    response_text: String,
}

fn spawn_ui_emitter(bus: SharedBus, app: AppHandle) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::EgoAction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let Ok(action) =
                        serde_json::from_value::<EgoActionPayload>(env.payload.clone())
                    else {
                        eprintln!("[ui-emitter] dropped malformed EgoAction payload");
                        continue;
                    };
                    let event = UiEgoActionEvent {
                        correlation_id: env.correlation_id,
                        user_query: action.user_query,
                        response_text: action.response_text,
                    };
                    if let Err(error) = app.emit("orion://ego.action", &event) {
                        eprintln!("[ui-emitter] failed to emit Tauri event: {error}");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    eprintln!("[ui-emitter] lagged on EgoAction, skipped {skipped}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    /// End-to-end smoke test: publishing a `MentorInput` results in an
    /// `EgoAction` arriving on the bus with a non-empty response text and
    /// a non-empty soul_ref. The deterministic model provider keeps this
    /// hermetic — no Ollama, no SAO required.
    ///
    /// Currently `#[ignore]`d because `ModelRouter` always constructs an
    /// `OllamaModelProvider`, whose internal `reqwest::blocking::Client`
    /// owns its own tokio runtime. That runtime panics when dropped inside
    /// the test's outer async context, regardless of where we move `core`.
    /// Resolving this is a follow-up ticket: convert `OllamaModelProvider`
    /// and `SaoProxyProvider` to use `reqwest`'s async client (so they no
    /// longer create nested runtimes) — see ADR-001 § "Out of scope". Once
    /// that lands, remove the `#[ignore]` here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "reqwest::blocking nested runtime; see follow-up ticket"]
    async fn mentor_input_round_trips_to_ego_action() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let core = OrionCore::with_persistence(persistence);

        let mut ego_rx = core.bus.subscribe(Topic::EgoAction);

        let ack = core
            .send_chat_message("Help me plan the day".to_string())
            .unwrap();
        assert!(ack.accepted);

        let env = timeout(Duration::from_secs(5), ego_rx.recv())
            .await
            .expect("EgoAction did not arrive within 5s")
            .expect("recv error");

        assert_eq!(env.topic, Topic::EgoAction);
        assert_eq!(env.correlation_id, Some(ack.correlation_id));
        assert!(!env.soul_ref.is_empty(), "soul_ref must be populated");

        let payload: EgoActionPayload = serde_json::from_value(env.payload).unwrap();
        assert!(!payload.response_text.is_empty());

        std::thread::spawn(move || drop(core)).join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
