use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::json;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use thiserror::Error;
use uuid::Uuid;

use crate::orion::birth::BirthResponse;
use crate::orion::bootstrap::{BusTransport, OrionBootstrap};
use crate::orion::bus::{
    current_soul_ref, BusError, Envelope, IggyBus, InMemoryBus, NatsJetStreamBus, RecvError,
    SharedBus, Topic,
};
use crate::orion::iggy_auth;
use crate::orion::iggy_supervisor::IggySupervisor;
#[cfg(test)]
use crate::orion::model::ModelConfig;
use crate::orion::model::{ModelCallStatus, ModelProviderKind, ModelRouter};
use crate::orion::nats_supervisor::NatsSupervisor;
use crate::orion::payloads::EgoActionPayload;
use crate::orion::persistence::{FilePersistence, Persistence, PersistenceError};
use crate::orion::sao::{SaoClientConfig, SaoClientError, SaoPolicyClient, ShipReport};
use crate::orion::security::{ConstitutionalVerifier, SecurityHealth};
use crate::orion::skills::{DocumentSkill, OAuthSkillCatalog, SkillAuthorization};
use crate::orion::{ego, egress, id, superego_local};

const UI_EGO_ACTION_EVENT: &str = "orion://ego/action";

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
/// `orion://ego/action` Tauri event.
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
    birth_error: Option<String>,
    bus_transport: BusTransport,
    /// Held to abort subscriber tasks when this `OrionCore` is dropped
    /// (e.g. on `apply_bundle_config` hot-swap). Subscribers that observe
    /// `RecvError::Closed` will exit cleanly on their own once the bus's
    /// final `Arc` reference is released, but explicit `abort()` here makes
    /// shutdown deterministic.
    handles: Vec<JoinHandle<()>>,
    /// Phase 2b: live iggy-server sidecar when running on the
    /// `BundledIggy` transport. Held so its `Drop` SIGKILLs the child on
    /// hot-swap or app shutdown.
    #[allow(dead_code)]
    iggy_supervisor: Option<IggySupervisor>,
    /// Product durable bus sidecar. Held so Drop terminates the local
    /// nats-server child on hot-swap or app shutdown.
    #[allow(dead_code)]
    nats_supervisor: Option<NatsSupervisor>,
}

impl Default for OrionCore {
    fn default() -> Self {
        // Sync default kept for legacy callers. Drives the async load + build
        // path via tauri's runtime.
        tauri::async_runtime::block_on(async {
            let bootstrap = OrionBootstrap::load().await;
            Self::build_async(bootstrap, None).await
        })
    }
}

