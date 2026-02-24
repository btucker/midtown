//! Workflow event types for customizable channel workflows.
//!
//! Each channel can have a `workflow.py` script. The daemon invokes it via
//! `uv run` when relevant events occur. This module defines the event taxonomy
//! that gets passed to those scripts as JSON.
//!
//! ## Event contract
//!
//! Every event is serialized to JSON and passed to the workflow script via
//! `--event '{"type":"pr.opened", ...}'`. The `type` field drives `transitions`
//! triggers on the Python side:
//!
//! ```python
//! machine.trigger(event["type"].replace(".", "_"))
//! ```
//!
//! ## Event taxonomy
//!
//! Events are grouped by source:
//!
//! | Group | Events |
//! |-------|--------|
//! | task | `task.created`, `task.assigned`, `task.completed` |
//! | pr | `pr.opened`, `pr.approved`, `pr.changes_requested`, `pr.merged`, `pr.ci_passed`, `pr.ci_failed`, `pr.conflict` |
//! | coworker | `coworker.idle`, `coworker.stuck`, `coworker.message` |
//! | channel | `channel.message` |
//! | timer | `timer.tick` |

use serde::Serialize;

/// A domain event emitted by the daemon when something relevant happens in a channel.
///
/// Events are serialized as tagged JSON objects with a `"type"` discriminant:
///
/// ```json
/// {"type": "pr.opened", "channel": "proj-workflows", "task_id": "42", "pr_number": 123, "coworker": "lexington"}
/// {"type": "coworker.idle", "channel": "proj-workflows", "task_id": "37", "coworker": "lexington"}
/// {"type": "coworker.idle", "channel": "proj-workflows", "coworker": "lexington"}
/// {"type": "timer.tick", "channel": "proj-workflows"}
/// ```
///
/// The `channel` field is always present. `task_id` is present for events that
/// are associated with a specific task, and **omitted entirely** (not serialized
/// as `null`) when absent. Use `event.get("task_id")` in Python to test
/// presence; `event["task_id"]` will raise `KeyError` when the field is absent.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum WorkflowEvent {
    // ── Task lifecycle ────────────────────────────────────────────────────────
    /// A new task was created in the channel.
    #[serde(rename = "task.created")]
    TaskCreated {
        /// Channel the task belongs to.
        channel: String,
        /// The new task's ID.
        task_id: String,
        /// Task subject line.
        subject: String,
    },

    /// A coworker was assigned to (or claimed) a task.
    #[serde(rename = "task.assigned")]
    TaskAssigned {
        channel: String,
        task_id: String,
        /// The coworker who claimed the task.
        coworker: String,
    },

    /// A task was marked completed.
    #[serde(rename = "task.completed")]
    TaskCompleted {
        channel: String,
        task_id: String,
        /// The coworker who completed the task, if known.
        coworker: Option<String>,
    },

    // ── PR lifecycle ─────────────────────────────────────────────────────────
    /// A coworker opened a pull request for a task.
    #[serde(rename = "pr.opened")]
    PrOpened {
        channel: String,
        task_id: String,
        /// GitHub PR number.
        pr_number: u64,
        /// The coworker who opened the PR.
        coworker: String,
    },

    /// A PR received an approving review.
    #[serde(rename = "pr.approved")]
    PrApproved {
        channel: String,
        task_id: String,
        pr_number: u64,
    },

    /// A reviewer requested changes on a PR.
    #[serde(rename = "pr.changes_requested")]
    PrChangesRequested {
        channel: String,
        task_id: String,
        pr_number: u64,
    },

    /// A PR was merged.
    #[serde(rename = "pr.merged")]
    PrMerged {
        channel: String,
        task_id: String,
        pr_number: u64,
    },

    /// All CI checks on a PR passed.
    #[serde(rename = "pr.ci_passed")]
    PrCiPassed {
        channel: String,
        task_id: String,
        pr_number: u64,
    },

    /// A CI check on a PR failed.
    #[serde(rename = "pr.ci_failed")]
    PrCiFailed {
        channel: String,
        task_id: String,
        pr_number: u64,
        /// Name of the failing check, if available. Omitted when absent.
        #[serde(skip_serializing_if = "Option::is_none")]
        check_name: Option<String>,
    },

    /// A PR has a merge conflict.
    #[serde(rename = "pr.conflict")]
    PrConflict {
        channel: String,
        task_id: String,
        pr_number: u64,
    },

    // ── Coworker lifecycle ───────────────────────────────────────────────────
    /// A coworker finished its current turn and is now idle.
    ///
    /// This fires when health checks detect a coworker has become idle.
    /// `task_id` is `None` when the coworker has no current task.
    #[serde(rename = "coworker.idle")]
    CoworkerIdle {
        channel: String,
        /// Task the coworker was working on, if any. Omitted when absent.
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        coworker: String,
    },

    /// The daemon detected that a coworker appears stuck (no progress).
    #[serde(rename = "coworker.stuck")]
    CoworkerStuck {
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        coworker: String,
    },

    /// A coworker posted a message to the channel.
    #[serde(rename = "coworker.message")]
    CoworkerMessage {
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        coworker: String,
        /// The message content.
        message: String,
    },

    // ── Channel ──────────────────────────────────────────────────────────────
    /// A human (non-coworker) posted a message to the channel.
    #[serde(rename = "channel.message")]
    ChannelMessage {
        channel: String,
        /// Message author.
        sender: String,
        /// The message content.
        message: String,
    },

    // ── Timer ────────────────────────────────────────────────────────────────
    /// Periodic fallback tick for reconciliation.
    ///
    /// Emitted on each `TaskDispatchTick` even when no specific event fired.
    /// Lets workflow scripts do periodic bookkeeping (e.g. nudge stuck tasks,
    /// check deadlines) without requiring a dedicated trigger event.
    #[serde(rename = "timer.tick")]
    TimerTick { channel: String },
}

