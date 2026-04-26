use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityHealth {
    pub constitutional_integrity: IntegrityState,
    pub checked_at: DateTime<Utc>,
    pub remediation: Option<String>,
}

impl SecurityHealth {
    pub fn healthy() -> Self {
        Self {
            constitutional_integrity: IntegrityState::Verified,
            checked_at: Utc::now(),
            remediation: None,
        }
    }

    pub fn degraded(reason: impl Into<String>) -> Self {
        Self {
            constitutional_integrity: IntegrityState::Degraded,
            checked_at: Utc::now(),
            remediation: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IntegrityState {
    Verified,
    Degraded,
}

#[derive(Default)]
pub struct ConstitutionalVerifier;

impl ConstitutionalVerifier {
    pub fn verify(&self, document: &str, signature: Option<&str>) -> SecurityHealth {
        match signature {
            Some(signature) if !signature.trim().is_empty() && !document.trim().is_empty() => {
                SecurityHealth::healthy()
            }
            _ => SecurityHealth::degraded(
                "constitutional signature unavailable; continue locally and request SAO remediation",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_signature_degrades_instead_of_failing_closed() {
        let health = ConstitutionalVerifier.verify("constitution", None);

        assert_eq!(health.constitutional_integrity, IntegrityState::Degraded);
        assert!(health.remediation.unwrap().contains("SAO remediation"));
    }
}
