//! Structured coworker state reporting.
//!
//! Coworkers report their workflow phase and current task via daemon RPC
//! (`coworker.report-state`). The daemon stores state in memory and uses it
//! to update tmux tab names and web UI status without parsing freeform `/me`
//! channel messages.
//!
//! Legacy file-based state (`~/.midtown/coworkers/<repo>/<name>/state.json`)
//! is retained as a fallback when the daemon is unreachable and for migration.
//!
//! The `/me` messages remain in the channel for human-readable history, but
//! state decisions and display are driven by structured data from the daemon.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Structured state report from a coworker.
///
/// Written atomically by hooks, read by the daemon to update status displays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoworkerStateReport {
    /// Current workflow phase.
    pub phase: WorkflowPhase,
    /// Task number being worked on (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u32>,
    /// When this state was last written.
    pub updated_at: DateTime<Utc>,
}

/// Workflow phases that map to tmux tab abbreviations.
///
/// These replace the keyword-matching in `parse_status()`. Each variant
/// has a fixed abbreviation used for the tmux tab display.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    /// Claiming a task ("claim")
    Claiming,
    /// Actively developing ("dev")
    Developing,
    /// Running tests ("test")
    Testing,
    /// Opening or requesting PR review ("PR")
    PullRequest,
    /// Reviewing someone else's PR ("review")
    Reviewing,
    /// Debugging / investigating ("debug")
    Debugging,
    /// Task completed ("done")
    Completed,
    /// Idle / waiting / blocked ("idle")
    Idle,
}

impl WorkflowPhase {
    /// Get the tmux tab abbreviation for this phase.
    ///
    /// These match the abbreviations previously produced by `parse_status()`.
    pub fn abbreviation(&self) -> &'static str {
        match self {
            Self::Claiming => "claim",
            Self::Developing => "dev",
            Self::Testing => "test",
            Self::PullRequest => "PR",
            Self::Reviewing => "review",
            Self::Debugging => "debug",
            Self::Completed => "done",
            Self::Idle => "idle",
        }
    }
}

impl CoworkerStateReport {
    /// Create a new state report with a phase and optional task.
    pub fn new(phase: WorkflowPhase, task_id: Option<u32>) -> Self {
        Self {
            phase,
            task_id,
            updated_at: Utc::now(),
        }
    }

    /// Format for tmux tab display: "phase" or "phase#task_id".
    ///
    /// Examples: "dev#42", "idle", "PR#7", "claim#13"
    ///
    /// Note: Task ID 0 is treated as "no task" since it's often used as a
    /// placeholder for taskless work (e.g., PR reviews without a formal task).
    pub fn display_status(&self) -> String {
        match self.task_id {
            Some(id) if id > 0 => format!("{}#{}", self.phase.abbreviation(), id),
            _ => self.phase.abbreviation().to_string(),
        }
    }
}

/// Get the state file path for a coworker.
///
/// Returns `~/.midtown/coworkers/<repo>/<name>/state.json`.
pub fn state_file_path(repo: &str, coworker_name: &str) -> PathBuf {
    crate::paths::coworkers_dir_for_repo(repo)
        .join(coworker_name)
        .join("state.json")
}

/// Read a coworker's state report from disk.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
pub fn read_state(repo: &str, coworker_name: &str) -> Option<CoworkerStateReport> {
    let path = state_file_path(repo, coworker_name);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write a coworker's state report to disk atomically.
///
/// Uses write-to-temp + rename to avoid partial reads by the daemon.
pub fn write_state(
    repo: &str,
    coworker_name: &str,
    report: &CoworkerStateReport,
) -> Result<(), std::io::Error> {
    let path = state_file_path(repo, coworker_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string(report).map_err(std::io::Error::other)?;

    // Atomic write: temp file + rename
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, &path)?;

    Ok(())
}

/// Read state reports for all coworkers in a repo.
///
/// Returns a map of coworker name → state report.
pub fn read_all_states(
    repo: &str,
    coworker_names: &[String],
) -> std::collections::HashMap<String, CoworkerStateReport> {
    coworker_names
        .iter()
        .filter_map(|name| read_state(repo, name).map(|report| (name.clone(), report)))
        .collect()
}

/// Clear a coworker's state file (e.g., on shutdown).
pub fn clear_state(repo: &str, coworker_name: &str) {
    let path = state_file_path(repo, coworker_name);
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_status_with_task() {
        let report = CoworkerStateReport::new(WorkflowPhase::Developing, Some(42));
        assert_eq!(report.display_status(), "dev#42");
    }

    #[test]
    fn test_display_status_without_task() {
        let report = CoworkerStateReport::new(WorkflowPhase::Idle, None);
        assert_eq!(report.display_status(), "idle");
    }

    #[test]
    fn test_all_phase_abbreviations() {
        assert_eq!(WorkflowPhase::Claiming.abbreviation(), "claim");
        assert_eq!(WorkflowPhase::Developing.abbreviation(), "dev");
        assert_eq!(WorkflowPhase::Testing.abbreviation(), "test");
        assert_eq!(WorkflowPhase::PullRequest.abbreviation(), "PR");
        assert_eq!(WorkflowPhase::Reviewing.abbreviation(), "review");
        assert_eq!(WorkflowPhase::Debugging.abbreviation(), "debug");
        assert_eq!(WorkflowPhase::Completed.abbreviation(), "done");
        assert_eq!(WorkflowPhase::Idle.abbreviation(), "idle");
    }

    #[test]
    fn test_serialize_roundtrip() {
        let report = CoworkerStateReport::new(WorkflowPhase::PullRequest, Some(7));
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"pull_request\""));
        assert!(json.contains("\"task_id\":7"));

        let parsed: CoworkerStateReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.phase, WorkflowPhase::PullRequest);
        assert_eq!(parsed.task_id, Some(7));
        assert_eq!(parsed.display_status(), "PR#7");
    }

    #[test]
    fn test_serialize_idle_no_task() {
        let report = CoworkerStateReport::new(WorkflowPhase::Idle, None);
        let json = serde_json::to_string(&report).unwrap();
        // task_id should be omitted when None (skip_serializing_if)
        assert!(!json.contains("task_id"));

        let parsed: CoworkerStateReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.phase, WorkflowPhase::Idle);
        assert_eq!(parsed.task_id, None);
    }

    #[test]
    fn test_display_status_with_task_zero_omits_number() {
        // Task ID 0 is used as a placeholder for taskless work (e.g., PR reviews
        // without a formal task assignment). It should display without the "#0"
        // suffix to avoid confusing window names like "PR#0".
        let report = CoworkerStateReport::new(WorkflowPhase::PullRequest, Some(0));
        // Should show "PR" not "PR#0"
        assert_eq!(report.display_status(), "PR");
    }
}
