//! HTTP client for SAO's commissioning surface.
//!
//! See `docs/sao-commissioning-contract.md` for the wire contract. This is
//! the only file in OrionII that calls `/api/orion/commission/*` — keeping
//! the surface in one place means contract drift is localized to a single
//! diff. The Tauri commands in `lib.rs` are thin adapters; UI state is in
//! `src/Commissioning/`.
//!
//! Errors are typed (`CommissioningError`) so the cockpit can branch on
//! `TokenInvalid`, `AlreadyCommissioned`, etc., and route the operator to
//! the right repair sub-mode without parsing strings.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::orion::bootstrap::CLIENT_VERSION;
use crate::orion::sao::SaoClientConfig;

const COMMISSION_VERSION_HEADER: &str = "X-Orion-Commission-Version";
const COMMISSION_VERSION: &str = "2";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResponse {
    pub commission_id: Uuid,
    pub mentor_id: Uuid,
    pub agent_id: Uuid,
    pub mentor_public_key_fpr: String,
    pub entity_public_key_fpr: String,
    pub allowed_role_keys: Vec<String>,
    #[serde(default)]
    pub q_and_a_enabled: bool,
    #[serde(default)]
    pub sao_provider: Option<String>,
    #[serde(default)]
    pub sao_id_model: Option<String>,
    #[serde(default)]
    pub sao_ego_model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizePayload {
    pub commission_id: Uuid,
    pub role_key: String,
    pub charter_text: String,
    pub charter_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeResponse {
    pub agent_id: Uuid,
    pub orion_id: Uuid,
    pub soul_ref: String,
    pub charter_hash: String,
    pub birth_certificate: BirthCertificate,
    pub defaults: CommissioningDefaults,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthCertificate {
    pub agent_id: Uuid,
    pub mentor_public_key: String,
    pub entity_public_key: String,
    pub charter_hash: String,
    pub role_key: String,
    pub issued_at: chrono::DateTime<chrono::Utc>,
    pub mentor_signature: String,
    pub entity_signature: String,
    pub sao_signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommissioningDefaults {
    pub provider: String,
    pub id_model: String,
    pub ego_model: String,
    #[serde(default)]
    pub policy_version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RepairRequest {
    #[serde(rename = "rotate_token")]
    RotateToken { new_token: String },
    #[serde(rename = "rebind")]
    Rebind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairResponse {
    pub agent_id: Uuid,
    pub orion_id: Uuid,
    pub soul_ref: String,
    pub charter_hash: String,
    pub charter_text: String,
    pub birth_certificate: BirthCertificate,
    pub defaults: CommissioningDefaults,
}

#[derive(Debug, Error)]
pub enum CommissioningError {
    #[error("SAO is not configured (missing bundle)")]
    NotConfigured,
    #[error("token rejected by SAO; refresh credentials")]
    TokenInvalid,
    #[error("agent is already commissioned ({agent_id}); use repair to re-bind")]
    AlreadyCommissioned { agent_id: Uuid },
    #[error("agent {agent_id} no longer exists in SAO")]
    AgentNotFound { agent_id: Uuid },
    #[error("SAO returned charter hash mismatch (local {local}, remote {remote})")]
    CharterHashMismatch { local: String, remote: String },
    #[error("commissioning version unsupported by SAO; expected {expected:?}")]
    UnsupportedVersion { expected: Vec<String> },
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub struct CommissioningClient {
    config: SaoClientConfig,
    client: reqwest::Client,
}

impl CommissioningClient {
    pub fn from_config(config: Option<SaoClientConfig>) -> Result<Self, CommissioningError> {
        let config = config.ok_or(CommissioningError::NotConfigured)?;
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| CommissioningError::Http(e.to_string()))?;
        Ok(Self { config, client })
    }

    /// Allow callers (e.g. token-rotation flows) to override the bearer
    /// without rebuilding the rest of the client.
    pub fn with_token(mut self, new_token: String) -> Self {
        self.config.bearer_token = new_token;
        self
    }

    pub async fn start(&self) -> Result<StartResponse, CommissioningError> {
        let body = serde_json::json!({ "clientVersion": CLIENT_VERSION });
        let response = self
            .client
            .post(format!(
                "{}/api/orion/commission/start",
                self.config.base_url
            ))
            .bearer_auth(&self.config.bearer_token)
            .header(COMMISSION_VERSION_HEADER, COMMISSION_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|e| CommissioningError::Decode(e.to_string()));
        }

        Err(map_error_status(status, &bytes))
    }

    pub async fn finalize(
        &self,
        payload: FinalizePayload,
    ) -> Result<FinalizeResponse, CommissioningError> {
        let response = self
            .client
            .post(format!(
                "{}/api/orion/commission/finalize",
                self.config.base_url
            ))
            .bearer_auth(&self.config.bearer_token)
            .header(COMMISSION_VERSION_HEADER, COMMISSION_VERSION)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        if status.is_success() {
            let parsed: FinalizeResponse = serde_json::from_slice(&bytes)
                .map_err(|e| CommissioningError::Decode(e.to_string()))?;
            verify_soul_ref(
                &parsed.soul_ref,
                &parsed.charter_hash,
                &payload.charter_hash,
            )?;
            return Ok(parsed);
        }

        Err(map_error_status(status, &bytes))
    }

    pub async fn repair(
        &self,
        request: RepairRequest,
    ) -> Result<RepairResponse, CommissioningError> {
        let response = self
            .client
            .post(format!(
                "{}/api/orion/commission/repair",
                self.config.base_url
            ))
            .bearer_auth(&self.config.bearer_token)
            .header(COMMISSION_VERSION_HEADER, COMMISSION_VERSION)
            .json(&request)
            .send()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CommissioningError::Http(e.to_string()))?;

        if status.is_success() {
            let parsed: RepairResponse = serde_json::from_slice(&bytes)
                .map_err(|e| CommissioningError::Decode(e.to_string()))?;
            // Sanity-check the local charter text really hashes to what
            // SAO claims — the contract requires this and an out-of-sync
            // SAO is a bug we want to catch before persisting locally.
            let recomputed = blake3::hash(parsed.charter_text.as_bytes())
                .to_hex()
                .to_string();
            if recomputed != parsed.charter_hash {
                return Err(CommissioningError::CharterHashMismatch {
                    local: recomputed,
                    remote: parsed.charter_hash.clone(),
                });
            }
            verify_soul_ref(&parsed.soul_ref, &parsed.charter_hash, &recomputed)?;
            return Ok(parsed);
        }

        Err(map_error_status(status, &bytes))
    }
}

fn verify_soul_ref(
    soul_ref: &str,
    remote_hash: &str,
    local_hash: &str,
) -> Result<(), CommissioningError> {
    if remote_hash != local_hash {
        return Err(CommissioningError::CharterHashMismatch {
            local: local_hash.to_string(),
            remote: remote_hash.to_string(),
        });
    }
    let expected = format!("blake3:{}", remote_hash);
    if soul_ref != expected {
        return Err(CommissioningError::Decode(format!(
            "soulRef shape mismatch: expected {expected}, got {soul_ref}"
        )));
    }
    Ok(())
}

fn map_error_status(status: reqwest::StatusCode, body: &[u8]) -> CommissioningError {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ErrorBody {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        agent_id: Option<Uuid>,
        #[serde(default)]
        expected: Option<Vec<String>>,
    }
    let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or(ErrorBody {
        code: None,
        agent_id: None,
        expected: None,
    });

    match status.as_u16() {
        401 => CommissioningError::TokenInvalid,
        409 if parsed.code.as_deref() == Some("already_commissioned") => {
            CommissioningError::AlreadyCommissioned {
                agent_id: parsed.agent_id.unwrap_or_else(Uuid::nil),
            }
        }
        404 => CommissioningError::AgentNotFound {
            agent_id: parsed.agent_id.unwrap_or_else(Uuid::nil),
        },
        400 if parsed.code.as_deref() == Some("unsupported_commission_version") => {
            CommissioningError::UnsupportedVersion {
                expected: parsed.expected.unwrap_or_default(),
            }
        }
        _ => CommissioningError::Http(format!(
            "{} {}",
            status.as_u16(),
            String::from_utf8_lossy(body)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_mock_server<F>(handler: F) -> String
    where
        F: FnOnce(&[u8]) -> String + Send + 'static,
    {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", server.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).unwrap();
            let response = handler(&buffer[..n]);
            stream.write_all(response.as_bytes()).unwrap();
        });
        base_url
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn unauthorized_response() -> String {
        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
    }

    #[tokio::test]
    async fn start_decodes_camel_case_response() {
        let body = serde_json::json!({
            "commissionId": Uuid::new_v4(),
            "mentorId": Uuid::new_v4(),
            "agentId": Uuid::new_v4(),
            "mentorPublicKeyFpr": "abcd",
            "entityPublicKeyFpr": "ef01",
            "allowedRoleKeys": ["calendar_assistant"],
            "qAndAEnabled": true,
            "saoProvider": "anthropic",
            "saoIdModel": "haiku",
            "saoEgoModel": "haiku"
        })
        .to_string();
        let url = spawn_mock_server(move |_| ok_response(&body));

        let client = CommissioningClient::from_config(Some(SaoClientConfig {
            base_url: url,
            bearer_token: "tok".into(),
            agent_id: None,
        }))
        .unwrap();
        let response = client.start().await.unwrap();
        assert_eq!(response.allowed_role_keys, vec!["calendar_assistant"]);
        assert_eq!(response.sao_provider.as_deref(), Some("anthropic"));
    }

    #[tokio::test]
    async fn start_maps_401_to_token_invalid() {
        let url = spawn_mock_server(|_| unauthorized_response());
        let client = CommissioningClient::from_config(Some(SaoClientConfig {
            base_url: url,
            bearer_token: "expired".into(),
            agent_id: None,
        }))
        .unwrap();

        let err = client.start().await.unwrap_err();
        assert!(matches!(err, CommissioningError::TokenInvalid));
    }

    #[tokio::test]
    async fn finalize_rejects_charter_hash_mismatch() {
        let agent_id = Uuid::new_v4();
        let body = serde_json::json!({
            "agentId": agent_id,
            "orionId": Uuid::new_v4(),
            // Mismatch: soul_ref hash != charter_hash field in payload
            "soulRef": "blake3:0000",
            "charterHash": "ffff",
            "birthCertificate": {
                "agentId": agent_id,
                "mentorPublicKey": "mpk",
                "entityPublicKey": "epk",
                "charterHash": "ffff",
                "roleKey": "calendar_assistant",
                "issuedAt": chrono::Utc::now(),
                "mentorSignature": "ms",
                "entitySignature": "es",
                "saoSignature": "ss"
            },
            "defaults": {
                "provider": "anthropic",
                "idModel": "haiku",
                "egoModel": "haiku",
                "policyVersion": 1
            }
        })
        .to_string();
        let url = spawn_mock_server(move |_| ok_response(&body));
        let client = CommissioningClient::from_config(Some(SaoClientConfig {
            base_url: url,
            bearer_token: "tok".into(),
            agent_id: None,
        }))
        .unwrap();

        let payload = FinalizePayload {
            commission_id: Uuid::new_v4(),
            role_key: "calendar_assistant".into(),
            charter_text: "# c".into(),
            charter_hash: "aaaa".into(),
        };
        let err = client.finalize(payload).await.unwrap_err();
        assert!(matches!(
            err,
            CommissioningError::CharterHashMismatch { .. }
        ));
    }
}