impl WorkflowEvent {
    /// Returns the channel this event belongs to.
    pub fn channel(&self) -> &str {
        match self {
            Self::TaskCreated { channel, .. }
            | Self::TaskAssigned { channel, .. }
            | Self::TaskCompleted { channel, .. }
            | Self::PrOpened { channel, .. }
            | Self::PrApproved { channel, .. }
            | Self::PrChangesRequested { channel, .. }
            | Self::PrMerged { channel, .. }
            | Self::PrCiPassed { channel, .. }
            | Self::PrCiFailed { channel, .. }
            | Self::PrConflict { channel, .. }
            | Self::CoworkerIdle { channel, .. }
            | Self::CoworkerStuck { channel, .. }
            | Self::CoworkerMessage { channel, .. }
            | Self::ChannelMessage { channel, .. }
            | Self::TimerTick { channel } => channel,
        }
    }

    /// Returns the task ID associated with this event, if any.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::TaskCreated { task_id, .. }
            | Self::TaskAssigned { task_id, .. }
            | Self::TaskCompleted { task_id, .. }
            | Self::PrOpened { task_id, .. }
            | Self::PrApproved { task_id, .. }
            | Self::PrChangesRequested { task_id, .. }
            | Self::PrMerged { task_id, .. }
            | Self::PrCiPassed { task_id, .. }
            | Self::PrCiFailed { task_id, .. }
            | Self::PrConflict { task_id, .. } => Some(task_id),
            Self::CoworkerIdle { task_id, .. }
            | Self::CoworkerStuck { task_id, .. }
            | Self::CoworkerMessage { task_id, .. } => task_id.as_deref(),
            Self::ChannelMessage { .. } | Self::TimerTick { .. } => None,
        }
    }
}

#[path = "workflow_tests.rs"]
#[cfg(test)]
mod tests;
