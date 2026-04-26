use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionIdentity {
    pub orion_id: Uuid,
    pub generation: u32,
    pub created_at: DateTime<Utc>,
    pub recovered_at: Option<DateTime<Utc>>,
    pub local_signing_key_id: Option<String>,
    pub sao_anchor: Option<String>,
}

impl CompanionIdentity {
    pub fn new() -> Self {
        Self {
            orion_id: Uuid::new_v4(),
            generation: 1,
            created_at: Utc::now(),
            recovered_at: None,
            local_signing_key_id: None,
            sao_anchor: None,
        }
    }

    pub fn mark_recovered(&mut self) {
        self.recovered_at = Some(Utc::now());
    }
}

impl Default for CompanionIdentity {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityState {
    pub identity: CompanionIdentity,
    pub version: u64,
    pub personality: PersonalityState,
    pub drives: Vec<String>,
    pub ethics_lean: EthicsLean,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl IdentityState {
    pub fn bootstrap() -> Self {
        Self {
            identity: CompanionIdentity::new(),
            version: 1,
            personality: PersonalityState::default(),
            drives: vec![
                "preserve continuity of self".to_string(),
                "serve the worker locally first".to_string(),
                "remain accountable to SAO asynchronously".to_string(),
            ],
            ethics_lean: EthicsLean::default(),
            last_sync_at: None,
            updated_at: Utc::now(),
        }
    }

    pub fn touch(&mut self) {
        self.version += 1;
        self.updated_at = Utc::now();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalityState {
    pub name: String,
    pub stance: String,
    pub continuity_note: String,
}

impl Default for PersonalityState {
    fn default() -> Self {
        Self {
            name: "Orion".to_string(),
            stance: "calm, direct, worker-owned companion".to_string(),
            continuity_note:
                "Identity is local-first and must survive restart, reinstall, and sync.".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthicsLean {
    pub deontological: f32,
    pub virtue: f32,
    pub consequential: f32,
}

impl Default for EthicsLean {
    fn default() -> Self {
        Self {
            deontological: 0.34,
            virtue: 0.33,
            consequential: 0.33,
        }
    }
}
