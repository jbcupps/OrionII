use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::orion::identity::IdentityState;
use crate::orion::memory::MemoryRecord;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaoEgressRecord {
    pub id: Uuid,
    pub event: SaoEvent,
    pub enqueued_at: DateTime<Utc>,
    pub attempts: u32,
    pub state: SaoEgressState,
}

impl SaoEgressRecord {
    pub fn pending(event: SaoEvent) -> Self {
        Self {
            id: Uuid::new_v4(),
            event,
            enqueued_at: Utc::now(),
            attempts: 0,
            state: SaoEgressState::Pending,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SaoEgressState {
    Pending,
    Acked,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SaoEvent {
    AuditAction {
        action: String,
        correlation_id: Uuid,
    },
    MemoryEvent {
        memory_id: Uuid,
        content: String,
    },
    IdentitySync {
        orion_id: Uuid,
        version: u64,
    },
}

impl SaoEvent {
    pub fn sanitized(self) -> Self {
        match self {
            SaoEvent::AuditAction {
                action,
                correlation_id,
            } => SaoEvent::AuditAction {
                action: sanitize_nppi(&action),
                correlation_id,
            },
            SaoEvent::MemoryEvent { memory_id, content } => SaoEvent::MemoryEvent {
                memory_id,
                content: sanitize_nppi(&content),
            },
            SaoEvent::IdentitySync { orion_id, version } => {
                SaoEvent::IdentitySync { orion_id, version }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyOverlay {
    pub version: u64,
    pub source: String,
    pub rules: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipReport {
    pub attempted: usize,
    pub acked: usize,
    pub failed: usize,
}

#[derive(Clone, Debug)]
pub struct SaoClientConfig {
    pub base_url: String,
    pub bearer_token: String,
    pub agent_id: Option<Uuid>,
}

impl SaoClientConfig {
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("SAO_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        let bearer_token = std::env::var("SAO_DEV_BEARER_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        let agent_id = std::env::var("SAO_AGENT_ID")
            .ok()
            .and_then(|value| Uuid::parse_str(value.trim()).ok());

        Some(Self {
            base_url,
            bearer_token,
            agent_id,
        })
    }

    /// Construct from a SAO-issued bundle config. The bearer here is the entity JWT.
    #[allow(dead_code)] // back-compat helper kept for tests / external callers
    pub fn from_bundle(base_url: String, bearer_token: String, agent_id: Uuid) -> Self {
        Self::from_bundle_anchor(base_url, bearer_token, Some(agent_id))
    }

    /// Bundle anchor — agent_id is optional because the live birth call is the canonical
    /// source of identity (the JWT itself carries it via `sub`).
    pub fn from_bundle_anchor(
        base_url: String,
        bearer_token: String,
        agent_id: Option<Uuid>,
    ) -> Self {
        Self {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            bearer_token,
            agent_id,
        }
    }
}

#[derive(Debug, Error)]
pub enum SaoClientError {
    #[error("SAO client is not configured")]
    NotConfigured,
    #[error("SAO request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub struct SaoShipper {
    config: Option<SaoClientConfig>,
    client: reqwest::Client,
}

impl Default for SaoShipper {
    fn default() -> Self {
        Self {
            config: SaoClientConfig::from_env(),
            client: reqwest::Client::new(),
        }
    }
}

impl SaoShipper {
    pub fn with_config(config: Option<SaoClientConfig>) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    #[cfg(test)]
    fn new(config: Option<SaoClientConfig>) -> Self {
        Self::with_config(config)
    }

    pub async fn ship_pending(
        &self,
        records: &mut [SaoEgressRecord],
        orion_id: Uuid,
    ) -> ShipReport {
        let mut report = ShipReport {
            attempted: 0,
            acked: 0,
            failed: 0,
        };
        let pending = records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                matches!(record.state, SaoEgressState::Pending).then_some(index)
            })
            .collect::<Vec<_>>();

        if pending.is_empty() {
            return report;
        }

        let Some(config) = &self.config else {
            report.failed = pending.len();
            return report;
        };

        let events = pending
            .iter()
            .map(|index| {
                let record = &mut records[*index];
                record.attempts += 1;
                report.attempted += 1;
                SaoEgressPayload {
                    id: record.id,
                    enqueued_at: record.enqueued_at,
                    attempts: record.attempts,
                    event: record.event.clone(),
                }
            })
            .collect::<Vec<_>>();

        let response = self
            .client
            .post(format!("{}/api/orion/egress", config.base_url))
            .bearer_auth(&config.bearer_token)
            .json(&SaoEgressRequest {
                agent_id: config.agent_id,
                orion_id,
                events,
                client_version: crate::orion::bootstrap::CLIENT_VERSION,
            })
            .send()
            .await;

        let Ok(response) = response else {
            report.failed = report.attempted;
            return report;
        };
        if !response.status().is_success() {
            report.failed = report.attempted;
            return report;
        }

        let Ok(response) = response.json::<SaoEgressResponse>().await else {
            report.failed = report.attempted;
            return report;
        };

        for result in response.results {
            if matches!(
                result.status,
                SaoEgressAckStatus::Acked | SaoEgressAckStatus::Duplicate
            ) {
                if let Some(record) = records.iter_mut().find(|record| record.id == result.id) {
                    record.state = SaoEgressState::Acked;
                    report.acked += 1;
                }
            }
        }
        report.failed = report.attempted.saturating_sub(report.acked);
        report
    }

    pub async fn fetch_policy(&self) -> Result<PolicyOverlay, SaoClientError> {
        let Some(config) = &self.config else {
            return Err(SaoClientError::NotConfigured);
        };

        let policy = self
            .client
            .get(format!("{}/api/orion/policy", config.base_url))
            .bearer_auth(&config.bearer_token)
            .send()
            .await?
            .error_for_status()?
            .json::<PolicyOverlay>()
            .await?;

        Ok(policy)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoEgressRequest {
    agent_id: Option<Uuid>,
    orion_id: Uuid,
    events: Vec<SaoEgressPayload>,
    client_version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoEgressPayload {
    id: Uuid,
    enqueued_at: DateTime<Utc>,
    attempts: u32,
    event: SaoEvent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaoEgressResponse {
    results: Vec<SaoEgressResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaoEgressResult {
    id: Uuid,
    status: SaoEgressAckStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum SaoEgressAckStatus {
    Acked,
    Duplicate,
    Failed,
}

impl Default for PolicyOverlay {
    fn default() -> Self {
        Self {
            version: 1,
            source: "local-default".to_string(),
            rules: vec![
                "SAO policy has not been pulled yet; local safety defaults apply.".to_string(),
            ],
            updated_at: Utc::now(),
        }
    }
}

pub struct MergeResult {
    pub identity: IdentityState,
    pub memories: Vec<MemoryRecord>,
    pub policy: PolicyOverlay,
}

pub fn merge_sao_refresh(
    mut local_identity: IdentityState,
    local_memories: Vec<MemoryRecord>,
    remote_memories: Vec<MemoryRecord>,
    remote_policy: PolicyOverlay,
) -> MergeResult {
    local_identity.touch();

    let mut merged = local_memories;
    for remote in remote_memories {
        let has_local_change = merged
            .iter()
            .any(|local| local.id == remote.id && local.updated_since_sync);

        if !has_local_change && !merged.iter().any(|local| local.id == remote.id) {
            merged.push(remote);
        }
    }

    MergeResult {
        identity: local_identity,
        memories: merged,
        policy: remote_policy,
    }
}

// `audit_message` was removed when the bus refactor consolidated egress to
// the `egress.outbound` subscriber (see ADR-001). Audit events are now
// generated inside `egress::handle_outbound`, which converts envelopes into
// `SaoEgressRecord`s — there is no longer a free-standing helper that
// constructs an audit `Message` from outside that seam.

pub fn sanitize_nppi(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            let has_at = token.contains('@');
            let digit_count = token.chars().filter(|char| char.is_ascii_digit()).count();

            if has_at || digit_count >= 7 {
                "[redacted]".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::identity::IdentityState;
    use crate::orion::memory::MemoryRecord;
    use crate::orion::message::Author;

    #[test]
    fn sanitizer_redacts_email_and_long_numbers() {
        let sanitized = sanitize_nppi("email worker@example.com and phone 5551234567");

        assert_eq!(sanitized, "email [redacted] and phone [redacted]");
    }

    #[test]
    fn sao_merge_keeps_local_identity_and_changed_memory() {
        let identity = IdentityState::bootstrap();
        let original_id = identity.identity.orion_id;
        let memory_id = Uuid::new_v4();
        let local_memory = MemoryRecord {
            id: memory_id,
            session_id: Uuid::new_v4(),
            author: Author::User,
            content: "local lived memory".to_string(),
            promoted: false,
            updated_since_sync: true,
            created_at: Utc::now(),
        };
        let remote_memory = MemoryRecord {
            content: "remote curated memory".to_string(),
            updated_since_sync: false,
            ..local_memory.clone()
        };
        let policy = PolicyOverlay {
            version: 2,
            source: "sao".to_string(),
            rules: vec!["policy wins".to_string()],
            updated_at: Utc::now(),
        };

        let merged = merge_sao_refresh(identity, vec![local_memory], vec![remote_memory], policy);

        assert_eq!(merged.identity.identity.orion_id, original_id);
        assert_eq!(merged.memories[0].content, "local lived memory");
        assert_eq!(merged.policy.version, 2);
    }

    #[tokio::test]
    async fn shipper_acks_pending_records_without_dropping_history() {
        let mut records = vec![SaoEgressRecord::pending(SaoEvent::AuditAction {
            action: "open local document".to_string(),
            correlation_id: Uuid::new_v4(),
        })];
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", server.local_addr().unwrap());
        let event_id = records[0].id;
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut buffer = [0u8; 2048];
            let _ = std::io::Read::read(&mut stream, &mut buffer).unwrap();
            let body = format!(
                r#"{{"accepted":1,"duplicate":0,"failed":0,"results":[{{"id":"{}","status":"acked"}}]}}"#,
                event_id
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        let shipper = SaoShipper::new(Some(SaoClientConfig {
            base_url,
            bearer_token: "dev-token".to_string(),
            agent_id: None,
        }));
        let report = shipper.ship_pending(&mut records, Uuid::new_v4()).await;

        assert_eq!(report.attempted, 1);
        assert_eq!(report.acked, 1);
        assert!(matches!(records[0].state, SaoEgressState::Acked));
        assert_eq!(records[0].attempts, 1);
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn shipper_leaves_records_pending_when_unconfigured() {
        let mut records = vec![SaoEgressRecord::pending(SaoEvent::IdentitySync {
            orion_id: Uuid::new_v4(),
            version: 1,
        })];
        let report = SaoShipper::new(None)
            .ship_pending(&mut records, Uuid::new_v4())
            .await;

        assert_eq!(report.acked, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(records[0].attempts, 0);
        assert!(matches!(records[0].state, SaoEgressState::Pending));
    }
}
