pub const USER_CHAT_INPUT: &str = "user.chat.input";
pub const USER_CHAT_OUTPUT: &str = "user.chat.output";
pub const USER_CHAT_INTERRUPT: &str = "user.chat.interrupt";

pub const EGO_INSTRUCTIONS: &str = "ego.instructions";
pub const EGO_CHECKPOINTS: &str = "ego.checkpoints";
pub const EGO_RESULTS: &str = "ego.results";

pub const AGENT_TASK_ASSIGNED: &str = "agent.task.assigned";
pub const AGENT_TASK_PROGRESS: &str = "agent.task.progress";
pub const AGENT_TASK_COMPLETED: &str = "agent.task.completed";
pub const AGENT_TASK_FAILED: &str = "agent.task.failed";

pub const SAO_EGRESS: &str = "sao.egress";

pub const ALL_TOPICS: &[&str] = &[
    USER_CHAT_INPUT,
    USER_CHAT_OUTPUT,
    USER_CHAT_INTERRUPT,
    EGO_INSTRUCTIONS,
    EGO_CHECKPOINTS,
    EGO_RESULTS,
    AGENT_TASK_ASSIGNED,
    AGENT_TASK_PROGRESS,
    AGENT_TASK_COMPLETED,
    AGENT_TASK_FAILED,
    SAO_EGRESS,
];
