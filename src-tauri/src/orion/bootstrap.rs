//! First-run configuration loader.
//!
//! When SAO ships a bundle to the user, it includes a `config.json` next to the installer.
//! On launch we look for that config in one of three places (in order):
//!
//!   1. `%APPDATA%\OrionII\config.json` — the canonical path (installer drops it here).
//!   2. `<exe-dir>\config.json` — co-located with the executable, useful for portable runs.
//!   3. Environment variables — back-compat with the dev flow (SAO_BASE_URL etc.).
//!
//! The loaded bootstrap contains the SAO base URL + entity token + chosen LLM defaults.
//! `OrionCore` consumes it to (a) adopt the SAO-assigned `agent_id` as its `orion_id`
//! when no local state exists yet, and (b) point the model router at SAO's LLM proxy.

use std::path::PathBuf;

use serde::Deserialize;
use uuid::Uuid;

use crate::orion::model::{ModelConfig, ModelProviderKind};
use crate::orion::sao::SaoClientConfig;

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
struct BundleConfig {
    sao_base_url: String,
    agent_id: Uuid,
    agent_token: String,
    default_provider: String,
    default_id_model: String,
    default_ego_model: String,
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
}

impl OrionBootstrap {
    pub fn load() -> Self {
        if let Some(bundle) = read_bundle_config() {
            tracing_log("loaded bundle config from disk");

            let sao = SaoClientConfig::from_bundle(
                bundle.sao_base_url.clone(),
                bundle.agent_token.clone(),
                bundle.agent_id,
            );
            let model = ModelConfig {
                provider: ModelProviderKind::SaoProxyWithFallback,
                ollama_base_url: "http://127.0.0.1:11434".to_string(),
                id_model: bundle.default_id_model.clone(),
                ego_model: bundle.default_ego_model.clone(),
                sao_provider: bundle.default_provider.clone(),
                ..ModelConfig::default()
            };
            return Self {
                sao: Some(sao),
                model,
                assigned_agent_id: Some(bundle.agent_id),
            };
        }

        // No bundle — fall back to env-based dev flow.
        Self {
            sao: SaoClientConfig::from_env(),
            model: ModelConfig::default(),
            assigned_agent_id: None,
        }
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
