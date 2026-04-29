use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use thiserror::Error;
use tracing::{error, warn};
use uuid::Uuid;

use crate::orion::birth::BirthResponse;
use crate::orion::bootstrap::{AgentNameSource, BusTransport, OrionBootstrap};
use crate::orion::bus::{
    current_soul_ref, BusError, Envelope, IggyBus, InMemoryBus, NatsJetStreamBus, RecvError,
    SharedBus, Topic,
};
use crate::orion::charter::{self, Charter, SharedCharter};
use crate::orion::iggy_auth;
use crate::orion::iggy_supervisor::IggySupervisor;
use crate::orion::journal::{self, EgoClock};
use crate::orion::model::{ModelCallStatus, ModelConfig, ModelProviderKind, ModelRouter};
use crate::orion::nats_supervisor::NatsSupervisor;
use crate::orion::payloads::EgoActionPayload;
use crate::orion::persistence::{FilePersistence, Persistence, PersistenceError};
use crate::orion::sao::{SaoClientConfig, SaoClientError, SaoPolicyClient, ShipReport};
use crate::orion::security::{ConstitutionalVerifier, SecurityHealth};
use crate::orion::skills::{DocumentSkill, OAuthSkillCatalog, SkillAuthorization};
use crate::orion::{ego, egress, governance, id, superego_local};

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
    /// Last time the journal subscriber observed a `Topic::EgoAction`
    /// envelope. `None` until the first reply lands. Surfaces in the
    /// cockpit so the operator can tell at a glance whether the bus is
    /// actually delivering — a stale or missing value is the symptom of
    /// the model layer being wedged behind the spinner.
    pub last_ego_action_at: Option<DateTime<Utc>>,
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
    anchor_agent_name: Option<String>,
    anchor_agent_name_source: Option<AgentNameSource>,
    birth: Option<BirthResponse>,
    birth_error: Option<String>,
    birth_error_status: Option<u16>,
    bus_transport: BusTransport,
    /// Shared, hot-swappable charter cell. Read on every `Envelope::new`
    /// callsite to compute `soul_ref`; replaced by the `governance`
    /// subscriber on `charter.update`.
    charter: SharedCharter,
    /// Shared "last EgoAction observed" cell; populated by the journal
    /// subscriber, read by `companion_status`. See `journal::EgoClock`.
    ego_clock: EgoClock,
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

        let persistence = FilePersistence::open_default_with_identity(bootstrap.assigned_agent_id);
        let persistence = Arc::new(Mutex::new(persistence));

        let model = match (&bootstrap.sao, bootstrap.model.provider.clone()) {
            (Some(sao), ModelProviderKind::SaoProxyWithFallback) => Arc::new(
                ModelRouter::with_sao_proxy(bootstrap.model.clone(), sao.clone()),
            ),
            _ => Arc::new(ModelRouter::new(bootstrap.model.clone())),
        };

        // Charter loads from disk (or writes the placeholder on first
        // boot). Held in a SharedCharter so the `governance` subscriber
        // can hot-swap on `charter.update` without rebuilding `OrionCore`.
        let charter = charter::shared(Charter::load_or_init_placeholder());

        // Bus selection. The `EventBus` trait abstracts every transport;
        // subscribers below don't change shape between them. Any failure
        // in a durable broker path falls back to the in-memory bus with a
        // diagnostic log — the entity stays alive even if the broker
        // doesn't.
        let orion_id = {
            let p = persistence.lock().expect("persistence mutex poisoned");
            p.identity().identity.orion_id
        };
        let supervisor_soul_ref = {
            let c = charter.read().expect("charter rwlock poisoned");
            current_soul_ref(&c)
        };

        let BusSelection {
            bus,
            iggy_supervisor,
            nats_supervisor,
        } = match select_bus(&bootstrap.bus_transport, orion_id, supervisor_soul_ref).await {
            Ok(selection) => selection,
            Err(cause) => {
                error!(
                    target: "orion::core",
                    %cause,
                    "durable bus path failed; falling back to in-memory"
                );
                BusSelection::in_memory()
            }
        };

        let ego_clock = journal::new_ego_clock();

        let mut handles = vec![
            id::spawn(
                bus.clone(),
                persistence.clone(),
                model.clone(),
                charter.clone(),
            ),
            ego::spawn(bus.clone(), model.clone()),
            journal::spawn(bus.clone(), persistence.clone(), ego_clock.clone()),
            superego_local::spawn(bus.clone()),
            governance::spawn(bus.clone(), persistence.clone(), charter.clone()),
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
            anchor_agent_name: bootstrap.anchor_agent_name,
            anchor_agent_name_source: bootstrap.anchor_agent_name_source,
            birth: bootstrap.birth,
            birth_error: bootstrap.birth_error,
            birth_error_status: bootstrap.birth_error_status,
            bus_transport: bootstrap.bus_transport,
            charter,
            ego_clock,
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
        Self::with_persistence_and_model(
            persistence,
            Arc::new(ModelRouter::new(ModelConfig {
                provider: ModelProviderKind::Deterministic,
                ..ModelConfig::default()
            })),
        )
    }

    #[cfg(test)]
    pub fn with_persistence_and_model(
        persistence: FilePersistence,
        model: Arc<ModelRouter>,
    ) -> Self {
        let persistence = Arc::new(Mutex::new(persistence));
        let bus: SharedBus = InMemoryBus::new();
        let ego_clock = journal::new_ego_clock();
        // Tests use a placeholder charter at a unique tmp path so they
        // don't fight over `%APPDATA%\OrionII\charter.md` between
        // parallel `cargo test` runs.
        let charter_path = std::env::temp_dir()
            .join(format!("orionii-test-charter-{}", Uuid::new_v4()))
            .join("charter.md");
        let charter = charter::shared(Charter::load_or_init_placeholder_at(charter_path));

        let handles = vec![
            id::spawn(
                bus.clone(),
                persistence.clone(),
                model.clone(),
                charter.clone(),
            ),
            ego::spawn(bus.clone(), model.clone()),
            journal::spawn(bus.clone(), persistence.clone(), ego_clock.clone()),
            superego_local::spawn(bus.clone()),
            governance::spawn(bus.clone(), persistence.clone(), charter.clone()),
        ];

        Self {
            bus,
            persistence,
            model,
            documents: DocumentSkill,
            oauth_catalog: OAuthSkillCatalog::default(),
            verifier: ConstitutionalVerifier,
            sao_config: None,
            anchor_agent_name: None,
            anchor_agent_name_source: None,
            birth: None,
            birth_error: None,
            birth_error_status: None,
            bus_transport: BusTransport::InMemory,
            charter,
            ego_clock,
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

    pub fn birth_error_status(&self) -> Option<u16> {
        self.birth_error_status
    }

    pub fn configured_agent_name(&self) -> Option<&str> {
        self.birth
            .as_ref()
            .map(|birth| birth.agent.name.as_str())
            .or(self.anchor_agent_name.as_deref())
    }

    pub fn agent_name_source_label(&self) -> &'static str {
        if self.birth.is_some() {
            return "birth";
        }

        self.anchor_agent_name_source
            .as_ref()
            .map(AgentNameSource::as_str)
            .unwrap_or("none")
    }

    pub fn model_config(&self) -> &ModelConfig {
        self.model.config()
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
        let agent_id = {
            let p = self.persistence.lock().expect("persistence mutex poisoned");
            p.identity().identity.orion_id.to_string()
        };
        let soul_ref = {
            let c = self.charter.read().expect("charter rwlock poisoned");
            current_soul_ref(&c)
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

    /// Fetch the latest policy from SAO (or synthesise a local-fallback
    /// version) and publish it onto `Topic::GovernanceInbound`. The
    /// `governance` subscriber applies it to persistence; this command
    /// merely surfaces the new version number to the caller. Persistence
    /// apply is async with respect to this return — `companion_status` may
    /// briefly observe the previous version until the subscriber lands the
    /// envelope.
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

        let new_version = policy.version;
        let agent_id = {
            let p = self.persistence.lock().expect("persistence mutex poisoned");
            p.identity().identity.orion_id.to_string()
        };
        let soul_ref = {
            let c = self.charter.read().expect("charter rwlock poisoned");
            current_soul_ref(&c)
        };
        let payload = json!({
            "kind": governance::KIND_POLICY_REFRESH,
            "policy": policy,
        });
        let env = Envelope::new(
            Topic::GovernanceInbound,
            agent_id,
            soul_ref,
            Some(Uuid::new_v4()),
            payload,
        );
        self.bus.publish(env).await?;
        Ok(new_version)
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
            last_ego_action_at: journal::last_ego_action_at(&self.ego_clock),
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
        Err(cause) => {
            warn!(
                target: "orion::core",
                %cause,
                "could not resolve PAT store path; using bootstrap creds"
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
        Err(cause) => {
            warn!(
                target: "orion::core",
                %cause,
                "PAT store load failed; using bootstrap creds"
            );
            iggy_auth::IggyCredentials {
                endpoint: endpoint.to_string(),
                pat: "iggy:iggy".to_string(),
            }
        }
    }
}

/// Convert a `Topic::EgoAction` envelope into the camelCase Tauri event
/// payload the React frontend listens for. Returns `None` for envelopes
/// that don't deserialize as an `EgoActionPayload` so the spawn loop can
/// log + continue without panicking. Extracted from `spawn_ui_emitter`
/// purely so it can be unit-tested without a Tauri `AppHandle`.
fn build_ui_event(env: &Envelope) -> Option<UiEgoActionEvent> {
    let action = serde_json::from_value::<EgoActionPayload>(env.payload.clone()).ok()?;
    Some(UiEgoActionEvent {
        correlation_id: env.correlation_id,
        user_query: action.user_query,
        response_text: action.response_text,
    })
}

fn spawn_ui_emitter(bus: SharedBus, app: AppHandle) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::EgoAction);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let Some(event) = build_ui_event(&env) else {
                        warn!(
                            target: "orion::ui_emitter",
                            correlation_id = ?env.correlation_id,
                            "dropped malformed EgoAction payload"
                        );
                        continue;
                    };
                    if let Err(cause) = app.emit(UI_EGO_ACTION_EVENT, &event) {
                        error!(
                            target: "orion::ui_emitter",
                            correlation_id = ?env.correlation_id,
                            %cause,
                            "failed to emit Tauri event"
                        );
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(target: "orion::ui_emitter", skipped, "lagged on EgoAction");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::ego::EgoRuntime;
    use crate::orion::ethics::{EthicsOverlay, EthicsOverlaySource};
    use crate::orion::identity::IdentityState;
    use crate::orion::model::{ModelError, ModelPrompt, ModelProvider, ModelResult};
    use crate::orion::sao::PolicyOverlay;
    use async_trait::async_trait;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn ui_ego_action_event_name_uses_tauri_safe_chars() {
        assert!(UI_EGO_ACTION_EVENT
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '/' | ':' | '_')));
    }

    #[test]
    fn build_ui_event_round_trips_camel_case_payload() {
        let payload = serde_json::to_value(&EgoActionPayload {
            user_query: "hi".to_string(),
            response_text: "hello".to_string(),
        })
        .unwrap();
        // Sanity-check that the payload on the wire is camelCase, since the
        // React listener types these fields as `userQuery` / `responseText`.
        assert!(payload.get("userQuery").is_some());
        assert!(payload.get("responseText").is_some());

        let cid = Uuid::new_v4();
        let env = Envelope::new(Topic::EgoAction, "agent-1", "soul:v1", Some(cid), payload);
        let event = build_ui_event(&env).expect("camelCase payload must deserialize");

        assert_eq!(event.correlation_id, Some(cid));
        assert_eq!(event.user_query, "hi");
        assert_eq!(event.response_text, "hello");

        // The event re-serializes camelCase too (UiEgoActionEvent has
        // `rename_all = "camelCase"`); the React listener depends on this.
        let serialized = serde_json::to_value(&event).unwrap();
        assert!(serialized.get("correlationId").is_some());
        assert!(serialized.get("userQuery").is_some());
        assert!(serialized.get("responseText").is_some());
    }

    #[test]
    fn build_ui_event_drops_malformed_payload() {
        let env = Envelope::new(
            Topic::EgoAction,
            "agent-1",
            "soul:v1",
            None,
            serde_json::json!({"unrelated": "shape"}),
        );
        assert!(build_ui_event(&env).is_none());
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
        assert!(
            env.soul_ref.starts_with("blake3:"),
            "soul_ref must be the content-addressed charter hash, got {}",
            env.soul_ref
        );

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

        timeout(Duration::from_secs(5), async {
            loop {
                let status = core.companion_status();
                if status.persisted_messages >= 2
                    && status.memory_count >= 2
                    && status.last_ego_action_at.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("journal did not persist chat exchange within 5s");

        // The cockpit reads `last_ego_action_at` from the journal subscriber
        // — a missing or stale value is the symptom the chat watchdog is
        // designed to surface, so make sure it advances on a healthy
        // round-trip.
        let last = core.companion_status().last_ego_action_at.unwrap();
        assert!(last <= chrono::Utc::now());

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A `ModelProvider` whose calls never complete. Used to verify that
    /// the Ego subscriber's per-call timeout shields the bus from a wedged
    /// provider — without it, no `EgoAction` would ever publish and the
    /// chat surface would spin forever.
    struct HangingModel;

    #[async_trait]
    impl ModelProvider for HangingModel {
        async fn consult_id(
            &self,
            _identity: &IdentityState,
            _query: &str,
            _context: &str,
        ) -> ModelResult<String> {
            std::future::pending().await
        }
        async fn generate_ego_response(
            &self,
            _prompt: &ModelPrompt,
            _ethics: &EthicsOverlay,
        ) -> ModelResult<String> {
            std::future::pending().await
        }
    }

    /// `EgoRuntime::respond_text` must time out and return a degraded
    /// fallback rather than block the Ego subscriber forever. The Rust
    /// timeout is 30 s; we drive virtual time so the test runs instantly.
    #[tokio::test(start_paused = true)]
    async fn ego_falls_back_when_model_hangs() {
        let prompt = ModelPrompt {
            system_prompt: "test".into(),
            user_query: "hello".into(),
            context: String::new(),
        };
        let ethics = EthicsOverlay {
            deontological: 0.34,
            virtue: 0.33,
            consequential: 0.33,
            guidance: vec!["test".into()],
            source: EthicsOverlaySource::LocalScaffold,
        };

        let task = tokio::spawn(async move {
            EgoRuntime
                .respond_text(&prompt, &ethics, &HangingModel)
                .await
        });

        // Advance virtual time past the Ego timeout. With `start_paused`,
        // `tokio::time::sleep` advances the mock clock without real-time
        // delay, so the wrapped `tokio::time::timeout` fires and the
        // degraded fallback string is returned.
        tokio::time::sleep(Duration::from_secs(31)).await;

        let text = task.await.unwrap();
        assert!(text.contains("degraded local cognition"), "got: {text}");
        assert!(text.contains("timed out"), "got: {text}");
    }

    /// A `ModelProvider` that returns immediately, used to drive a fast
    /// integration round-trip through Id + Ego without depending on the
    /// deterministic provider's specific output text.
    struct FastModel;

    #[async_trait]
    impl ModelProvider for FastModel {
        async fn consult_id(
            &self,
            _identity: &IdentityState,
            query: &str,
            _context: &str,
        ) -> ModelResult<String> {
            Ok(format!("fast id signal for: {query}"))
        }
        async fn generate_ego_response(
            &self,
            prompt: &ModelPrompt,
            _ethics: &EthicsOverlay,
        ) -> ModelResult<String> {
            Ok(format!("fast ego reply to: {}", prompt.user_query))
        }
    }

    /// Sanity check that `FastModel` itself is wired through the trait;
    /// keeps the type alive even if a future test stops using it.
    #[tokio::test]
    async fn fast_model_returns_eagerly() {
        let prompt = ModelPrompt {
            system_prompt: "s".into(),
            user_query: "q".into(),
            context: String::new(),
        };
        let ethics = EthicsOverlay {
            deontological: 0.0,
            virtue: 0.0,
            consequential: 0.0,
            guidance: vec![],
            source: EthicsOverlaySource::LocalScaffold,
        };
        let text = FastModel
            .generate_ego_response(&prompt, &ethics)
            .await
            .unwrap();
        assert!(text.contains("fast ego reply"));
        let id = FastModel
            .consult_id(&IdentityState::bootstrap(), "hi", "")
            .await
            .unwrap();
        assert!(id.contains("fast id signal"));
    }

    /// A model error path keeps the trait surface honest; `ModelError` is
    /// the only return failure shape the Ego runtime knows how to render
    /// as a degraded reply.
    #[test]
    fn model_error_renders_to_string() {
        let err = ModelError::HttpFailure("503".into());
        assert!(err.to_string().contains("HTTP"));
    }

    /// The governance subscriber must apply a `charter.update` envelope by
    /// replacing the in-memory `SharedCharter` so subsequent envelopes
    /// carry the new `soul_ref` from line one. The on-disk write is
    /// validated implicitly via `Charter::replace`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn governance_subscriber_applies_charter_update() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let core = OrionCore::with_persistence(persistence);

        // Snapshot the placeholder soul_ref before any update lands.
        let before = {
            let c = core.charter.read().unwrap();
            current_soul_ref(&c)
        };

        let new_charter = "# Real charter\n\nDo the work the operator commissioned.\n";
        let env = Envelope::new(
            Topic::GovernanceInbound,
            "agent-1",
            "soul:placeholder",
            Some(Uuid::new_v4()),
            serde_json::json!({
                "kind": crate::orion::governance::KIND_CHARTER_UPDATE,
                "charter_text": new_charter,
                // No birth_certificate field on purpose: the cert path
                // touches APPDATA and isn't what this test is validating.
            }),
        );
        core.bus.publish(env).await.unwrap();

        timeout(Duration::from_secs(5), async {
            loop {
                let now = {
                    let c = core.charter.read().unwrap();
                    current_soul_ref(&c)
                };
                if now != before
                    && now == format!("blake3:{}", blake3::hash(new_charter.as_bytes()).to_hex())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("governance subscriber did not apply charter.update within 5s");

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The governance subscriber must apply a `policy.refresh` envelope to
    /// persistence, replacing the bus-bypass that previously lived in the
    /// `apply_sao_policy_refresh` command body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn governance_subscriber_applies_policy_refresh() {
        let dir = std::env::temp_dir().join(format!("orionii-test-{}", Uuid::new_v4()));
        let persistence = FilePersistence::open(&dir).unwrap();
        let core = OrionCore::with_persistence(persistence);
        let starting_version = core.companion_status().policy_version;

        let new_policy = PolicyOverlay {
            version: starting_version + 7,
            source: "test".into(),
            rules: vec!["governance subscriber test rule".into()],
            updated_at: chrono::Utc::now(),
        };
        let env = Envelope::new(
            Topic::GovernanceInbound,
            "agent-1",
            "soul:v1",
            Some(Uuid::new_v4()),
            serde_json::json!({
                "kind": crate::orion::governance::KIND_POLICY_REFRESH,
                "policy": new_policy,
            }),
        );
        core.bus.publish(env).await.unwrap();

        timeout(Duration::from_secs(5), async {
            loop {
                if core.companion_status().policy_version == starting_version + 7 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("governance subscriber did not apply policy.refresh within 5s");

        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }
}
