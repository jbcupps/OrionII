use tauri::Manager;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

mod orion;

use orion::commissioning_client::CommissioningError;

/// Initialise the tracing subscriber once at process start. The default
/// filter is `info` for OrionII modules and `warn` for everything else;
/// override via the `ORIONII_LOG` env var (e.g. `ORIONII_LOG=debug,orionii_lib=trace`).
/// `try_init` is used so the function is safe to call multiple times in
/// tests where the global subscriber may already be installed.
fn init_tracing() {
    let filter = EnvFilter::try_from_env("ORIONII_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,orionii_lib=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoConnectionStatus {
    configured: bool,
    base_url: Option<String>,
    agent_id: Option<String>,
    birthed: bool,
    agent_name: Option<String>,
    agent_name_source: String,
    owner_username: Option<String>,
    provider: Option<String>,
    id_model: Option<String>,
    ego_model: Option<String>,
    birthed_at: Option<String>,
    policy_version: Option<u64>,
    birth_error: Option<String>,
    birth_status_code: Option<u16>,
    bus_transport: String,
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
            return Err("Iggy is not active on this entity (BusTransport is InMemory)".to_string())
        }
    };
    let path = orion::iggy_auth::pat_store_path().map_err(|e| format!("PAT store path: {e}"))?;
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
    Ok(build_sao_connection_status(&core))
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
///
/// Use the async `build_async` path directly. The previous version called
/// `from_bootstrap_with_app_blocking`, which wraps `block_on` — invoking
/// `block_on` from inside an async Tauri command worker can panic or stall
/// the runtime, leaving the new core's subscribers wedged and chat silent
/// after Apply.
#[tauri::command]
async fn apply_bundle_config(
    json: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<ApplyConfigResult, String> {
    let target = orion::bootstrap::write_bundle_config_json(&json)?;

    let bootstrap = orion::bootstrap::OrionBootstrap::load().await;
    let new_core = orion::OrionCore::build_async(bootstrap, Some(app.clone())).await;
    {
        let mut slot = state.lock().await;
        *slot = new_core;
    }

    // Re-read connection status under a fresh lock acquisition.
    let status = {
        let core = state.lock().await;
        build_sao_connection_status(&core)
    };
    Ok(ApplyConfigResult {
        written_to: target.display().to_string(),
        status,
    })
}

// =====================================================================
// Commissioning command surface (Slice 7).
//
// Keeps thin-adapter shape per AGENTS.md: each command body is a small
// translation layer between the cockpit and `orion::commissioning_client`
// / `orion::charter_template`. Hot-swap on finalize/repair reuses the
// same async `build_async` path that `apply_bundle_config` switched to
// after the deadlock fix.
// =====================================================================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleSummary {
    key: String,
    display_name: String,
    description: String,
    time_estimate_minutes: u32,
    slots: Vec<RoleSlotSummary>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleSlotSummary {
    key: String,
    label: String,
    kind: orion::charter_template::SlotKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[serde(tag = "stage")]
enum CommissioningStateView {
    /// No SAO bundle is configured. Operator must paste a bundle config
    /// or run the installer; the cockpit's existing offline UI handles
    /// this. Commissioning cannot start.
    NotConfigured,
    /// Bundle present, no charter / certificate on disk yet, no birth
    /// has succeeded. Default first-launch path.
    FirstLaunch,
    /// Bundle present, certificate and charter on disk and birth call
    /// succeeded. Commissioning is complete; cockpit shows the chat UI.
    Commissioned,
    /// Bundle present, certificate exists locally, but the latest birth
    /// call returned 401. Token rotation is needed.
    NeedsTokenRefresh { agent_name: Option<String> },
    /// Bundle present, no local certificate, but bundle's `agent_id`
    /// suggests an agent was previously commissioned. Re-bind to recover
    /// charter + certificate from SAO's archive.
    NeedsRebind { agent_id: String },
}

fn local_artifacts_present() -> bool {
    let charter = charter_path();
    let cert = birth_certificate_path();
    let charter_real = charter
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| !text.contains("This entity has not yet been commissioned"))
        .unwrap_or(false);
    let cert_present = cert.as_ref().map(|p| p.exists()).unwrap_or(false);
    charter_real && cert_present
}

fn is_token_rejected_status(status: Option<u16>) -> bool {
    matches!(status, Some(401 | 403))
}

fn classify_commissioning_state(
    configured: bool,
    birthed: bool,
    birth_error_status: Option<u16>,
    agent_name: Option<String>,
    agent_id: Option<String>,
    local_artifacts_present: bool,
) -> CommissioningStateView {
    if !configured {
        return CommissioningStateView::NotConfigured;
    }
    if birthed {
        return CommissioningStateView::Commissioned;
    }
    if is_token_rejected_status(birth_error_status) {
        return CommissioningStateView::NeedsTokenRefresh { agent_name };
    }
    if local_artifacts_present {
        if let Some(agent_id) = agent_id {
            return CommissioningStateView::NeedsRebind { agent_id };
        }
    }
    CommissioningStateView::FirstLaunch
}

fn charter_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        std::path::PathBuf::from(appdata)
            .join("OrionII")
            .join("charter.md")
    })
}

