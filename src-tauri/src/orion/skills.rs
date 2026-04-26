use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orion::memory::DocumentChunk;
use crate::orion::message::{Author, Message, MessageKind, Payload, Priority};
use crate::orion::topics;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAuthorization {
    pub skill_name: String,
    pub scopes: Vec<String>,
    pub auth_kind: SkillAuthKind,
}

impl SkillAuthorization {
    pub fn oauth(skill_name: impl Into<String>, scopes: Vec<String>) -> Self {
        Self {
            skill_name: skill_name.into(),
            scopes,
            auth_kind: SkillAuthKind::Oauth,
        }
    }

    pub fn local_os(skill_name: impl Into<String>, scopes: Vec<String>) -> Self {
        Self {
            skill_name: skill_name.into(),
            scopes,
            auth_kind: SkillAuthKind::LocalOsAcl,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkillAuthKind {
    LocalOsAcl,
    Oauth,
}

#[derive(Default)]
pub struct DocumentSkill;

impl DocumentSkill {
    pub fn retrieve_context(&self, query: &str, chunks: &[DocumentChunk]) -> String {
        let terms = normalize_terms(query);
        if terms.is_empty() {
            return String::new();
        }

        chunks
            .iter()
            .filter(|chunk| {
                let haystack = chunk.text.to_lowercase();
                terms.iter().any(|term| haystack.contains(term))
            })
            .take(3)
            .map(|chunk| format!("{}#{}: {}", chunk.source_path, chunk.chunk_idx, chunk.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn chunk_document(&self, source_path: impl Into<String>, text: &str) -> Vec<DocumentChunk> {
        let source_path = source_path.into();
        text.split("\n\n")
            .map(str::trim)
            .filter(|chunk| !chunk.is_empty())
            .enumerate()
            .map(|(idx, chunk)| DocumentChunk::new(source_path.clone(), idx, chunk))
            .collect()
    }
}

#[derive(Default)]
pub struct OutlookSkill;

impl OutlookSkill {
    pub fn authorization(&self) -> SkillAuthorization {
        SkillAuthorization::local_os(
            "outlook",
            vec![
                "mail.read.local".to_string(),
                "calendar.read.local".to_string(),
            ],
        )
    }

    pub fn search_task(
        &self,
        session_id: Uuid,
        correlation_id: Uuid,
        parent_msg_id: Uuid,
        query: &str,
    ) -> Message {
        Message::new(
            MessageKind::AgentAssigned,
            Author::Ego,
            topics::AGENT_TASK_ASSIGNED,
            Priority::UserInput,
            session_id,
            correlation_id,
            Some(parent_msg_id),
            Payload::AgentTask {
                description: format!("Search local Outlook data for '{}'", query),
            },
        )
    }
}

#[derive(Default)]
pub struct OAuthSkillCatalog {
    skills: Vec<SkillAuthorization>,
}

impl OAuthSkillCatalog {
    pub fn register(&mut self, skill: SkillAuthorization) {
        self.skills.push(skill);
    }

    pub fn scopes_for(&self, skill_name: &str) -> Option<&[String]> {
        self.skills
            .iter()
            .find(|skill| skill.skill_name == skill_name)
            .map(|skill| skill.scopes.as_slice())
    }
}

fn normalize_terms(query: &str) -> Vec<String> {
    query
        .split(|char: char| !char.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 2)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_skill_chunks_and_retrieves_matching_context() {
        let skill = DocumentSkill;
        let chunks = skill.chunk_document(
            "notes.md",
            "Payroll policy lives here.\n\nOutlook integration notes live there.",
        );

        let context = skill.retrieve_context("What about Outlook?", &chunks);

        assert!(context.contains("Outlook integration"));
        assert!(!context.contains("Payroll policy"));
    }

    #[test]
    fn outlook_skill_uses_local_acl_not_graph_oauth() {
        let skill = OutlookSkill;
        let authorization = skill.authorization();

        assert_eq!(authorization.auth_kind, SkillAuthKind::LocalOsAcl);
        assert!(!authorization
            .scopes
            .iter()
            .any(|scope| scope.contains("graph")));
    }

    #[test]
    fn oauth_catalog_tracks_minimum_scopes_per_external_skill() {
        let mut catalog = OAuthSkillCatalog::default();
        catalog.register(SkillAuthorization::oauth(
            "ticketing",
            vec!["tickets.read".to_string()],
        ));

        assert_eq!(
            catalog.scopes_for("ticketing").unwrap(),
            ["tickets.read".to_string()]
        );
    }
}
