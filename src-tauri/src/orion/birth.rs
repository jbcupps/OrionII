//! "Birth" — the canonical first-touch SAO call that returns everything an entity needs to run.
//!
//! On every launch, OrionII calls `GET {sao_base_url}/api/orion/birth` with its entity bearer.
//! SAO returns the live agent metadata, endpoint URLs, scopes, current policy, and the
//! personality seed in one payload. This means changes made in SAO (new model, swapped
//! provider, updated policy) take effect on the next OrionII launch without re-bundling.
//!
//! If SAO is unreachable, the bundle config.json acts as the offline fallback.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

use crate::orion::sao::SaoClientConfig;

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthAgent {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub default_provider: Option<String>,
    pub default_id_model: Option<String>,
    pub default_ego_model: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthEndpoints {
    pub sao_base_url: String,
    pub policy_url: String,
    pub egress_url: String,
    pub llm_url: String,
    pub birth_url: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthOwner {
    pub user_id: Uuid,
    pub username: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthPolicy {
    pub version: u64,
    pub source: String,
    pub rules: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthPersonalitySeed {
    pub name: String,
    pub stance: String,
    pub drives: Vec<String>,
    pub deontological: f32,
    pub virtue: f32,
    pub consequential: f32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BirthResponse {
    pub birthed_at: DateTime<Utc>,
    pub client_version_min: String,
    pub agent: BirthAgent,
    pub endpoints: BirthEndpoints,
    pub owner: BirthOwner,
    pub scopes: Vec<String>,
    pub policy: BirthPolicy,
    pub personality_seed: BirthPersonalitySeed,
}

#[allow(dead_code)] // NotConfigured is reserved for future call sites that don't go through bootstrap
#[derive(Debug, Error)]
pub enum BirthError {
    #[error("SAO is not configured")]
    NotConfigured,
    #[error("birth call failed: {0}")]
    Http(String),
    #[error("birth response was unparseable: {0}")]
    Parse(String),
    #[error("birth rejected: HTTP {status}: {body}")]
    Rejected { status: u16, body: String },
}

impl BirthError {
    pub fn status_code(&self) -> Option<u16> {
        match self {
            BirthError::Rejected { status, .. } => Some(*status),
            _ => None,
        }
    }
}

pub async fn fetch_birth(config: &SaoClientConfig) -> Result<BirthResponse, BirthError> {
    let url = format!("{}/api/orion/birth", config.base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e: reqwest::Error| BirthError::Http(e.to_string()))?;

    let response = client
        .get(&url)
        .bearer_auth(&config.bearer_token)
        .send()
        .await
        .map_err(|e: reqwest::Error| BirthError::Http(e.to_string()))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e: reqwest::Error| BirthError::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(BirthError::Rejected {
            status: status.as_u16(),
            body: text,
        });
    }

    serde_json::from_str(&text).map_err(|e| BirthError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_mock_server(response: &'static str) -> String {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", server.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        base_url
    }

    #[tokio::test]
    async fn fetch_birth_preserves_rejected_status_code() {
        let base_url = spawn_mock_server(
            "HTTP/1.1 403 Forbidden\r\ncontent-length: 26\r\nconnection: close\r\n\r\n{\"error\":\"token rejected\"}",
        );
        let config = SaoClientConfig {
            base_url,
            bearer_token: "expired".to_string(),
            agent_id: None,
        };

        let error = fetch_birth(&config).await.unwrap_err();
        assert_eq!(error.status_code(), Some(403));
    }
}
