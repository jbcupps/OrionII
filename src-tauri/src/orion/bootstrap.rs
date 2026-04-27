//! First-run configuration loader.
//!
//! Two-stage flow:
//!
//! 1. **Anchor** — read `config.json` from `%APPDATA%\OrionII\config.json` or next to the
//!    executable, OR fall back to env vars in the dev path. Only `sao_base_url` +
//!    `agent_token` are strictly required (everything else is back-compat).
//! 2. **Birth** — call `GET /api/orion/birth` on SAO with the entity bearer to dynamically
//!    fetch the live agent metadata, default provider/model, scopes, current policy, and
//!    personality seed. This means changes made in SAO take effect on the next launch with
//!    no re-bundling. If birth fails (offline, revoked token, etc.) we fall back to the
//!    bundle defaults.

use std::path::PathBuf;

use serde::Deserialize;
use uuid::Uuid;

use crate::orion::birth::{self, BirthResponse};
use crate::orion::model::{ModelConfig, ModelProviderKind};
use crate::orion::sao::SaoClientConfig;

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
struct BundleConfig {
    sao_base_url: String,
    #[serde(default)]
    agent_id: Option<Uuid>,
    agent_token: String,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    default_id_model: Option<String>,
    #[serde(default)]
    default_ego_model: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    client_version_min: Option<String>,
    /// Which transport the entity bus runs on. SAO bundle configs default to
    /// the product durable bus; env-only dev mode still defaults to InMemory.
    #[serde(default = "default_bundle_bus_transport")]
    bus_transport: BusTransport,
}

/// Bus transport selection. The `EventBus` trait abstracts over every
/// backend, so callers don't change shape — only the concrete impl swaps.
/// See docs/ADR-003-nats-jetstream-transport.md.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BusTransport {
    /// Phase 1/2a: tokio broadcast channels in-process. No durability.
    /// Default for dev and tests.
    #[default]
    InMemory,
    /// Product durable path: a Tauri-bundled nats-server sidecar with
    /// JetStream enabled. Durable across restarts, file-backed, and
    /// packaged with the MSI by default.
    #[serde(rename = "nats_jetstream", alias = "nats_jet_stream")]
    NatsJetStream {
        /// NATS client port. Defaults to NATS' canonical 4222.
        #[serde(default = "default_nats_port")]
        port: u16,
    },
    /// Connect to an externally managed NATS JetStream node (testing,
    /// advanced deployments). OrionII still owns the entity-internal
    /// topics; this only swaps broker ownership.
    #[serde(rename = "external_nats_jetstream", alias = "external_nats_jet_stream")]
    ExternalNatsJetStream { endpoint: String },
    /// Phase 2b: a Tauri-bundled iggy-server sidecar managed by
    /// `iggy_supervisor`. Durable across restarts; Personal Access Token
    /// auto-provisioned on first run and stored in the per-user
    /// `iggy_pat` file (mode 600 / restricted ACL).
    BundledIggy {
        /// TCP port the sidecar listens on. Defaults to 8090 (Iggy's
        /// canonical TCP port). Override only if it collides locally.
        #[serde(default = "default_iggy_tcp_port")]
        port: u16,
    },
    /// Connect to an externally managed Iggy node (testing, advanced
    /// deployments). Phase 2 does not provision PATs over HTTP for this
    /// path — supply the token directly via `pat`.
    ExternalIggy { endpoint: String, pat: String },
}

fn default_iggy_tcp_port() -> u16 {
    8090
}

fn default_nats_port() -> u16 {
    4222
}

fn default_bundle_bus_transport() -> BusTransport {
    BusTransport::NatsJetStream {
        port: default_nats_port(),
    }
}

impl BusTransport {
    fn normalized_for_product_bundle(self) -> Self {
        match self {
            BusTransport::BundledIggy { .. }
                if std::env::var_os("ORIONII_ENABLE_LEGACY_IGGY").is_none() =>
            {
                tracing_log(
                    "legacy bundled_iggy transport found in config.json; using product default nats_jetstream",
                );
                default_bundle_bus_transport()
            }
            other => other,
        }
    }