fn birth_certificate_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| {
        std::path::PathBuf::from(appdata)
            .join("OrionII")
            .join("birth_certificate.json")
    })
}

fn write_local_artifacts(charter_text: &str, cert: &serde_json::Value) -> Result<(), String> {
    let charter_p = charter_path()
        .ok_or_else(|| "Cannot resolve %APPDATA%\\OrionII\\charter.md".to_string())?;
    let cert_p = birth_certificate_path()
        .ok_or_else(|| "Cannot resolve %APPDATA%\\OrionII\\birth_certificate.json".to_string())?;
    if let Some(parent) = charter_p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create charter dir: {e}"))?;
    }
    std::fs::write(&charter_p, charter_text.as_bytes())
        .map_err(|e| format!("write charter.md: {e}"))?;
    let pretty =
        serde_json::to_string_pretty(cert).map_err(|e| format!("encode certificate: {e}"))?;
    std::fs::write(&cert_p, pretty).map_err(|e| format!("write birth_certificate.json: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn commissioning_state(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<CommissioningStateView, String> {
    let (configured, birthed, birth_error_status, agent_name, agent_id) = {
        let core = state.lock().await;
        (
            core.sao_config().is_some(),
            core.birth().is_some(),
            core.birth_error_status(),
            core.configured_agent_name().map(str::to_string),
            core.sao_config()
                .and_then(|c| c.agent_id.map(|id| id.to_string())),
        )
    };
    let local_present = configured && !birthed && local_artifacts_present();
    Ok(classify_commissioning_state(
        configured,
        birthed,
        birth_error_status,
        agent_name,
        agent_id,
        local_present,
    ))
}

#[tauri::command]
async fn list_commissioning_roles() -> Result<Vec<RoleSummary>, String> {
    orion::charter_template::load_all()
        .map(|roles| {
            roles
                .into_iter()
                .map(|r| RoleSummary {
                    key: r.key,
                    display_name: r.display_name,
                    description: r.description,
                    time_estimate_minutes: r.time_estimate_minutes,
                    slots: r
                        .slots
                        .into_iter()
                        .map(|s| RoleSlotSummary {
                            key: s.key,
                            label: s.label,
                            kind: s.kind,
                        })
                        .collect(),
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn render_charter_from_role(
    role_key: String,
    slot_values: std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let role = orion::charter_template::find(&role_key).map_err(|e| e.to_string())?;
    orion::charter_template::render(&role, &slot_values).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "code"
)]
enum CommissioningCommandError {
    TokenInvalid {
        message: String,
    },
    AlreadyCommissioned {
        agent_id: String,
        message: String,
    },
    AgentNotFound {
        agent_id: String,
        message: String,
    },
    UnsupportedVersion {
        expected: Vec<String>,
        message: String,
    },
    Message {
        message: String,
    },
}

impl From<CommissioningError> for CommissioningCommandError {
    fn from(error: CommissioningError) -> Self {
        match error {
            CommissioningError::TokenInvalid => Self::TokenInvalid {
                message: "token rejected by SAO; refresh credentials".to_string(),
            },
            CommissioningError::AlreadyCommissioned { agent_id } => Self::AlreadyCommissioned {
                agent_id: agent_id.to_string(),
                message: format!(
                    "agent is already commissioned ({agent_id}); use repair to re-bind"
                ),
            },
            CommissioningError::AgentNotFound { agent_id } => Self::AgentNotFound {
                agent_id: agent_id.to_string(),
                message: format!("agent {agent_id} no longer exists in SAO"),
            },
            CommissioningError::UnsupportedVersion { expected } => Self::UnsupportedVersion {
                message: format!("commissioning version unsupported by SAO; expected {expected:?}"),
                expected,
            },
            other => Self::Message {
                message: other.to_string(),
            },
        }
    }
}

#[tauri::command]
async fn commission_start(
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<orion::commissioning_client::StartResponse, CommissioningCommandError> {
    let sao = {
        let core = state.lock().await;
        core.sao_config().cloned()
    };
    let client = orion::commissioning_client::CommissioningClient::from_config(sao)
        .map_err(CommissioningCommandError::from)?;
    client
        .start()
        .await
        .map_err(CommissioningCommandError::from)
}

/// Q&A path v0: single-turn description-to-charter via the SAO LLM proxy.
/// The contract spec calls for a multi-turn dialog ending with
/// `[CHARTER_READY]`; this v0 ships a single-shot translator so the
/// commissioning surface is end-to-end functional. Multi-turn lands in a
/// follow-up; the wire endpoint and `role: "commissioning"` discriminator
/// are reused unchanged when it does.
#[tauri::command]
async fn commission_qna(
    description: String,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<String, String> {
    let sao = {
        let core = state.lock().await;
        core.sao_config().cloned()
    };
    let sao = sao.ok_or_else(|| "SAO is not configured".to_string())?;

    let body = serde_json::json!({
        "provider": "anthropic",
        "model": "claude-haiku-4-5-20251001",
        "role": "commissioning",
        "system": "You produce a Markdown business charter for an OrionII entity. Given the operator's free-form description, return a single Markdown document that names the role, states its purpose in 1-2 sentences, lists the systems and outputs in scope, and captures explicit boundaries (what the agent must never do without the operator's approval). Do not include any preamble — the response is the charter body itself.",
        "prompt": description,
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let response = client
        .post(format!("{}/api/llm/generate", sao.base_url))
        .bearer_auth(&sao.bearer_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("LLM proxy request: {e}"))?;

    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("LLM proxy response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "LLM proxy returned {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        ));
    }
    #[derive(serde::Deserialize)]
    struct LlmResponse {
        text: Option<String>,
        #[serde(default)]
        error: Option<String>,
    }
    let parsed: LlmResponse =
        serde_json::from_slice(&bytes).map_err(|e| format!("decode LLM response: {e}"))?;
    if let Some(err) = parsed.error {
        return Err(err);
    }
    parsed
        .text
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "LLM proxy returned empty text".to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CommissionFinalizeResult {
    soul_ref: String,
    charter_hash: String,
    status: SaoConnectionStatus,
}

#[tauri::command]
async fn commission_finalize(
    commission_id: uuid::Uuid,
    role_key: String,
    charter_text: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<CommissionFinalizeResult, String> {
    let charter_hash = blake3::hash(charter_text.as_bytes()).to_hex().to_string();
    let payload = orion::commissioning_client::FinalizePayload {
        commission_id,
        role_key,
        charter_text: charter_text.clone(),
        charter_hash: charter_hash.clone(),
    };

    let sao = {
        let core = state.lock().await;
        core.sao_config().cloned()
    };
    let client = orion::commissioning_client::CommissioningClient::from_config(sao)
        .map_err(|e| e.to_string())?;
    let response = client.finalize(payload).await.map_err(|e| e.to_string())?;

    let cert_value = serde_json::to_value(&response.birth_certificate)
        .map_err(|e| format!("encode certificate: {e}"))?;
    write_local_artifacts(&charter_text, &cert_value)?;

    // Hot-swap so the next bootstrap reads the new charter, calls
    // GET /api/orion/birth, and OrionCore.birth flips to Some.
    let bootstrap = orion::bootstrap::OrionBootstrap::load().await;
    let new_core = orion::OrionCore::build_async(bootstrap, Some(app.clone())).await;
    {
        let mut slot = state.lock().await;
        *slot = new_core;
    }

    let status = {
        let core = state.lock().await;
        build_sao_connection_status(&core)
    };
    Ok(CommissionFinalizeResult {
        soul_ref: response.soul_ref,
        charter_hash: response.charter_hash,
        status,
    })
}

#[derive(serde::Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
enum RepairKind {
    #[serde(rename = "rotate_token")]
    RotateToken { new_token: String },
    #[serde(rename = "rebind")]
    Rebind,
}

#[tauri::command]
async fn commission_repair(
    request: RepairKind,
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<orion::OrionCore>>,
) -> Result<CommissionFinalizeResult, String> {
    let sao = {
        let core = state.lock().await;
        core.sao_config().cloned()
    };
    let client = orion::commissioning_client::CommissioningClient::from_config(sao)
        .map_err(|e| e.to_string())?;

    let (commission_repair_request, rotated_token) = match request {
        RepairKind::RotateToken { new_token } => (
            orion::commissioning_client::RepairRequest::RotateToken {
                new_token: new_token.clone(),
            },
            Some(new_token),
        ),
        RepairKind::Rebind => (orion::commissioning_client::RepairRequest::Rebind, None),
    };

    // For rotate_token, hit the repair endpoint with the *new* token as
    // the bearer — the old one is what just 401'd.
    let client = match &rotated_token {
        Some(token) => client.with_token(token.clone()),
        None => client,
    };

    let response = client
        .repair(commission_repair_request)
        .await
        .map_err(|e| e.to_string())?;

    let cert_value = serde_json::to_value(&response.birth_certificate)
        .map_err(|e| format!("encode certificate: {e}"))?;
    write_local_artifacts(&response.charter_text, &cert_value)?;

    // Token rotation: rewrite config.json with the new bearer so the
    // next bootstrap picks it up. Re-bind reuses the existing token.
    if let Some(token) = rotated_token {
        rewrite_bundle_token(&token)?;
    }

    let bootstrap = orion::bootstrap::OrionBootstrap::load().await;
    let new_core = orion::OrionCore::build_async(bootstrap, Some(app.clone())).await;
    {
        let mut slot = state.lock().await;
        *slot = new_core;
    }

    let status = {
        let core = state.lock().await;
        build_sao_connection_status(&core)
    };
    Ok(CommissionFinalizeResult {
        soul_ref: response.soul_ref,
        charter_hash: response.charter_hash,
        status,
    })
}

fn rewrite_bundle_token(new_token: &str) -> Result<(), String> {
    let path = orion::bootstrap::config_target_path()
        .ok_or_else(|| "Cannot resolve config.json target path".to_string())?;
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert(
            "agent_token".to_string(),
            serde_json::Value::String(new_token.to_string()),
        );
    } else {
        return Err("config.json root is not an object".to_string());
    }
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("encode {}: {e}", path.display()))?;
    std::fs::write(&path, pretty).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn build_sao_connection_status(core: &orion::OrionCore) -> SaoConnectionStatus {
    let config = core.sao_config();
    let birth = core.birth();
    let model = core.model_config();

    SaoConnectionStatus {
        configured: config.is_some(),
        base_url: config.map(|c| c.base_url.clone()),
        agent_id: birth
            .map(|b| b.agent.id.to_string())
            .or_else(|| config.and_then(|c| c.agent_id.map(|id| id.to_string()))),
        birthed: birth.is_some(),
        agent_name: core.configured_agent_name().map(str::to_string),
        agent_name_source: core.agent_name_source_label().to_string(),
        owner_username: birth.and_then(|b| b.owner.username.clone()),
        provider: birth
            .and_then(|b| b.agent.default_provider.clone())
            .or_else(|| Some(model.sao_provider.clone())),
        id_model: birth
            .and_then(|b| b.agent.default_id_model.clone())
            .or_else(|| Some(model.id_model.clone())),
        ego_model: birth
            .and_then(|b| b.agent.default_ego_model.clone())
            .or_else(|| Some(model.ego_model.clone())),
        birthed_at: birth.map(|b| b.birthed_at.to_rfc3339()),
        policy_version: birth.map(|b| b.policy.version),
        birth_error: core.birth_error().map(str::to_string),
        birth_status_code: core.birth_error_status(),
        bus_transport: core.bus_transport_label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commissioning_state_serializes_camel_case_payloads() {
        let token_refresh = serde_json::to_value(CommissioningStateView::NeedsTokenRefresh {
            agent_name: Some("Agent Ada".to_string()),
        })
        .unwrap();
        assert_eq!(
            token_refresh,
            json!({
                "stage": "needsTokenRefresh",
                "agentName": "Agent Ada"
            })
        );

        let rebind = serde_json::to_value(CommissioningStateView::NeedsRebind {
            agent_id: "agent-123".to_string(),
        })
        .unwrap();
        assert_eq!(
            rebind,
            json!({
                "stage": "needsRebind",
                "agentId": "agent-123"
            })
        );
    }

    #[test]
    fn commissioning_state_deserializes_camel_case_payloads() {
        let parsed: CommissioningStateView = serde_json::from_value(json!({
            "stage": "needsTokenRefresh",
            "agentName": "Agent Ada"
        }))
        .unwrap();

        assert_eq!(
            parsed,
            CommissioningStateView::NeedsTokenRefresh {
                agent_name: Some("Agent Ada".to_string())
            }
        );
    }

    #[test]
    fn mocked_expired_birth_response_produces_needs_token_refresh() {
        for status in [401, 403] {
            let view = classify_commissioning_state(
                true,
                false,
                Some(status),
                Some("Agent Ada".to_string()),
                Some("agent-123".to_string()),
                false,
            );

            assert_eq!(
                view,
                CommissioningStateView::NeedsTokenRefresh {
                    agent_name: Some("Agent Ada".to_string())
                }
            );
        }
    }

    #[test]
    fn first_launch_anchor_routes_to_commissioning() {
        let view = classify_commissioning_state(
            true,
            false,
            Some(404),
            Some("Agent Ada".to_string()),
            Some("agent-123".to_string()),
            false,
        );

        assert_eq!(view, CommissioningStateView::FirstLaunch);
    }

    #[test]
    fn local_artifact_drift_routes_to_rebind() {
        let view = classify_commissioning_state(
            true,
            false,
            Some(500),
            Some("Agent Ada".to_string()),
            Some("agent-123".to_string()),
            true,
        );

        assert_eq!(
            view,
            CommissioningStateView::NeedsRebind {
                agent_id: "agent-123".to_string()
            }
        );
    }

    #[test]
    fn repair_kind_accepts_camel_case_new_token() {
        let parsed: RepairKind = serde_json::from_value(json!({
            "kind": "rotate_token",
            "newToken": "fresh-token"
        }))
        .unwrap();

        match parsed {
            RepairKind::RotateToken { new_token } => assert_eq!(new_token, "fresh-token"),
            RepairKind::Rebind => panic!("expected rotate_token"),
        }
    }

    #[test]
    fn start_errors_serialize_with_structured_codes() {
        let value = serde_json::to_value(CommissioningCommandError::AlreadyCommissioned {
            agent_id: "agent-123".to_string(),
            message: "already commissioned".to_string(),
        })
        .unwrap();

        assert_eq!(
            value,
            json!({
                "code": "alreadyCommissioned",
                "agentId": "agent-123",
                "message": "already commissioned"
            })
        );
    }
}

pub fn run() {
    init_tracing();
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
            apply_bundle_config,
            commissioning_state,
            list_commissioning_roles,
            render_charter_from_role,
            commission_start,
            commission_qna,
            commission_finalize,
            commission_repair
        ])
        .run(tauri::generate_context!())
        .expect("error while running OrionII");
}
