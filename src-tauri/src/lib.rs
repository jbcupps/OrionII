use std::sync::Mutex;

mod orion;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoConnectionStatus {
    configured: bool,
    base_url: Option<String>,
    agent_id: Option<String>,
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
            };
        }
    };
    let config = core.sao_config();
    SaoConnectionStatus {
        configured: config.is_some(),
        base_url: config.map(|c| c.base_url.clone()),
        agent_id: config.and_then(|c| c.agent_id.map(|id| id.to_string())),
    }
}

pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(orion::OrionCore::default()))
        .invoke_handler(tauri::generate_handler![
            send_chat_message,
            index_document,
            refresh_sao_policy,
            ship_sao_egress,
            sao_connection_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running OrionII");
}
