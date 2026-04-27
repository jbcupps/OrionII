//! Egress subscriber — the one ethical seam between the entity and SAO.
//!
//! Anything that crosses out of the entity to SAO must first land on
//! `Topic::EgressOutbound` and be picked up here. The subscriber runs each
//! envelope through `sanitize()`, then enqueues it for the existing SAO
//! shipper (HTTP `POST /api/orion/egress`). Centralizing the seam in one
//! file is what prevents the Id from accidentally leaking raw stimulus to
//! the mentor's governance plane — see ADR-001.
//!
//! Phase 1 sanitizer is a key-name redaction stub. Full NPPI-aware
//! sanitization is `TODO(NPPI)` and lives in a separate ticket. The seam is
//! load-bearing on Day 1; the policy inside the seam can deepen later
//! without changing where anything sits.

use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use tokio::sync::broadcast;

use crate::orion::bus::{Envelope, SharedBus, Topic};
use crate::orion::persistence::{FilePersistence, Persistence};
use crate::orion::sao::{SaoClientConfig, SaoEgressRecord, SaoEvent};

pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    sao_config: Option<SaoClientConfig>,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::EgressOutbound);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_outbound(&persistence, &sao_config, env),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    eprintln!(
                        "[egress] lagged on EgressOutbound, skipped {skipped} envelopes"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn handle_outbound(
    persistence: &Arc<Mutex<FilePersistence>>,
    sao_config: &Option<SaoClientConfig>,
    env: Envelope,
) {
    let sanitized = sanitize(env);

    let action = sanitized
        .payload
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("egress")
        .to_string();
    let correlation_id = sanitized.correlation_id.unwrap_or_else(uuid::Uuid::new_v4);

    let record = SaoEgressRecord::pending(
        SaoEvent::AuditAction {
            action,
            correlation_id,
        }
        .sanitized(),
    );

    {
        let mut p = persistence.lock().expect("persistence mutex poisoned");
        if let Err(error) = p.enqueue_sao(record) {
            eprintln!("[egress] failed to enqueue SAO event: {error}");
            return;
        }
    }

    // Best-effort ship; failure is fine — the record stays Pending and the
    // user-triggered ship_sao_egress command (or a future scheduled
    // shipper) will retry.
    let mut p = persistence.lock().expect("persistence mutex poisoned");
    if let Err(error) = p.ship_sao_egress(sao_config.as_ref()) {
        eprintln!("[egress] ship attempt failed: {error}");
    }
}

/// Phase 1 sanitization stub. Walks the envelope payload and removes any
/// object key whose name matches the redaction list (case-insensitive).
///
/// This is intentionally simple — it is not the real NPPI sanitizer
/// (TODO(NPPI)). It exists so the seam is visibly load-bearing on Day 1:
/// "egress sanitization actually does *something*" is what a code reader
/// will see when they trace the path. The richer policy plugs in here
/// without changing where it sits in the architecture.
pub fn sanitize(mut env: Envelope) -> Envelope {
    redact_in_place(&mut env.payload);
    env
}

const REDACT_KEY_FRAGMENTS: &[&str] = &["secret", "token", "key", "password"];

fn redact_in_place(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            map.retain(|k, _| {
                let lower = k.to_lowercase();
                !REDACT_KEY_FRAGMENTS
                    .iter()
                    .any(|fragment| lower.contains(fragment))
            });
            for (_, v) in map.iter_mut() {
                redact_in_place(v);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_in_place(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::bus::Topic;
    use serde_json::json;

    fn make(payload: serde_json::Value) -> Envelope {
        Envelope::new(
            Topic::EgressOutbound,
            "agent-1",
            "soul:v1",
            None,
            payload,
        )
    }

    #[test]
    fn plain_payload_passes_through_unchanged() {
        let env = make(json!({
            "action": "ship",
            "user": "worker"
        }));
        let out = sanitize(env);
        assert_eq!(out.payload["action"], "ship");
        assert_eq!(out.payload["user"], "worker");
    }

    #[test]
    fn payload_with_sensitive_keys_loses_them() {
        let env = make(json!({
            "user": "worker",
            "api_key": "should-not-leak",
            "nested": {
                "session_token": "drop-me",
                "keep": "yes"
            }
        }));
        let out = sanitize(env);
        assert_eq!(out.payload["user"], "worker");
        assert!(out.payload.get("api_key").is_none());
        assert_eq!(out.payload["nested"]["keep"], "yes");
        assert!(out.payload["nested"].get("session_token").is_none());
    }
}
