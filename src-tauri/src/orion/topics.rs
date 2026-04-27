//! String-form view of canonical bus topics.
//!
//! Topics are an enum: see `crate::orion::bus::Topic`. The constants in this
//! module are derived from `Topic::as_str()` so legacy code paths (e.g.
//! persisted `Message.topic: String` fields, audit logs, debug renders) can
//! reference them by name without losing the canonical vocabulary.
//!
//! Adding a topic requires an ADR (see ADR-001 and CLAUDE.md). Do not pass
//! raw topic strings at call sites — import from here, or better, use the
//! `Topic` enum directly.
//!
//! Legacy variants `USER_CHAT_INTERRUPT` and `AGENT_TASK_*` were dropped
//! when the bus consolidated to the canonical 8-topic vocabulary. They
//! return as ADR-gated topic additions when those features re-land.

#![allow(dead_code)]

pub const MENTOR_INPUT: &str = "mentor.input";
pub const ID_STIMULUS: &str = "id.stimulus";
pub const ID_REACTION: &str = "id.reaction";
pub const EGO_DELIBERATION: &str = "ego.deliberation";
pub const EGO_ACTION: &str = "ego.action";
pub const SUPEREGO_LOCAL_EVALUATION: &str = "superego.local.evaluation";
pub const EGRESS_OUTBOUND: &str = "egress.outbound";
pub const GOVERNANCE_INBOUND: &str = "governance.inbound";

pub const ALL_TOPICS: &[&str] = &[
    MENTOR_INPUT,
    ID_STIMULUS,
    ID_REACTION,
    EGO_DELIBERATION,
    EGO_ACTION,
    SUPEREGO_LOCAL_EVALUATION,
    EGRESS_OUTBOUND,
    GOVERNANCE_INBOUND,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::bus::Topic;

    #[test]
    fn topic_constants_match_enum_as_str() {
        assert_eq!(MENTOR_INPUT, Topic::MentorInput.as_str());
        assert_eq!(ID_STIMULUS, Topic::IdStimulus.as_str());
        assert_eq!(ID_REACTION, Topic::IdReaction.as_str());
        assert_eq!(EGO_DELIBERATION, Topic::EgoDeliberation.as_str());
        assert_eq!(EGO_ACTION, Topic::EgoAction.as_str());
        assert_eq!(
            SUPEREGO_LOCAL_EVALUATION,
            Topic::SuperegoLocalEvaluation.as_str()
        );
        assert_eq!(EGRESS_OUTBOUND, Topic::EgressOutbound.as_str());
        assert_eq!(GOVERNANCE_INBOUND, Topic::GovernanceInbound.as_str());
    }

    #[test]
    fn all_topics_covers_every_enum_variant() {
        assert_eq!(ALL_TOPICS.len(), Topic::ALL.len());
        for topic in Topic::ALL {
            assert!(ALL_TOPICS.contains(&topic.as_str()));
        }
    }
}
