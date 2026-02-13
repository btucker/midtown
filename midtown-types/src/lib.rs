//! Shared types between the Midtown daemon and Zellij plugin.
//!
//! These types define the RPC contract for plugin ↔ daemon communication.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Complete dashboard state returned by `plugin.dashboard` RPC.
///
/// The plugin polls this once per second to keep its UI current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardState {
    pub tasks: Vec<TaskSummary>,
    pub coworkers: Vec<CoworkerSummary>,
    pub channel_messages: Vec<ChannelMessage>,
    pub lead_nudge_queue: Vec<String>,
    pub daemon_version: String,
}

/// Summary of a task for the plugin sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub pr_number: Option<u64>,
    pub pr_status: Option<String>,
}

/// Summary of a coworker for the plugin sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoworkerSummary {
    pub name: String,
    pub status: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub is_alive: bool,
    pub has_usage_limit: bool,
    pub has_api_error: bool,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// A channel message for the plugin channel view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub sender: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub message_type: String,
}

/// Request to attach to a coworker's session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachRequest {
    pub coworker_name: String,
    /// If true, stop the coworker immediately without waiting for turn completion.
    pub force: bool,
}

/// Response from an attach request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachResponse {
    pub success: bool,
    /// The Claude Code session ID to resume with `claude --resume <id>`.
    pub session_id: Option<String>,
    pub error: Option<String>,
}

/// Recent streaming output from a headless coworker (for read-only view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoworkerStreamOutput {
    pub coworker_name: String,
    pub events: Vec<StreamEvent>,
}

/// A single event from a coworker's JSON stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub content: String,
}
