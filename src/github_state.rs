//! Persistent GitHub state for the midtown daemon.
//!
//! Stores PR reviewer assignments in a JSON file that survives daemon restarts.
//! This prevents duplicate reviewer assignments and enables the web UI to show
//! which coworker is reviewing each PR.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use tracing::{debug, warn};

/// Persistent state for GitHub-related data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubState {
    /// Map of PR number -> reviewer assignment
    #[serde(default)]
    pub pr_reviewers: HashMap<u64, PrReviewerAssignment>,
}

/// A PR reviewer assignment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewerAssignment {
    /// PR number
    pub pr_number: u64,
    /// Coworker name assigned to review
    pub reviewer: String,
    /// When the assignment was made
    pub assigned_at: DateTime<Utc>,
}

impl GitHubState {
    /// Load state from a file, returning default if file doesn't exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse github-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                debug!(
                    "Loaded GitHub state with {} PR reviewers",
                    state.pr_reviewers.len()
                );
                Ok(state)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!("github-state.json not found, using defaults");
                Ok(Self::default())
            }
            Err(e) => Err(e),
        }
    }

    /// Save state to a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(path, contents)?;
        debug!(
            "Saved GitHub state with {} PR reviewers",
            self.pr_reviewers.len()
        );
        Ok(())
    }

    /// Assign a reviewer to a PR.
    pub fn assign_reviewer(&mut self, pr_number: u64, reviewer: &str) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            assigned_at: Utc::now(),
        };
        self.pr_reviewers.insert(pr_number, assignment);
    }

    /// Check if a PR has a reviewer assigned.
    pub fn get_reviewer(&self, pr_number: u64) -> Option<&str> {
        self.pr_reviewers
            .get(&pr_number)
            .map(|a| a.reviewer.as_str())
    }

    /// Check if a PR has been assigned for review.
    pub fn is_assigned(&self, pr_number: u64) -> bool {
        self.pr_reviewers.contains_key(&pr_number)
    }

    /// Remove a reviewer assignment (e.g., when PR is merged/closed).
    pub fn remove_assignment(&mut self, pr_number: u64) -> Option<PrReviewerAssignment> {
        self.pr_reviewers.remove(&pr_number)
    }

    /// Get all coworkers currently assigned to review PRs.
    pub fn assigned_reviewers(&self) -> impl Iterator<Item = &str> {
        self.pr_reviewers.values().map(|a| a.reviewer.as_str())
    }

    /// Get the PR number assigned to a specific reviewer.
    pub fn pr_for_reviewer(&self, reviewer: &str) -> Option<u64> {
        self.pr_reviewers
            .iter()
            .find(|(_, a)| a.reviewer == reviewer)
            .map(|(pr_number, _)| *pr_number)
    }

    /// Clean up assignments for PRs that are no longer open.
    ///
    /// Takes a list of open PR numbers and removes assignments for any PRs not in the list.
    pub fn cleanup_closed_prs(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        let to_remove: Vec<_> = self
            .pr_reviewers
            .keys()
            .filter(|pr| !open_set.contains(pr))
            .copied()
            .collect();

        for pr in to_remove {
            debug!("Cleaning up reviewer assignment for closed PR #{}", pr);
            self.pr_reviewers.remove(&pr);
        }
    }
}

/// Load GitHub state for the current repository.
pub fn load_state() -> io::Result<GitHubState> {
    let path = crate::paths::github_state_file();
    GitHubState::load(&path)
}

/// Save GitHub state for the current repository.
pub fn save_state(state: &GitHubState) -> io::Result<()> {
    let path = crate::paths::github_state_file();
    state.save(&path)
}

/// Load GitHub state for a specific repository.
pub fn load_state_for_repo(repo: &str) -> io::Result<GitHubState> {
    let path = crate::paths::github_state_file_for_repo(repo);
    GitHubState::load(&path)
}

/// Save GitHub state for a specific repository.
pub fn save_state_for_repo(repo: &str, state: &GitHubState) -> io::Result<()> {
    let path = crate::paths::github_state_file_for_repo(repo);
    state.save(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_state_default() {
        let state = GitHubState::default();
        assert!(state.pr_reviewers.is_empty());
    }

    #[test]
    fn test_assign_reviewer() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");

        assert!(state.is_assigned(42));
        assert_eq!(state.get_reviewer(42), Some("lexington"));
        assert!(!state.is_assigned(43));
    }

    #[test]
    fn test_remove_assignment() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");

        let removed = state.remove_assignment(42);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().reviewer, "lexington");
        assert!(!state.is_assigned(42));
    }

    #[test]
    fn test_assigned_reviewers() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        let reviewers: Vec<_> = state.assigned_reviewers().collect();
        assert_eq!(reviewers.len(), 2);
        assert!(reviewers.contains(&"lexington"));
        assert!(reviewers.contains(&"park"));
    }

    #[test]
    fn test_pr_for_reviewer() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        assert_eq!(state.pr_for_reviewer("lexington"), Some(42));
        assert_eq!(state.pr_for_reviewer("park"), Some(43));
        assert_eq!(state.pr_for_reviewer("york"), None);

        // After removal, should return None
        state.remove_assignment(42);
        assert_eq!(state.pr_for_reviewer("lexington"), None);
    }

    #[test]
    fn test_cleanup_closed_prs() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");
        state.assign_reviewer(44, "york");

        // Only PR 42 and 44 are still open
        state.cleanup_closed_prs(&[42, 44]);

        assert!(state.is_assigned(42));
        assert!(!state.is_assigned(43)); // cleaned up
        assert!(state.is_assigned(44));
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("github-state.json");

        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        state.save(&path).unwrap();

        let loaded = GitHubState::load(&path).unwrap();
        assert_eq!(loaded.pr_reviewers.len(), 2);
        assert_eq!(loaded.get_reviewer(42), Some("lexington"));
        assert_eq!(loaded.get_reviewer(43), Some("park"));
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");

        let state = GitHubState::load(&path).unwrap();
        assert!(state.pr_reviewers.is_empty());
    }
}
