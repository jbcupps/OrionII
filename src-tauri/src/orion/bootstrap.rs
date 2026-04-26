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
}

#[derive(Debug, Clone)]
pub struct OrionBootstrap {
    pub sao: Option<SaoClientConfig>,
    pub model: ModelConfig,
    /// SAO-assigned identity for this entity. None when running in offline/dev mode.
    pub assigned_agent_id: Option<Uuid>,
    /// Live birth payload from SAO, populated when `/api/orion/birth` succeeds.
    pub birth: Option<BirthResponse>,
}

impl OrionBootstrap {
    pub fn load() -> Self {
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
            Self {
                sao: Some(sao),
                model,
                assigned_agent_id: bundle.agent_id,
                birth: None,
            }
        } else {
            // No bundle — fall back to env-based dev flow.
            Self {
                sao: SaoClientConfig::from_env(),
                model: ModelConfig::default(),
                assigned_agent_id: None,
                birth: None,
            }
        };

        // Stage 2: live birth call. Best-effort; offline mode keeps the anchor defaults.
        if let Some(sao) = &anchor.sao {
            match birth::fetch_birth(sao) {
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
                }
                Err(e) => {
                    tracing_log(&format!(
                        "birth call failed; running with bundle defaults: {e}"
                    ));
                }
            }
        }

        anchor
    }
}

impl Default for OrionBootstrap {
    fn default() -> Self {
        Self::load()
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
