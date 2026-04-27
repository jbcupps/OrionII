//! Egress subscriber — the one ethical seam between the entity and SAO.
//!
//! Anything that crosses out of the entity to SAO must first land on
//! `Topic::EgressOutbound` and be picked up here. The subscriber runs each
//! envelope through `sanitize()`, enqueues an `SaoEgressRecord`, then ships
//! it via `SaoShipper`. Centralizing the seam in one file is what prevents
//! the Id from accidentally leaking raw stimulus to the mentor's governance
//! plane — see ADR-001.
//!
//! Phase 2a moved the SAO HTTP call **into this file** from `Persistence`.
//! `git grep -nE "SaoShipper" src-tauri/src` should match only here. That's
//! the strongest form of Rule #4: exactly one file calls the egress client.
//!
//! Phase 1 sanitizer is a key-name redaction stub. Full NPPI-aware
//! sanitization is `TODO(NPPI)` and lives in a separate ticket.

use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use uuid::Uuid;

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::persistence::{FilePersistence, Persistence, PersistenceError};
use crate::orion::sao::{SaoClientConfig, SaoEgressRecord, SaoEvent, SaoShipper, ShipReport};

pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    sao_config: Option<SaoClientConfig>,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::EgressOutbound);
    tauri::async_runtime::spawn(async move {
        let shipper = SaoShipper::with_config(sao_config);
        loop {
            match rx.recv().await {
                Ok(env) => handle_outbound(&persistence, &shipper, env).await,
                Err(RecvError::Lagged(skipped)) => {
                    eprintln!("[egress] lagged on EgressOutbound, skipped {skipped} envelopes");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

async fn handle_outbound(
    persistence: &Arc<Mutex<FilePersistence>>,
    shipper: &SaoShipper,
    env: Envelope,
) {
    let sanitized = sanitize(env);

    let action = sanitized
        .payload
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("egress")
        .to_string();
    let correlation_id = sanitized.correlation_id.unwrap_or_else(Uuid::new_v4);

    let record = SaoEgressRecord::pending(
        SaoEvent::AuditAction {
            action,
            correlation_id,
        }
        .sanitized(),
    );

    // Enqueue under the lock; release before any .await.
    {
        let mut p = persistence.lock().expect("persistence mutex poisoned");
        if let Err(error) = p.enqueue_sao(record) {
            eprintln!("[egress] failed to enqueue SAO event: {error}");
            return;
        }
    }

    if let Err(error) = ship_pending_with_shipper(persistence, shipper).await {
        eprintln!("[egress] failed to ship pending SAO egress: {error}");
    }
}

/// User-triggered flush for any pending SAO egress backlog. This keeps the
/// HTTP egress path inside `egress.rs`; commands and services never call
/// `SaoShipper` directly.
pub async fn ship_pending_backlog(
    persistence: &Arc<Mutex<FilePersistence>>,
    sao_config: Option<SaoClientConfig>,
) -> Result<ShipReport, PersistenceError> {
    let shipper = SaoShipper::with_config(sao_config);
    ship_pending_with_shipper(persistence, &shipper).await
}

async fn ship_pending_with_shipper(
    persistence: &Arc<Mutex<FilePersistence>>,
    shipper: &SaoShipper,
) -> Result<ShipReport, PersistenceError> {
    let (orion_id, mut pending) = {
        let mut p = persistence.lock().expect("persistence mutex poisoned");
        (p.identity().identity.orion_id, p.take_pending_egress())
    };

    if pending.is_empty() {
        return Ok(ShipReport {
            attempted: 0,
            acked: 0,
            failed: 0,
        });
    }

    // Ship outside the lock. SaoShipper makes the HTTP call.
    let report = shipper.ship_pending(&mut pending, orion_id).await;

    // Merge results back under the lock.
    let mut p = persistence.lock().expect("persistence mutex poisoned");
    p.merge_egress_results(pending)?;
    Ok(report)
}

/// Phase 1 sanitization stub. Walks the envelope payload and removes any
/// object key whose name matches the redaction list (case-insensitive).
///
/// This is intentionally simple — it is not the real NPPI sanitizer
/// (TODO(NPPI)). It exists so the seam is visibly load-bearing on Day 1.
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
        Envelope::new(Topic::EgressOutbound, "agent-1", "soul:v1", None, payload)
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
