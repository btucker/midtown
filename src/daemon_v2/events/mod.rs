mod store;

pub use store::EventStore;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type AgentId = String;
pub type TaskId = String;
pub type MessageId = String;
pub type WorktreeId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Lead,
    Fork,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiStatus {
    Pending,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewState {
    None,
    Pending,
    Approved,
    ChangesRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessStatus {
    Running,
    Stopped,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomainEvent {
    // Agents
    AgentCreated {
        id: AgentId,
        name: String,
        kind: AgentKind,
        agent_type: String,
        provider: Provider,
        channel: Option<String>,
        task_id: Option<TaskId>,
    },
    AgentStarted {
        id: AgentId,
        pid: u32,
    },
    AgentStopped {
        id: AgentId,
        reason: String,
    },
    AgentResumed {
        id: AgentId,
    },

    // Tasks
    TaskCreated {
        id: TaskId,
        subject: String,
        channel: String,
        blocked_by: Vec<TaskId>,
    },
    TaskAssigned {
        task_id: TaskId,
        agent_id: AgentId,
    },
    TaskCompleted {
        task_id: TaskId,
    },
    TaskReset {
        task_id: TaskId,
        reason: String,
    },
    TaskUnblocked {
        task_id: TaskId,
    },

    // PRs
    PrOpened {
        number: u64,
        branch: String,
        author: String,
    },
    PrUpdated {
        number: u64,
        ci_status: CiStatus,
        review_state: ReviewState,
    },
    PrMerged {
        number: u64,
        branch: String,
    },
    PrClosed {
        number: u64,
    },
    PrReviewRequested {
        number: u64,
    },
    PrLinkedToTask {
        number: u64,
        task_id: TaskId,
    },

    // Chat
    MessagePosted {
        id: MessageId,
        channel: String,
        sender: String,
        content: String,
        thread_id: Option<String>,
    },
    MentionRouted {
        message_id: MessageId,
        target_agent: AgentId,
    },

    // Health
    ProcessHealthChecked {
        agent_id: AgentId,
        status: ProcessStatus,
    },
    UsageLimitHit {
        agent_id: AgentId,
        reset_at: DateTime<Utc>,
    },
    AuthErrorDetected {
        agent_id: AgentId,
    },

    // Worktrees
    WorktreeCreated {
        id: WorktreeId,
        path: PathBuf,
        task_id: Option<TaskId>,
    },
    WorktreeRemoved {
        id: WorktreeId,
    },

    // Config
    ConfigUpdated {
        key: String,
        value: serde_json::Value,
    },
}
