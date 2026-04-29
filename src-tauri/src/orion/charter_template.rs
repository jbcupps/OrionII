//! Role templates for the Commissioning **fast path**.
//!
//! Each role lives in `src-tauri/templates/roles/*.toml` and is embedded
//! into the binary at compile time via `include_str!`. The fast path takes
//! the operator's slot answers and renders the `charter_template` string
//! into a Markdown charter that proceeds to the Review stage like any other
//! charter.
//!
//! The renderer is hand-rolled `String::replace` over `{{ key }}`
//! placeholders rather than a templating crate — six small templates with
//! a fixed slot vocabulary do not justify a tinytemplate / handlebars
//! dependency. Every slot is required; missing-slot rendering returns an
//! error rather than emitting a half-filled charter.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const ROLE_FILES: &[(&str, &str)] = &[
    (
        "calendar_assistant",
        include_str!("../../templates/roles/calendar_assistant.toml"),
    ),
    (
        "tech_document_reader_writer",
        include_str!("../../templates/roles/tech_document_reader_writer.toml"),
    ),
    (
        "email_triage",
        include_str!("../../templates/roles/email_triage.toml"),
    ),
    (
        "research_analyst",
        include_str!("../../templates/roles/research_analyst.toml"),
    ),
    (
        "project_coordinator",
        include_str!("../../templates/roles/project_coordinator.toml"),
    ),
    (
        "compliance_reviewer",
        include_str!("../../templates/roles/compliance_reviewer.toml"),
    ),
];

/// On-disk shape (TOML, snake_case). The cockpit-facing JSON is built by
/// the Tauri command in Slice 7 via a separate `RoleSummary` so the wire
/// format can be camelCase without dragging the TOML file format with it.
#[derive(Clone, Debug, Deserialize)]
pub struct Role {
    pub key: String,
    pub display_name: String,
    pub description: String,
    pub time_estimate_minutes: u32,
    pub slots: Vec<Slot>,
    /// Markdown body with `{{ slot_key }}` placeholders. Not exposed to
    /// the cockpit's role list (the cockpit only needs metadata + slots);
    /// rendering happens server-side via `render`.
    #[serde(default)]
    pub charter_template: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Slot {
    pub key: String,
    pub label: String,
    pub kind: SlotKind,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SlotKind {
    Text,
    Url,
    Timezone,
    Multiline,
}

#[derive(Debug, Error)]
pub enum RoleError {
    #[error("role `{0}` not found")]
    UnknownRole(String),
    #[error("role template `{role}` failed to parse: {cause}")]
    Parse { role: String, cause: String },
    #[error("missing required slot `{0}`")]
    MissingSlot(String),
    #[error("template still contains unrendered placeholder `{0}`")]
    UnrenderedPlaceholder(String),
}

pub fn load_all() -> Result<Vec<Role>, RoleError> {
    ROLE_FILES
        .iter()
        .map(|(key, body)| {
            toml::from_str::<Role>(body).map_err(|cause| RoleError::Parse {
                role: (*key).to_string(),
                cause: cause.to_string(),
            })
        })
        .collect()
}

pub fn find(role_key: &str) -> Result<Role, RoleError> {
    load_all()?
        .into_iter()
        .find(|role| role.key == role_key)
        .ok_or_else(|| RoleError::UnknownRole(role_key.to_string()))
}

pub fn render(role: &Role, slot_values: &HashMap<String, String>) -> Result<String, RoleError> {
    for slot in &role.slots {
        let value = slot_values.get(&slot.key);
        if value.map(|v| v.trim().is_empty()).unwrap_or(true) {
            return Err(RoleError::MissingSlot(slot.key.clone()));
        }
    }

    let mut rendered = role.charter_template.clone();
    for (key, value) in slot_values {
        // Tolerate `{{ key }}`, `{{key}}`, and `{{  key }}` — operators
        // pasting templates into an editor often shift whitespace.
        for placeholder in [
            format!("{{{{ {key} }}}}"),
            format!("{{{{{key}}}}}"),
            format!("{{{{  {key}  }}}}"),
        ] {
            rendered = rendered.replace(&placeholder, value);
        }
    }

    if let Some(unrendered) = scan_for_placeholder(&rendered) {
        return Err(RoleError::UnrenderedPlaceholder(unrendered));
    }
    Ok(rendered)
}

/// Returns the first `{{ ... }}` placeholder still in the rendered text,
/// or `None` if rendering filled every slot.
fn scan_for_placeholder(rendered: &str) -> Option<String> {
    let start = rendered.find("{{")?;
    let after_start = &rendered[start + 2..];
    let end = after_start.find("}}")?;
    Some(format!("{{{{{}}}}}", &after_start[..end].trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populate(role: &Role) -> HashMap<String, String> {
        role.slots
            .iter()
            .map(|slot| (slot.key.clone(), format!("test-{}", slot.key)))
            .collect()
    }

    #[test]
    fn every_template_loads_and_renders_without_unfilled_placeholders() {
        let roles = load_all().expect("all role templates must parse");
        assert_eq!(
            roles.len(),
            6,
            "v1 ships exactly six role templates; update this test deliberately when that changes"
        );

        for role in &roles {
            let values = populate(role);
            let rendered = render(role, &values).unwrap_or_else(|e| panic!("{}: {e}", role.key));
            assert!(
                !rendered.contains("{{"),
                "{}: rendered charter still contains a `{{{{` placeholder:\n{}",
                role.key,
                rendered
            );
            // Spot-check every slot value made it into the body.
            for value in values.values() {
                assert!(
                    rendered.contains(value),
                    "{}: rendered charter is missing slot value `{}`",
                    role.key,
                    value
                );
            }
        }
    }

    #[test]
    fn render_rejects_missing_slot() {
        let role = find("calendar_assistant").unwrap();
        let mut values = populate(&role);
        values.remove("organization");
        let err = render(&role, &values).unwrap_err();
        assert!(matches!(err, RoleError::MissingSlot(slot) if slot == "organization"));
    }

    #[test]
    fn find_returns_unknown_role_error() {
        let err = find("nonexistent").unwrap_err();
        assert!(matches!(err, RoleError::UnknownRole(_)));
    }
}
