//! Headless clusterer for assigning tasks to topic channels.
//!
//! The clusterer is a resumable Claude session that accumulates context about
//! channel assignments over time. When a new task is created, the daemon sends
//! task info + current channel state, and the clusterer returns a JSON decision
//! about which channel to assign the task to.
//!
//! Unlike the architect (one-shot), the clusterer session persists across
//! invocations to maintain consistent channel grouping strategies.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::specialized::{SpecializedCoworker, SpecializedRole};
use crate::daemon::state::DaemonPersistentState;

/// Timeout for a single clusterer invocation.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// System prompt for the clusterer agent.
const CLUSTERER_SYSTEM_PROMPT: &str = include_str!("../../agents/clusterer.md");

/// JSON schema for clusterer response.
/// NOTE: Not currently used (JSON schema validation via claude --json-schema is TODO).
#[allow(dead_code)]
const CLUSTERER_SCHEMA: &str = include_str!("../clusterer_schema.json");

/// Input data sent to the clusterer for each task.
#[derive(Debug, Clone, Serialize)]
pub struct ClustererRequest {
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

/// Clusterer's decision about channel assignment.
#[derive(Debug, Clone, Deserialize)]
pub struct ClustererResponse {
    /// Channel to assign the task to.
    pub channel: String,
    /// Rationale for the assignment.
    #[allow(dead_code)] // Logged but not currently used in logic
    pub rationale: String,
    /// Optional suggestions for channel maintenance.
    #[allow(dead_code)] // Logged but not currently acted upon
    #[serde(default)]
    pub suggestions: Vec<ChannelSuggestion>,
}

/// A suggestion for channel maintenance (archive or merge).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum ChannelSuggestion {
    Archive {
        channel: String,
        reason: String,
    },
    Merge {
        from: String,
        into: String,
        reason: String,
    },
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
        serde_json::from_str(raw).map_err(|e| {
            format!(
                "Failed to parse clusterer response as JSON: {} (response: {})",
                e, raw
            )
        })
    }
}

/// Invoke the clusterer to assign a channel for a new task.
///
/// On first invocation, spawns a fresh session and saves the session ID.
/// On subsequent invocations, resumes the previous session to maintain context.
///
/// Returns the channel assignment or an error.
pub async fn assign_channel(
    request: ClustererRequest,
    cwd: PathBuf,
    persistent_state: &mut DaemonPersistentState,
) -> Result<ClustererResponse, String> {
    let role = ClustererRole;
    let session_id = persistent_state.clusterer_session_id.clone();

    info!(
        "Clusterer: assigning channel for task '{}' (resume={})",
        request.task_subject,
        session_id.is_some()
    );

    let result = SpecializedCoworker::execute(&role, request, session_id, Some(cwd), None)
        .await
        .map_err(|e| format!("Clusterer execution failed: {}", e))?;

    info!(
        "Clusterer: assigned to '{}' (cost=${:.4}, duration={}ms, session_id={})",
        result.response.channel,
        result.cost_usd,
        result.duration_ms,
        result.session_id.as_deref().unwrap_or("unknown"),
    );

    // Save the session ID for next time
    persistent_state.clusterer_session_id = result.session_id;

    // Log any suggestions
    for suggestion in &result.response.suggestions {
        match suggestion {
            ChannelSuggestion::Archive { channel, reason } => {
                info!("Clusterer suggests archiving '{}': {}", channel, reason);
            }
            ChannelSuggestion::Merge { from, into, reason } => {
                info!(
                    "Clusterer suggests merging '{}' into '{}': {}",
                    from, into, reason
                );
            }
        }
    }

    Ok(result.response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clusterer_schema_parses() {
        let schema: Result<serde_json::Value, _> = serde_json::from_str(CLUSTERER_SCHEMA);
        assert!(schema.is_ok(), "clusterer schema should be valid JSON");
    }

    #[test]
    fn test_clusterer_request_serialization() {
        let request = ClustererRequest {
            task_subject: "Add auth endpoint".to_string(),
            task_description: "Implement JWT authentication".to_string(),
            channels: vec![
                ChannelInfo {
                    name: "auth".to_string(),
                    active_task_count: 2,
                    recent_tasks: vec!["Fix login bug".to_string()],
                },
                ChannelInfo {
                    name: "api".to_string(),
                    active_task_count: 1,
                    recent_tasks: vec![],
                },
            ],
            recent_completions: vec![CompletedTaskInfo {
                subject: "Update tests".to_string(),
                channel: Some("testing".to_string()),
            }],
        };

        let json = serde_json::to_string(&request);
        assert!(json.is_ok());
    }

    #[test]
    fn test_clusterer_response_deserialization() {
        let json = r#"{
            "channel": "auth-refactor",
            "rationale": "This task is related to authentication",
            "suggestions": [
                {
                    "action": "archive",
                    "channel": "old-auth",
                    "reason": "All tasks completed"
                },
                {
                    "action": "merge",
                    "from": "auth-v2",
                    "into": "auth",
                    "reason": "Same area of work"
                }
            ]
        }"#;

        let response: Result<ClustererResponse, _> = serde_json::from_str(json);
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.channel, "auth-refactor");
        assert_eq!(response.suggestions.len(), 2);
    }

    #[test]
    fn test_clusterer_response_minimal() {
        let json = r#"{
            "channel": "midtown",
            "rationale": "Meta work"
        }"#;

        let response: Result<ClustererResponse, _> = serde_json::from_str(json);
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.channel, "midtown");
        assert!(response.suggestions.is_empty());
    }

    #[test]
    fn test_clusterer_role_basics() {
        let role = ClustererRole;

        assert_eq!(role.role_name(), "clusterer");
        assert_eq!(role.model(), "haiku");
        assert!(role.persist_session());
        assert_eq!(role.max_budget_usd(), 0.10);
        assert!(!role.allow_tools());

        let request = ClustererRequest {
            task_subject: "Test task".to_string(),
            task_description: "Test description".to_string(),
            channels: vec![],
            recent_completions: vec![],
        };

        let formatted = role.format_request(&request);
        assert!(formatted.contains("Test task"));
        assert!(formatted.contains("Test description"));

        let valid_json = r#"{"channel": "test", "rationale": "because"}"#;
        let response = role.parse_response(valid_json);
        assert!(response.is_ok());

        let invalid_json = "not json";
        let err = role.parse_response(invalid_json);
        assert!(err.is_err());
    }
}
