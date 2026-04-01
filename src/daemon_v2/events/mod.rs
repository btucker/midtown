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
        bound_thread_id: Option<String>,
        icon: Option<String>,
        color: Option<String>,
    },
    AgentStarted {
        id: AgentId,
        pid: u32,
        session_id: Option<String>,
    },
    AgentStopped {
        id: AgentId,
        reason: String,
    },
    AgentResumed {
        id: AgentId,
        pid: u32,
    },
    AgentGarbageCollected {
        id: AgentId,
    },
    AgentStateReported {
        id: AgentId,
        state: String,
    },
    AgentSpawnFailed {
        name: String,
        agent_type: String,
        reason: String,
    },
    AgentStopFailed {
        id: AgentId,
        reason: String,
    },

    // Tasks
    TaskCreated {
        id: TaskId,
        subject: String,
        channel: String,
        blocked_by: Vec<TaskId>,
        agent_type: Option<String>,
        agent_name: Option<String>,
        icon: Option<String>,
        color: Option<String>,
        parent: Option<TaskId>,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        message_id: Option<String>,
    },
    TaskUpdated {
        task_id: TaskId,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        message_id: Option<String>,
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
        #[serde(alias = "author")]
        github_author: String,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_data: Option<Vec<crate::message::ToolBlock>>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto_output: bool,
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

    // Reminders
    ReminderCreated {
        id: String,
        trigger: String,
        message: String,
        cron_expr: Option<String>,
    },
    ReminderCancelled {
        id: String,
    },

    // Workflows
    WorkflowStateSet {
        channel: String,
        key: String,
        state: String,
    },

    // Channel settings
    ChannelLeadDrivenSet {
        channel: String,
        lead_driven: bool,
    },

    ChannelDirectorySet {
        channel: String,
        directory: Option<String>,
    },

    // Config
    ConfigUpdated {
        key: String,
        value: serde_json::Value,
    },
}