impl OrionCore {
    /// Async constructor. Phase 2a: in-memory bus, no awaits actually fire.
    /// Phase 2b will add the Iggy connect path that does .await here.
    pub async fn build_async(bootstrap: OrionBootstrap, app: Option<AppHandle>) -> Self {
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

        // Bus selection. The `EventBus` trait abstracts every transport;
        // subscribers below don't change shape between them. Any failure
        // in a durable broker path falls back to the in-memory bus with a
        // diagnostic log — the entity stays alive even if the broker
        // doesn't.
        let (orion_id, supervisor_soul_ref) = {
            let p = persistence.lock().expect("persistence mutex poisoned");
            let identity = p.identity();
            (identity.identity.orion_id, current_soul_ref(identity))
        };

        let BusSelection {
            bus,
            iggy_supervisor,
            nats_supervisor,
        } = match select_bus(&bootstrap.bus_transport, orion_id, supervisor_soul_ref).await {
            Ok(selection) => selection,
            Err(error) => {
                eprintln!(
                    "[orion-core] durable bus path failed: {error}; falling back to in-memory"
                );
                BusSelection::in_memory()
            }
        };

        let mut handles = vec![
            id::spawn(bus.clone(), persistence.clone(), model.clone()),
            ego::spawn(bus.clone(), model.clone()),
            superego_local::spawn(bus.clone()),
            egress::spawn(bus.clone(), persistence.clone(), bootstrap.sao.clone()),
        ];
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
            birth_error: bootstrap.birth_error,
            bus_transport: bootstrap.bus_transport,
            handles,
            iggy_supervisor,
            nats_supervisor,
        }
    }

    /// Sync wrapper that loads bootstrap + builds in a single block_on.
    /// Prefer `build_async` from inside async code.
    #[allow(dead_code)]
    pub fn from_bootstrap_blocking() -> Self {
        tauri::async_runtime::block_on(async {
            let bootstrap = OrionBootstrap::load().await;
            Self::build_async(bootstrap, None).await
        })
    }

    pub fn from_bootstrap_with_app_blocking(app: AppHandle) -> Self {
        tauri::async_runtime::block_on(async {
            let bootstrap = OrionBootstrap::load().await;
            Self::build_async(bootstrap, Some(app)).await
        })
    }

    #[cfg(test)]
    pub fn with_persistence(persistence: FilePersistence) -> Self {
        let persistence = Arc::new(Mutex::new(persistence));
        let model = Arc::new(ModelRouter::new(ModelConfig {
            provider: ModelProviderKind::Deterministic,
            ..ModelConfig::default()
        }));
        let bus: SharedBus = InMemoryBus::new();

        let handles = vec![
            id::spawn(bus.clone(), persistence.clone(), model.clone()),
            ego::spawn(bus.clone(), model.clone()),
            superego_local::spawn(bus.clone()),
        ];

        Self {
            bus,
            persistence,
            model,
            documents: DocumentSkill,
            oauth_catalog: OAuthSkillCatalog::default(),
            verifier: ConstitutionalVerifier,
            sao_config: None,
            birth: None,
            birth_error: None,
            bus_transport: BusTransport::InMemory,
            handles,
            iggy_supervisor: None,
            nats_supervisor: None,
        }
    }

    pub fn birth(&self) -> Option<&BirthResponse> {
        self.birth.as_ref()
    }

    pub fn birth_error(&self) -> Option<&str> {
        self.birth_error.as_deref()
    }

    /// Publish a `MentorInput` envelope and return immediately. The Ego
    /// response arrives later as a Tauri event — see ADR-001 § "UI
    /// inversion".
    pub async fn send_chat_message(&self, text: String) -> Result<ChatExchange, OrionError> {
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
        self.bus.publish(env).await?;

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

    pub async fn apply_sao_policy_refresh(&self, rules: Vec<String>) -> Result<u64, OrionError> {
        let policy_client = SaoPolicyClient::with_config(self.sao_config.clone());
        let policy = match policy_client.fetch_policy().await {
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

    /// User-triggered ship of any pending SAO egress backlog. Same take /
    /// ship / merge dance as `egress::handle_outbound`, just exposed as a
    /// command so the UI can flush the queue manually.
    pub async fn ship_sao_egress(&self) -> Result<ShipReport, OrionError> {
        Ok(egress::ship_pending_backlog(&self.persistence, self.sao_config.clone()).await?)
    }

    pub fn sao_config(&self) -> Option<&SaoClientConfig> {
        self.sao_config.as_ref()
    }

    pub fn bus_transport_label(&self) -> &'static str {
        self.bus_transport.as_label()
    }

    /// Returns the iggy-server endpoint when running on the
    /// `BundledIggy` transport; `None` otherwise. Used by the
    /// `rotate_iggy_token` command.
    pub fn iggy_endpoint(&self) -> Option<String> {
        self.iggy_supervisor
            .as_ref()
            .map(|s| s.endpoint().to_string())
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

struct BusSelection {
    bus: SharedBus,
    iggy_supervisor: Option<IggySupervisor>,
    nats_supervisor: Option<NatsSupervisor>,
}

impl BusSelection {
    fn in_memory() -> Self {
        Self {
            bus: InMemoryBus::new(),
            iggy_supervisor: None,
            nats_supervisor: None,
        }
    }
}

/// Resolve which bus transport to construct. Returns the bus + any owned
/// sidecar supervisor handle. Errors here are non-fatal: the caller falls
/// back to in-memory and continues.
async fn select_bus(
    transport: &BusTransport,
    orion_id: Uuid,
    supervisor_soul_ref: String,
) -> Result<BusSelection, String> {
    match transport {
        BusTransport::InMemory => Ok(BusSelection::in_memory()),

        BusTransport::NatsJetStream { port } => {
            let supervisor_bus: SharedBus = InMemoryBus::new();
            let supervisor = NatsSupervisor::start(*port, supervisor_bus, supervisor_soul_ref)
                .await
                .map_err(|e| format!("nats supervisor start: {e}"))?;
            let nats_bus = NatsJetStreamBus::connect(supervisor.endpoint(), orion_id)
                .await
                .map_err(|e| format!("nats connect: {e}"))?;
            Ok(BusSelection {
                bus: nats_bus as SharedBus,
                iggy_supervisor: None,
                nats_supervisor: Some(supervisor),
            })
        }

        BusTransport::ExternalNatsJetStream { endpoint } => {
            let nats_bus = NatsJetStreamBus::connect(endpoint, orion_id)
                .await
                .map_err(|e| format!("external nats connect: {e}"))?;
            Ok(BusSelection {
                bus: nats_bus as SharedBus,
                iggy_supervisor: None,
                nats_supervisor: None,
            })
        }

        BusTransport::BundledIggy { port } => {
            // The supervisor needs a SharedBus to publish broker-unstable
            // governance into. We give it a temporary in-memory bus
            // initially, then construct the real IggyBus and let the
            // supervisor watcher continue using its temp handle. The
            // governance envelope is informational only at this point —
            // future Phase 2.1 will plumb the real bus through to the
            // supervisor after IggyBus is built.
            let supervisor_bus: SharedBus = InMemoryBus::new();
            let supervisor = IggySupervisor::start(*port, supervisor_bus, supervisor_soul_ref)
                .await
                .map_err(|e| format!("supervisor start: {e}"))?;
            let endpoint_no_scheme = supervisor
                .endpoint()
                .trim_start_matches("tcp://")
                .to_string();
            let creds = load_or_provision_creds(supervisor.endpoint()).await;
            let (user, pass) = parse_pat(&creds.pat);
            let iggy_bus = IggyBus::connect(&endpoint_no_scheme, &user, &pass, orion_id)
                .await
                .map_err(|e| format!("iggy connect: {e}"))?;
            Ok(BusSelection {
                bus: iggy_bus as SharedBus,
                iggy_supervisor: Some(supervisor),
                nats_supervisor: None,
            })
        }

        BusTransport::ExternalIggy { endpoint, pat } => {
            let endpoint_no_scheme = endpoint.trim_start_matches("tcp://").to_string();
            let (user, pass) = parse_pat(pat);
            let iggy_bus = IggyBus::connect(&endpoint_no_scheme, &user, &pass, orion_id)
                .await
                .map_err(|e| format!("external iggy connect: {e}"))?;
            Ok(BusSelection {
                bus: iggy_bus as SharedBus,
                iggy_supervisor: None,
                nats_supervisor: None,
            })
        }
    }
}

/// Phase 2b PAT format: `username:password` until the real PAT-mint flow
/// in `iggy_auth` lands. The split-at-`:` parser falls back to bootstrap
/// credentials if the format is unrecognised. See ADR-002 § "PAT auth".
fn parse_pat(pat: &str) -> (String, String) {
    if let Some((user, pass)) = pat.split_once(':') {
        (user.to_string(), pass.to_string())
    } else {
        // Treat the whole string as a token; iggy bootstrap admin
        // user/pass is the documented default.
        ("iggy".to_string(), "iggy".to_string())
    }
}

async fn load_or_provision_creds(endpoint: &str) -> iggy_auth::IggyCredentials {
    let path = match iggy_auth::pat_store_path() {
        Ok(p) => p,
        Err(error) => {
            eprintln!(
                "[orion-core] could not resolve PAT store path: {error}; using bootstrap creds"
            );
            return iggy_auth::IggyCredentials {
                endpoint: endpoint.to_string(),
                pat: "iggy:iggy".to_string(),
            };
        }
    };
    match iggy_auth::load(&path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            let provisioned = iggy_auth::provision_first_run(endpoint).await.ok();
            if let Some(c) = &provisioned {
                let _ = iggy_auth::save(&path, c);
            }
            provisioned.unwrap_or_else(|| iggy_auth::IggyCredentials {
                endpoint: endpoint.to_string(),
                pat: "iggy:iggy".to_string(),
            })
        }
        Err(error) => {
            eprintln!("[orion-core] PAT store load failed: {error}; using bootstrap creds");
            iggy_auth::IggyCredentials {
                endpoint: endpoint.to_string(),
                pat: "iggy:iggy".to_string(),
            }
        }
    }
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
                    if let Err(error) = app.emit(UI_EGO_ACTION_EVENT, &event) {
                        eprintln!("[ui-emitter] failed to emit Tauri event: {error}");
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    eprintln!("[ui-emitter] lagged on EgoAction, skipped {skipped}");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn ui_ego_action_event_name_uses_tauri_safe_chars() {
        assert!(UI_EGO_ACTION_EVENT
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '/' | ':' | '_')));
    }

    /// End-to-end smoke test: publishing a `MentorInput` results in an
    /// `EgoAction` arriving on the bus with a non-empty response text and
    /// a non-empty soul_ref. The deterministic model provider keeps this
    /// hermetic — no Ollama, no SAO required.
    ///
    /// Was `#[ignore]`d in Phase 1 because of `reqwest::blocking`'s nested
    /// runtime panicking on drop inside an async context. Phase 2a's async
    /// model layer removes that — there is no longer a nested runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mentor_input_round_trips_to_ego_action() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let core = OrionCore::with_persistence(persistence);

        let mut ego_rx = core.bus.subscribe(Topic::EgoAction);
        let mut egress_rx = core.bus.subscribe(Topic::EgressOutbound);

        let ack = core
            .send_chat_message("Help me plan the day".to_string())
            .await
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

        let outbound = timeout(Duration::from_secs(5), egress_rx.recv())
            .await
            .expect("EgressOutbound did not arrive within 5s")
            .expect("recv error");

        assert_eq!(outbound.topic, Topic::EgressOutbound);
        assert_eq!(outbound.correlation_id, Some(ack.correlation_id));
        assert_eq!(outbound.soul_ref, env.soul_ref);
        assert_eq!(outbound.payload["action"], Topic::EgoAction.as_str());
        assert_eq!(outbound.payload["sourceTopic"], Topic::EgoAction.as_str());

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
