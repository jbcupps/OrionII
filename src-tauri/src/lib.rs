use std::path::PathBuf;

use tauri::Manager;
use tokio::sync::Mutex;

mod orion;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoConnectionStatus {
    configured: bool,
    base_url: Option<String>,
    agent_id: Option<String>,
    birthed: bool,
    agent_name: Option<String>,
    owner_username: Option<String>,
    provider: Option<String>,
    id_model: Option<String>,
    ego_model: Option<String>,
    birthed_at: Option<String>,
    policy_version: Option<u64>,
}

/// Tauri command bodies are intentionally thin adapters: each one publishes
/// to the bus and returns immediately. The real work happens in subscriber
/// tasks owned by `OrionCore`. See ADR-001 and AGENTS.md before adding a
/// new command.
///
/// Phase 2a: commands are `async fn` because `EventBus::publish` is async
/// and the OrionCore Mutex is now `tokio::sync::Mutex` (Send across .await,
/// unlike `std::sync::Mutex`). The lock is held for the duration of each
/// command body; Phase 2b will likely introduce a `Handle`-style accessor
/// so commands can clone the shared `Arc` references and release the lock
/// before any network I/O hits the Iggy bus.

#[tauri::command]
async fn send_chat_message(
    text: String,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::ChatExchange, String> {
    let core = state.lock().await;
    core.send_chat_message(text)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn index_document(
    source_path: String,
    contents: String,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<usize, String> {
    let core = state.lock().await;
    core.index_document(source_path, contents)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn refresh_sao_policy(
    rules: Vec<String>,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<u64, String> {
    let core = state.lock().await;
    core.apply_sao_policy_refresh(rules)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn ship_sao_egress(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::sao::ShipReport, String> {
    let core = state.lock().await;
    core.ship_sao_egress()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn companion_status(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::service::CompanionStatusReport, String> {
    let core = state.lock().await;
    Ok(core.companion_status())
}

/// Rotate the Iggy Personal Access Token in the local PAT store.
///
/// Phase 2b: re-runs the provisioning flow against the configured iggy
/// endpoint and overwrites `iggy_pat`. The actual server-side token
/// revoke + new-token mint is `TODO(phase-2b-pat-mint)` in
/// `iggy_auth::provision_first_run`. Until that lands, this command
/// simply rewrites the file with a freshly-generated stub PAT.
#[tauri::command]
async fn rotate_iggy_token(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<String, String> {
    let core = state.lock().await;
    let endpoint = match core.iggy_endpoint() {
        Some(e) => e,
        None => {
            return Err(
                "Iggy is not active on this entity (BusTransport is InMemory)".to_string(),
            )
        }
    };
    let path = orion::iggy_auth::pat_store_path()
        .map_err(|e| format!("PAT store path: {e}"))?;
    let creds = orion::iggy_auth::provision_first_run(&endpoint)
        .await
        .map_err(|e| format!("PAT provision: {e}"))?;
    orion::iggy_auth::save(&path, &creds).map_err(|e| format!("PAT save: {e}"))?;
    Ok(format!(
        "Rotated PAT written to {}; restart OrionII to apply",
        path.display()
    ))
}

#[tauri::command]
async fn sao_connection_status(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<SaoConnectionStatus, String> {
    let core = state.lock().await;
    let config = core.sao_config();
    let birth = core.birth();
    Ok(SaoConnectionStatus {
        configured: config.is_some(),
        base_url: config.map(|c| c.base_url.clone()),
        agent_id: birth
            .map(|b| b.agent.id.to_string())
            .or_else(|| config.and_then(|c| c.agent_id.map(|id| id.to_string()))),
        birthed: birth.is_some(),
        agent_name: birth.map(|b| b.agent.name.clone()),
        owner_username: birth.and_then(|b| b.owner.username.clone()),
        provider: birth.and_then(|b| b.agent.default_provider.clone()),
        id_model: birth.and_then(|b| b.agent.default_id_model.clone()),
        ego_model: birth.and_then(|b| b.agent.default_ego_model.clone()),
        birthed_at: birth.map(|b| b.birthed_at.to_rfc3339()),
        policy_version: birth.map(|b| b.policy.version),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyConfigResult {
    written_to: String,
    status: SaoConnectionStatus,
}

/// Validate a pasted bundle JSON, write it to %APPDATA%\OrionII\config.json (or the
/// portable fallback), then re-run bootstrap and hot-swap the OrionCore so the user
/// doesn't have to restart the app.
#[tauri::command]
async fn apply_bundle_config(
    json: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<ApplyConfigResult, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Pasted config is empty".to_string());
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("Pasted text is not valid JSON: {e}"))?;
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

    let new_core = orion::OrionCore::from_bootstrap_with_app_blocking(app.clone());
    {
        let mut slot = state.lock().await;
        *slot = new_core;
    }

    // Re-read connection status under a fresh lock acquisition.
    let status = {
        let core = state.lock().await;
        let config = core.sao_config();
        let birth = core.birth();
        SaoConnectionStatus {
            configured: config.is_some(),
            base_url: config.map(|c| c.base_url.clone()),
            agent_id: birth
                .map(|b| b.agent.id.to_string())
                .or_else(|| config.and_then(|c| c.agent_id.map(|id| id.to_string()))),
            birthed: birth.is_some(),
            agent_name: birth.map(|b| b.agent.name.clone()),
            owner_username: birth.and_then(|b| b.owner.username.clone()),
            provider: birth.and_then(|b| b.agent.default_provider.clone()),
            id_model: birth.and_then(|b| b.agent.default_id_model.clone()),
            ego_model: birth.and_then(|b| b.agent.default_ego_model.clone()),
            birthed_at: birth.map(|b| b.birthed_at.to_rfc3339()),
            policy_version: birth.map(|b| b.policy.version),
        }
    };
    Ok(ApplyConfigResult {
        written_to: target.display().to_string(),
        status,
    })
}

fn config_target_path() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("OrionII");
        p.push("config.json");
        return Some(p);
    }
    // Portable fallback: next to the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return Some(dir.join("config.json"));
        }
    }
    None
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let core = orion::OrionCore::from_bootstrap_with_app_blocking(handle);
            app.manage(Mutex::new(core));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send_chat_message,
            index_document,
            refresh_sao_policy,
            ship_sao_egress,
            companion_status,
            rotate_iggy_token,
            sao_connection_status,
            apply_bundle_config
        ])
        .run(tauri::generate_context!())
        .expect("error while running OrionII");
}
