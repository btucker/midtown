//! Headless clusterer for assigning tasks to topic channels.
//!
//! The clusterer is a resumable Claude session that accumulates context about
//! channel assignments over time. When a new task is created, the daemon sends
//! task info + current channel state, and the clusterer returns a JSON decision
//! about which channel to assign the task to.
//!
//! Unlike the architect (one-shot), the clusterer session persists across
//! invocations to maintain consistent channel grouping strategies.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tracing::{info, warn};

use super::specialized::{SpecializedCoworker, SpecializedRole};
use crate::auth::AuthProvider;
use crate::daemon::state::DaemonPersistentState;

/// Timeout for a single clusterer invocation.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Number of consecutive failures after which the clusterer session ID is cleared.
///
/// After this many failures in a row, the next invocation will spawn a fresh session
/// rather than continuing to retry a dead one.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// System prompt for the clusterer agent.
const CLUSTERER_SYSTEM_PROMPT: &str = include_str!("../../agents/clusterer.md");

/// JSON schema for clusterer response.
/// NOTE: Not currently used (JSON schema validation via claude --json-schema is TODO).
#[allow(dead_code)]
const CLUSTERER_SCHEMA: &str = include_str!("../clusterer_schema.json");

/// Input data sent to the clusterer for each task.
#[derive(Debug, Clone, Serialize)]
pub struct ClustererRequest {
    /// New task ID (e.g., "1234").
    pub task_id: String,
    /// New task subject.
    pub task_subject: String,
    /// New task description.
    pub task_description: String,
    /// Current channels with their state.
    pub channels: Vec<ChannelInfo>,
    /// Recently completed tasks (for archive/merge signals).
    pub recent_completions: Vec<CompletedTaskInfo>,
}

/// Information about a channel's current state.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelInfo {
    /// Channel name.
    pub name: String,
    /// Number of active (non-completed) tasks.
    pub active_task_count: usize,
    /// Recent task subjects in this channel (up to 3).
    pub recent_tasks: Vec<String>,
}

/// Information about a recently completed task.
#[derive(Debug, Clone, Serialize)]
pub struct CompletedTaskInfo {
    /// Task subject.
    pub subject: String,
    /// Channel it was assigned to.
    pub channel: Option<String>,
}

/// Clusterer's decision about channel operations.
///
/// Returns a full ClusteringDiff describing all channel operations to perform,
/// not just a single task assignment. This allows the clusterer to create,
/// archive, and merge channels as part of its decision.
pub type ClustererResponse = crate::clustering::ClusteringDiff;

/// Strip markdown code fences from a model response.
///
/// Models sometimes wrap JSON output in triple-backtick fences, e.g.:
/// ```json
/// { ... }
/// ```
///
/// This function returns the inner content when fences are present, or the
/// original string otherwise.
fn strip_markdown_fences(s: &str) -> &str {
    let trimmed = s.trim();
    // Match opening fence: ``` optionally followed by a language tag
    let after_open = if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on the opening line
        match rest.find('\n') {
            Some(newline_pos) => &rest[newline_pos + 1..],
            None => return s, // malformed — no newline after opening fence
        }
    } else {
        return s;
    };
    // Strip the closing fence
    match after_open.rfind("```") {
        Some(close_pos) => after_open[..close_pos].trim(),
        None => s, // no closing fence — return original
    }
}

/// Clusterer role implementation.
struct ClustererRole;

impl SpecializedRole for ClustererRole {
    type Request = ClustererRequest;
    type Response = ClustererResponse;

    fn role_name(&self) -> &'static str {
        "clusterer"
    }

    fn system_prompt(&self) -> String {
        CLUSTERER_SYSTEM_PROMPT.to_string()
    }

    fn model(&self) -> &str {
        "haiku"
    }

    fn persist_session(&self) -> bool {
        true // Resume-on-demand to accumulate context
    }

    fn max_budget_usd(&self) -> f64 {
        0.10 // Lightweight, fast decisions
    }

    fn allow_tools(&self) -> bool {
        false // No tools needed for clustering logic
    }

    fn request_timeout(&self) -> Duration {
        SESSION_TIMEOUT
    }

    fn format_request(&self, request: &Self::Request) -> String {
        // Send as pretty-printed JSON
        serde_json::to_string_pretty(request).unwrap_or_else(|e| {
            warn!("Failed to serialize clusterer request: {}", e);
            format!("{:?}", request)
        })
    }

    fn parse_response(&self, raw: &str) -> Result<Self::Response, String> {
        let stripped = strip_markdown_fences(raw);
        serde_json::from_str(stripped).map_err(|e| {
            format!(
                "Failed to parse clusterer response as JSON: {} (response: {})",
                e, raw
            )
        })
    }
}

/// Record a successful clusterer invocation.
///
/// Updates the session ID and resets the consecutive failure counter.
pub fn record_clusterer_success(ps: &mut DaemonPersistentState, session_id: Option<String>) {
    ps.clusterer_session_id = session_id;
    ps.clusterer_consecutive_failures = 0;
}

/// Record a failed clusterer invocation.
///
/// Increments the consecutive failure counter. When the counter reaches
/// `MAX_CONSECUTIVE_FAILURES`, clears the session ID so the next invocation
/// spawns a fresh session instead of retrying a dead one.
pub fn record_clusterer_failure(ps: &mut DaemonPersistentState) {
    ps.clusterer_consecutive_failures += 1;
    if ps.clusterer_consecutive_failures >= MAX_CONSECUTIVE_FAILURES
        && ps.clusterer_session_id.is_some()
    {
        warn!(
            "Clusterer: {} consecutive failures, clearing session ID to force fresh session on next attempt",
            ps.clusterer_consecutive_failures
        );
        ps.clusterer_session_id = None;
    }
}

/// Invoke the clusterer to assign a channel for a new task.
///
/// On first invocation, spawns a fresh session and saves the session ID.
/// On subsequent invocations, resumes the previous session to maintain context.
///
/// Returns a [`ClusteringDiff`](crate::clustering::ClusteringDiff) containing
/// channel operations (create, archive, merge) and task assignments, or an error.
pub async fn assign_channel(
    request: ClustererRequest,
    cwd: PathBuf,
    persistent_state: &mut DaemonPersistentState,
    auth_provider: AuthProvider,
    auth_profile_dir: &Path,
) -> Result<ClustererResponse, String> {
    let role = ClustererRole;
    let session_id = persistent_state.clusterer_session_id.clone();

    info!(
        "Clusterer: assigning channel for task '{}' (resume={})",
        request.task_subject,
        session_id.is_some()
    );

    let result = SpecializedCoworker::execute(
        &role,
        request,
        session_id,
        Some(cwd),
        None,
        auth_provider,
        auth_profile_dir,
    )
    .await;

    match result {
        Ok(result) => {
            info!(
                "Clusterer: returned {} creates, {} archives, {} merges, {} assignments (cost=${:.4}, duration={}ms, session_id={})",
                result.response.create_channels.len(),
                result.response.archive_channels.len(),
                result.response.merge_channels.len(),
                result.response.assign_tasks.len(),
                result.cost_usd,
                result.duration_ms,
                result.session_id.as_deref().unwrap_or("unknown"),
            );

            record_clusterer_success(persistent_state, result.session_id);

            Ok(result.response)
        }
        Err(e) => {
            let err_msg = format!("Clusterer execution failed: {}", e);
            warn!("{}", err_msg);
            record_clusterer_failure(persistent_state);
            Err(err_msg)
        }
    }
}

#[path = "clusterer_tests.rs"]
#[cfg(test)]
mod tests;
