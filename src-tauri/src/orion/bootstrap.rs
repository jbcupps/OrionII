//! First-run configuration loader.
//!
//! Two-stage flow:
//!
//! 1. **Anchor** — read `config.json` from `%APPDATA%\OrionII\config.json` or next to the
//!    executable, OR fall back to env vars in the dev path. Only `sao_base_url` +
//!    `agent_token` are required; the bundle is purely a **credentials carrier** under the
//!    new commissioning flow. The legacy `agent_name`, `default_provider`,
//!    `default_id_model`, `default_ego_model`, and `bus_transport` fields are still parsed
//!    for back-compat (ignored bundles still load) but the source of truth for those
//!    values has moved to SAO's commissioning response — see
//!    `docs/sao-commissioning-contract.md`.
//! 2. **Birth** — call `GET /api/orion/birth` on SAO with the entity bearer to dynamically
//!    fetch the live agent metadata, default provider/model, scopes, current policy, and
//!    personality seed. This is the idempotent re-read path; the canonical write path is
//!    `POST /api/orion/commission/finalize` from the commissioning UI. If birth fails
//!    (offline, revoked token, etc.) the cockpit routes the operator to the appropriate
//!    Repair sub-mode rather than degrading silently.

use std::path::PathBuf;

use serde::Deserialize;
use uuid::Uuid;

use crate::orion::birth::{self, BirthResponse};
use crate::orion::model::{ModelConfig, ModelProviderKind};
use crate::orion::sao::SaoClientConfig;

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// On-disk shape of `config.json` shipped in a SAO agent bundle.
///
/// Only `sao_base_url` and `agent_token` are required. Every other field
/// is parsed best-effort for back-compat with bundles minted before the
/// commissioning flow — the source of truth for `default_provider`,
/// `default_id_model`, `default_ego_model`, and `agent_name` has moved to
/// SAO's commissioning response. `agent_id` is still useful as a
/// short-circuit for the Repair → Re-bind sub-mode (it tells the cockpit
/// which agent to re-fetch a charter for); past that, treat the bundle
/// as one-time credentials.
#[derive(Debug, Clone, Deserialize)]
struct BundleConfig {
    sao_base_url: String,
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    agent_name: Option<String>,
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
    /// Bus transport selection. Pre-commissioning bundles set this; under
    /// the commissioning flow SAO's defaults can override it via the
    /// finalize response. Bundles that omit it default to the product
    /// durable bus (NATS JetStream).
    #[serde(default = "default_bundle_bus_transport")]
    bus_transport: BusTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentNameSource {
    Bundle,
    TokenClaim,
}

impl AgentNameSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentNameSource::Bundle => "bundle",
            AgentNameSource::TokenClaim => "tokenClaim",
        }
    }
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
    /// Non-secret display name from the bundle or legacy entity-name token
    /// claim. This is useful in anchor-only mode but is not treated as a
    /// verified birth identity.
    pub anchor_agent_name: Option<String>,
    pub anchor_agent_name_source: Option<AgentNameSource>,
    /// Live birth payload from SAO, populated when `/api/orion/birth` succeeds.
    pub birth: Option<BirthResponse>,
    /// Last birth failure, if the bundle anchor was present but SAO rejected
    /// or could not be reached. Exposed to the UI so users are guided toward a
    /// fresh SAO bundle instead of manual JSON paste.
    pub birth_error: Option<String>,
    pub birth_error_status: Option<u16>,
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
            let (anchor_agent_name, anchor_agent_name_source) = resolve_anchor_agent_name(&bundle);
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
                anchor_agent_name,
                anchor_agent_name_source,
                birth: None,
                birth_error: None,
                birth_error_status: None,
                bus_transport,
            }
        } else {
            // No bundle — fall back to env-based dev flow.
            Self {
                sao: SaoClientConfig::from_env(),
                model: ModelConfig::default(),
                assigned_agent_id: None,
                anchor_agent_name: None,
                anchor_agent_name_source: None,
                birth: None,
                birth_error: None,
                birth_error_status: None,
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
                    anchor.birth_error_status = None;
                }
                Err(e) => {
                    let status = e.status_code();
                    let message = e.to_string();
                    tracing_log(&format!(
                        "birth call failed; running with bundle defaults: {message}"
                    ));
                    anchor.birth_error = Some(message);
                    anchor.birth_error_status = status;
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

pub fn config_target_path() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("OrionII");
        p.push("config.json");
        return Some(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return Some(dir.join("config.json"));
        }
    }
    None
}

pub fn write_bundle_config_json(json: &str) -> Result<PathBuf, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Pasted config is empty".to_string());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Pasted text is not valid JSON: {e}"))?;
    if !parsed.is_object() {
        return Err("Pasted JSON must be an object".to_string());
    }
    let obj = parsed.as_object().expect("checked above");
    for required in ["sao_base_url", "agent_token"] {
        let val = obj.get(required).and_then(|v| v.as_str()).unwrap_or("");
        if val.trim().is_empty() {
            return Err(format!("Required field `{required}` is missing or empty"));
        }
    }

    let target = config_target_path()
        .ok_or_else(|| "Could not determine target path for config.json".to_string())?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    let pretty = serde_json::to_string_pretty(&parsed)
        .map_err(|e| format!("Failed to re-serialize config: {e}"))?;
    std::fs::write(&target, pretty)
        .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;
    Ok(target)
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

fn resolve_anchor_agent_name(bundle: &BundleConfig) -> (Option<String>, Option<AgentNameSource>) {
    if let Some(name) = trimmed_non_empty(bundle.agent_name.as_deref()) {
        return (Some(name), Some(AgentNameSource::Bundle));
    }

    if let Some(name) = legacy_entity_name_claim(&bundle.agent_token) {
        return (Some(name), Some(AgentNameSource::TokenClaim));
    }

    (None, None)
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn legacy_entity_name_claim(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = decode_base64_url(payload)?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    trimmed_non_empty(value.get("entity_name").and_then(|v| v.as_str()))
}

fn decode_base64_url(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for ch in input.chars() {
        if ch == '=' {
            break;
        }
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '-' => 62,
            '_' => 63,
            _ => return None,
        };

        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
            if bits > 0 {
                buffer &= (1 << bits) - 1;
            } else {
                buffer = 0;
            }
        }
    }

    Some(output)
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
    fn bundle_agent_name_takes_precedence_over_token_claim() {
        let parsed: BundleConfig = serde_json::from_value(json!({
            "sao_base_url": "http://localhost:3100",
            "agent_name": "Bundle Abigail",
            "agent_token": "header.eyJlbnRpdHlfbmFtZSI6IlRva2VuIEFiaWdhaWwifQ.sig",
        }))
        .unwrap();

        assert_eq!(
            resolve_anchor_agent_name(&parsed),
            (
                Some("Bundle Abigail".to_string()),
                Some(AgentNameSource::Bundle)
            )
        );
    }

    #[test]
    fn token_entity_name_is_legacy_display_fallback() {
        let parsed: BundleConfig = serde_json::from_value(json!({
            "sao_base_url": "http://localhost:3100",
            "agent_token": "header.eyJlbnRpdHlfbmFtZSI6IlRva2VuIEFiaWdhaWwifQ.sig",
        }))
        .unwrap();

        assert_eq!(
            resolve_anchor_agent_name(&parsed),
            (
                Some("Token Abigail".to_string()),
                Some(AgentNameSource::TokenClaim)
            )
        );
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
