//! Governance subscriber.
//!
//! Inbound governance from SAO (and from the local
//! `apply_sao_policy_refresh` command's fallback path) flows in through
//! `Topic::GovernanceInbound`. This subscriber dispatches on the
//! `payload.kind` field and applies the change to persistence — keeping the
//! command body a thin adapter and preserving Rule #4: governance arrives
//! over the bus, not via direct persistence calls.
//!
//! Today the only kind is `policy.refresh`; future kinds (memory.refresh,
//! birth.update, etc.) plug in here without changing the seam.

use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use tracing::{error, info, warn};

use crate::orion::bus::{Envelope, RecvError, SharedBus, Topic};
use crate::orion::charter::SharedCharter;
use crate::orion::persistence::{FilePersistence, Persistence};
use crate::orion::sao::PolicyOverlay;

/// Payload `kind` discriminator: a SAO policy refresh, or a local-fallback
/// policy synthesised by `apply_sao_policy_refresh` when SAO is offline.
pub const KIND_POLICY_REFRESH: &str = "policy.refresh";

/// Payload `kind` discriminator: a SAO-counter-signed charter update.
/// Body shape: `{ "kind": "charter.update", "charter_text": "...",
/// "birth_certificate": <BirthCertificate> }`. The subscriber rewrites the
/// local `charter.md` and `birth_certificate.json`, and replaces the
/// in-memory `SharedCharter` so subsequent envelopes carry the new
/// `soul_ref` from line one.
pub const KIND_CHARTER_UPDATE: &str = "charter.update";

pub fn spawn(
    bus: SharedBus,
    persistence: Arc<Mutex<FilePersistence>>,
    charter: SharedCharter,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe(Topic::GovernanceInbound);
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle_inbound(&persistence, &charter, env),
                Err(RecvError::Lagged(skipped)) => {
                    warn!(target: "orion::governance", skipped, "lagged on GovernanceInbound");
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

fn handle_inbound(
    persistence: &Arc<Mutex<FilePersistence>>,
    charter: &SharedCharter,
    env: Envelope,
) {
    let kind = env
        .payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match kind {
        KIND_POLICY_REFRESH => apply_policy_refresh(persistence, &env),
        KIND_CHARTER_UPDATE => apply_charter_update(charter, &env),
        other => {
            warn!(
                target: "orion::governance",
                kind = other,
                correlation_id = ?env.correlation_id,
                "unknown GovernanceInbound kind; ignoring"
            );
        }
    }
}

fn apply_policy_refresh(persistence: &Arc<Mutex<FilePersistence>>, env: &Envelope) {
    let Some(policy_value) = env.payload.get("policy") else {
        warn!(
            target: "orion::governance",
            correlation_id = ?env.correlation_id,
            "policy.refresh envelope missing `policy` field"
        );
        return;
    };
    let policy = match serde_json::from_value::<PolicyOverlay>(policy_value.clone()) {
        Ok(policy) => policy,
        Err(cause) => {
            warn!(
                target: "orion::governance",
                correlation_id = ?env.correlation_id,
                %cause,
                "policy.refresh envelope had malformed PolicyOverlay"
            );
            return;
        }
    };

    let new_version = policy.version;
    let mut p = persistence.lock().expect("persistence mutex poisoned");
    if let Err(cause) = p.apply_sao_refresh(Vec::new(), policy) {
        error!(
            target: "orion::governance",
            correlation_id = ?env.correlation_id,
            %cause,
            "failed to apply policy refresh"
        );
        return;
    }
    info!(
        target: "orion::governance",
        correlation_id = ?env.correlation_id,
        new_version,
        "policy_refresh_applied"
    );
}

fn apply_charter_update(charter: &SharedCharter, env: &Envelope) {
    let Some(text) = env.payload.get("charter_text").and_then(|v| v.as_str()) else {
        warn!(
            target: "orion::governance",
            correlation_id = ?env.correlation_id,
            "charter.update envelope missing `charter_text` field"
        );
        return;
    };

    // The certificate is informational here — `governance` writes it
    // to disk for diagnostics + repair flows but does not validate
    // signatures locally. SAO is the single source of truth; OrionII
    // re-fetches the canonical certificate via `GET /api/orion/birth`
    // when it needs to verify post-amendment.
    if let Some(certificate) = env.payload.get("birth_certificate") {
        if let Err(cause) = write_birth_certificate(certificate) {
            warn!(
                target: "orion::governance",
                correlation_id = ?env.correlation_id,
                %cause,
                "failed to persist birth certificate (continuing anyway)"
            );
        }
    }

    let mut c = charter.write().expect("charter rwlock poisoned");
    if let Err(cause) = c.replace(text) {
        error!(
            target: "orion::governance",
            correlation_id = ?env.correlation_id,
            %cause,
            "failed to write charter.md"
        );
        return;
    }
    let new_hash = c.hash();
    info!(
        target: "orion::governance",
        correlation_id = ?env.correlation_id,
        new_hash = %new_hash,
        "charter_update_applied"
    );
}

fn write_birth_certificate(value: &serde_json::Value) -> std::io::Result<()> {
    let path = birth_certificate_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, pretty)
}

fn birth_certificate_path() -> std::path::PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return std::path::PathBuf::from(appdata)
            .join("OrionII")
            .join("birth_certificate.json");
    }
    std::env::temp_dir()
        .join("OrionII")
        .join("birth_certificate.json")
}
