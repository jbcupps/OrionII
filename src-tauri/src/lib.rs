use std::path::PathBuf;
use std::sync::Mutex;

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

#[tauri::command]
fn send_chat_message(
    text: String,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::ChatExchange, String> {
    let mut core = state
        .lock()
        .map_err(|_| "Orion core state lock was poisoned".to_string())?;

    core.send_chat_message(text)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn index_document(
    source_path: String,
    contents: String,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<usize, String> {
    let mut core = state
        .lock()
        .map_err(|_| "Orion core state lock was poisoned".to_string())?;

    core.index_document(source_path, contents)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn refresh_sao_policy(
    rules: Vec<String>,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<u64, String> {
    let mut core = state
        .lock()
        .map_err(|_| "Orion core state lock was poisoned".to_string())?;

    core.apply_sao_policy_refresh(rules)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn ship_sao_egress(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::sao::ShipReport, String> {
    let mut core = state
        .lock()
        .map_err(|_| "Orion core state lock was poisoned".to_string())?;

    core.ship_sao_egress().map_err(|error| error.to_string())
}

#[tauri::command]
fn sao_connection_status(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> SaoConnectionStatus {
    let core = match state.lock() {
        Ok(c) => c,
        Err(_) => {
            return SaoConnectionStatus {
                configured: false,
                base_url: None,
                agent_id: None,
                birthed: false,
                agent_name: None,
                owner_username: None,
                provider: None,
                id_model: None,
                ego_model: None,
                birthed_at: None,
                policy_version: None,
            };
        }
    };
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
fn apply_bundle_config(
    json: String,
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

    let new_core = orion::OrionCore::from_bootstrap(orion::bootstrap::OrionBootstrap::load());
    let mut slot = state
        .lock()
        .map_err(|_| "Orion core state lock was poisoned".to_string())?;
    *slot = new_core;
    drop(slot);

    let status = sao_connection_status(state);
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
        .manage(Mutex::new(orion::OrionCore::default()))
        .invoke_handler(tauri::generate_handler![
            send_chat_message,
            index_document,
            refresh_sao_policy,
            ship_sao_egress,
            sao_connection_status,
            apply_bundle_config
        ])
        .run(tauri::generate_context!())
        .expect("error while running OrionII");
}
