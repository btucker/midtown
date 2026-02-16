//! Structured coworker workflow phases.
//!
//! Coworkers report their workflow phase and current task via daemon RPC
//! (`coworker.report-state`). The daemon stores state in memory and uses it
//! to update web UI status.

use serde::{Deserialize, Serialize};

/// Workflow phases for coworker status tracking.
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
    /// Get the short abbreviation for this phase.
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

impl std::str::FromStr for WorkflowPhase {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claiming" => Ok(Self::Claiming),
            "developing" => Ok(Self::Developing),
            "testing" => Ok(Self::Testing),
            "pull_request" | "pull-request" => Ok(Self::PullRequest),
            "reviewing" => Ok(Self::Reviewing),
            "debugging" => Ok(Self::Debugging),
            "completed" => Ok(Self::Completed),
            "idle" => Ok(Self::Idle),
            _ => Err(format!("Unknown phase: {}", s)),
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

    #[test]
    fn test_from_str_all_phases() {
        assert_eq!(
            "claiming".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Claiming
        );
        assert_eq!(
            "developing".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Developing
        );
        assert_eq!(
            "testing".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Testing
        );
        assert_eq!(
            "pull_request".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::PullRequest
        );
        assert_eq!(
            "pull-request".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::PullRequest
        );
        assert_eq!(
            "reviewing".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Reviewing
        );
        assert_eq!(
            "debugging".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Debugging
        );
        assert_eq!(
            "completed".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Completed
        );
        assert_eq!(
            "idle".parse::<WorkflowPhase>().unwrap(),
            WorkflowPhase::Idle
        );
    }

    #[test]
    fn test_from_str_invalid() {
        assert!("unknown".parse::<WorkflowPhase>().is_err());
        assert!("".parse::<WorkflowPhase>().is_err());
    }
}