    pub fn as_label(&self) -> &'static str {
        match self {
            BusTransport::InMemory => "in_memory",
            BusTransport::NatsJetStream { .. } => "nats_jetstream",
            BusTransport::ExternalNatsJetStream { .. } => "external_nats_jetstream",
            BusTransport::BundledIggy { .. } => "bundled_iggy",
            BusTransport::ExternalIggy { .. } => "external_iggy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrionBootstrap {
    pub sao: Option<SaoClientConfig>,
    pub model: ModelConfig,
    /// SAO-assigned identity for this entity. None when running in offline/dev mode.
    pub assigned_agent_id: Option<Uuid>,
    /// Live birth payload from SAO, populated when `/api/orion/birth` succeeds.
    pub birth: Option<BirthResponse>,
    /// Last birth failure, if the bundle anchor was present but SAO rejected
    /// or could not be reached. Exposed to the UI so users are guided toward a
    /// fresh SAO bundle instead of manual JSON paste.
    pub birth_error: Option<String>,
    /// Phase 2b: which transport the entity bus uses.
    pub bus_transport: BusTransport,
}

impl OrionBootstrap {
    /// Async load. Stage 1 (anchor) is sync disk I/O; stage 2 (birth) is the
    /// SAO HTTP call, which Phase 2a converted to async `reqwest`.
    pub async fn load() -> Self {
        let mut anchor = if let Some(bundle) = read_bundle_config() {
            tracing_log("loaded bundle config from disk");

            let sao = SaoClientConfig::from_bundle_anchor(
                bundle.sao_base_url.clone(),
                bundle.agent_token.clone(),
                bundle.agent_id,
            );
            let model = ModelConfig {
                provider: ModelProviderKind::SaoProxyWithFallback,
                ollama_base_url: "http://127.0.0.1:11434".to_string(),
                id_model: bundle
                    .default_id_model
                    .clone()
                    .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string()),
                ego_model: bundle
                    .default_ego_model
                    .clone()
                    .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string()),
                sao_provider: bundle
                    .default_provider
                    .clone()
                    .unwrap_or_else(|| "anthropic".to_string()),
                ..ModelConfig::default()
            };
            let bus_transport = bundle.bus_transport.clone().normalized_for_product_bundle();
            Self {
                sao: Some(sao),
                model,
                assigned_agent_id: bundle.agent_id,
                birth: None,
                birth_error: None,
                bus_transport,
            }
        } else {
            // No bundle — fall back to env-based dev flow.
            Self {
                sao: SaoClientConfig::from_env(),
                model: ModelConfig::default(),
                assigned_agent_id: None,
                birth: None,
                birth_error: None,
                bus_transport: BusTransport::default(),
            }
        };

        // Stage 2: live birth call. Best-effort; offline mode keeps the anchor defaults.
        if let Some(sao) = &anchor.sao {
            match birth::fetch_birth(sao).await {
                Ok(birth) => {
                    tracing_log(&format!(
                        "birthed as agent {} ({}) — live provider {} / id={} / ego={}",
                        birth.agent.id,
                        birth.agent.name,
                        birth.agent.default_provider.as_deref().unwrap_or("(none)"),
                        birth.agent.default_id_model.as_deref().unwrap_or("(none)"),
                        birth.agent.default_ego_model.as_deref().unwrap_or("(none)"),
                    ));
                    if let Some(p) = &birth.agent.default_provider {
                        anchor.model.sao_provider = p.clone();
                    }
                    if let Some(m) = &birth.agent.default_id_model {
                        anchor.model.id_model = m.clone();
                    }
                    if let Some(m) = &birth.agent.default_ego_model {
                        anchor.model.ego_model = m.clone();
                    }
                    anchor.assigned_agent_id = Some(birth.agent.id);
                    anchor.birth = Some(birth);
                    anchor.birth_error = None;
                }
                Err(e) => {
                    let message = e.to_string();
                    tracing_log(&format!(
                        "birth call failed; running with bundle defaults: {message}"
                    ));
                    anchor.birth_error = Some(message);
                }
            }
        }

        anchor
    }
}

impl Default for OrionBootstrap {
    fn default() -> Self {
        // Sync wrapper drives the async load via tauri's runtime. Prefer
        // `OrionBootstrap::load().await` from inside async code.
        tauri::async_runtime::block_on(Self::load())
    }
}

fn read_bundle_config() -> Option<BundleConfig> {
    for path in candidate_paths() {
        if !path.exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<BundleConfig>(&contents) {
                Ok(parsed) => {
                    tracing_log(&format!("config.json loaded from {}", path.display()));
                    return Some(parsed);
                }
                Err(e) => {
                    tracing_log(&format!(
                        "failed to parse config.json at {}: {e}",
                        path.display()
                    ));
                }
            },
            Err(e) => {
                tracing_log(&format!(
                    "failed to read config.json at {}: {e}",
                    path.display()
                ));
            }
        }
    }
    None
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("OrionII");
        p.push("config.json");
        paths.push(p);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("config.json"));
        }
    }

    paths
}

fn tracing_log(msg: &str) {
    eprintln!("[OrionII bootstrap] {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sao_bundle_nats_transport_kind() {
        let parsed: BusTransport = serde_json::from_value(json!({
            "kind": "nats_jetstream",
            "port": 4222,
        }))
        .unwrap();

        assert!(matches!(parsed, BusTransport::NatsJetStream { port: 4222 }));
    }

    #[test]
    fn parses_legacy_spelling_for_nats_transport_kind() {
        let parsed: BusTransport = serde_json::from_value(json!({
            "kind": "nats_jet_stream",
            "port": 4223,
        }))
        .unwrap();

        assert!(matches!(parsed, BusTransport::NatsJetStream { port: 4223 }));
    }

    #[test]
    fn bundle_config_defaults_to_nats_transport() {
        let parsed: BundleConfig = serde_json::from_value(json!({
            "sao_base_url": "http://localhost:3100",
            "agent_token": "token",
        }))
        .unwrap();

        assert!(matches!(
            parsed.bus_transport,
            BusTransport::NatsJetStream { port: 4222 }
        ));
    }

    #[test]
    fn legacy_bundled_iggy_bundle_normalizes_to_nats_transport() {
        let parsed: BusTransport = serde_json::from_value(json!({
            "kind": "bundled_iggy",
            "port": 8090,
        }))
        .unwrap();

        assert!(matches!(
            parsed.normalized_for_product_bundle(),
            BusTransport::NatsJetStream { port: 4222 }
        ));
    }
}
