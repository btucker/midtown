pub mod chat;
pub mod dispatch;
pub mod health;
pub mod prs;

use crate::daemon_v2::events::{AgentId, AgentKind, Provider, TaskId};

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub name: String,
    pub kind: AgentKind,
    pub agent_type: String,
    pub provider: Provider,
    pub channel: Option<String>,
    pub task_id: Option<TaskId>,
    pub initial_prompt: Option<String>,
    pub working_dir: Option<String>,
    pub model: Option<String>,
    pub bound_thread_id: Option<String>,
    /// When set, fork from the given parent session ID using `--fork-session`.
    /// The spawned session inherits the parent's conversation context.
    pub fork_from_session: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    SpawnAgent(SpawnConfig),
    StopAgent {
        id: AgentId,
        reason: String,
    },
    ResumeAgent {
        id: AgentId,
    },
    NudgeAgent {
        id: AgentId,
        message: String,
    },
    AssignTask {
        task_id: TaskId,
        agent_id: AgentId,
    },
    CompleteTask {
        task_id: TaskId,
    },
    ResetTask {
        task_id: TaskId,
    },
    Post {
        channel: String,
        sender: String,
        content: String,
        thread_id: Option<String>,
    },
    PostSystem {
        channel: String,
        content: String,
    },
    PollProcessHealth,
    PollPrs,
    CreateWorktree {
        task_id: TaskId,
        branch: String,
    },
    RemoveWorktree {
        task_id: TaskId,
    },
}
