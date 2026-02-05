//! Structured coworker workflow phases.
//!
//! Coworkers report their workflow phase and current task via daemon RPC
//! (`coworker.report-state`). The daemon stores state in memory and uses it
//! to update tmux tab names and web UI status.

use serde::{Deserialize, Serialize};

/// Workflow phases that map to tmux tab abbreviations.
///
/// Each variant has a fixed abbreviation used for the tmux tab display.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let json = serde_json::to_string(&WorkflowPhase::PullRequest).unwrap();
        assert_eq!(json, "\"pull_request\"");

        let parsed: WorkflowPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, WorkflowPhase::PullRequest);
    }
}
