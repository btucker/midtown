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

/// How long a review assignment is valid before it expires (10 minutes).
/// Mirrors PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS from the in-memory tracker.
pub const PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS: u64 = 600;

/// A pending review spawn scheduled by a webhook event.
///
/// Instead of fire-and-forget `tokio::spawn` + `sleep`, we persist these so they
/// survive daemon restarts. The daemon tick loop drains ready entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReviewSpawn {
    pub pr_number: u64,
    pub spawn_after: DateTime<Utc>,
}

/// Persistent state for GitHub-related data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubState {
    /// Map of PR number -> reviewer assignment
    #[serde(default)]
    pub pr_reviewers: HashMap<u64, PrReviewerAssignment>,

    /// Set of PR numbers that have a confirmed Claude review.
    /// Review status is monotonic — once a PR has a review, it never loses it.
    /// This cache eliminates redundant `gh pr view` calls on every poll cycle.
    #[serde(default)]
    pub reviewed_prs: std::collections::HashSet<u64>,

    /// Webhook-triggered review spawns waiting for their delay to elapse.
    /// Persisted so they survive daemon restarts.
    #[serde(default)]
    pub pending_review_spawns: Vec<PendingReviewSpawn>,
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

    /// Check if a PR has been assigned for review and the assignment hasn't expired.
    pub fn is_assigned(&self, pr_number: u64) -> bool {
        match self.pr_reviewers.get(&pr_number) {
            Some(assignment) => {
                let elapsed = Utc::now().signed_duration_since(assignment.assigned_at);
                elapsed < chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64)
            }
            None => false,
        }
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

    /// Clean up assignments that have expired (older than timeout).
    pub fn cleanup_expired_assignments(&mut self) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        let to_remove: Vec<_> = self
            .pr_reviewers
            .iter()
            .filter(|(_, a)| now.signed_duration_since(a.assigned_at) > timeout)
            .map(|(pr, _)| *pr)
            .collect();

        for pr in to_remove {
            debug!(
                "Cleaning up expired reviewer assignment for PR #{} (timed out)",
                pr
            );
            self.pr_reviewers.remove(&pr);
        }
    }

    /// Clean up expired assignments, but preserve those for active coworkers.
    ///
    /// Same as `cleanup_expired_assignments` but skips removal of assignments
    /// where the reviewer coworker is still running. This prevents losing track
    /// of a reviewer just because the review is taking longer than the timeout.
    /// Active coworkers' assignments are refreshed to the current time.
    pub fn cleanup_expired_preserving(
        &mut self,
        active_coworkers: &std::collections::HashSet<String>,
    ) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        let to_remove: Vec<_> = self
            .pr_reviewers
            .iter()
            .filter(|(_, a)| {
                now.signed_duration_since(a.assigned_at) > timeout
                    && !active_coworkers.contains(&a.reviewer)
            })
            .map(|(pr, _)| *pr)
            .collect();

        for pr in to_remove {
            debug!(
                "Cleaning up expired reviewer assignment for PR #{} (timed out, coworker inactive)",
                pr
            );
            self.pr_reviewers.remove(&pr);
        }

        // Refresh timestamps for active coworkers whose assignments would have expired
        for assignment in self.pr_reviewers.values_mut() {
            if now.signed_duration_since(assignment.assigned_at) > timeout
                && active_coworkers.contains(&assignment.reviewer)
            {
                assignment.assigned_at = now;
            }
        }
    }

    /// Check if a PR has a cached Claude review result.
    pub fn has_cached_review(&self, pr_number: u64) -> bool {
        self.reviewed_prs.contains(&pr_number)
    }

    /// Mark a PR as having a Claude review (cache it permanently).
    pub fn mark_reviewed_pr(&mut self, pr_number: u64) {
        self.reviewed_prs.insert(pr_number);
    }

    /// Count active (non-expired) review assignments.
    pub fn active_count(&self) -> usize {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .values()
            .filter(|a| now.signed_duration_since(a.assigned_at) < timeout)
            .count()
    }

    /// Get the set of coworker names with active (non-expired) review assignments.
    pub fn active_reviewers(&self) -> std::collections::HashSet<String> {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .values()
            .filter(|a| now.signed_duration_since(a.assigned_at) < timeout)
            .map(|a| a.reviewer.clone())
            .collect()
    }

    /// Get all active (non-expired) review assignments.
    pub fn active_assignments(&self) -> HashMap<u64, PrReviewerAssignment> {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .iter()
            .filter(|(_, a)| now.signed_duration_since(a.assigned_at) < timeout)
            .map(|(pr, a)| (*pr, a.clone()))
            .collect()
    }

    /// Clean up review cache entries for PRs that are no longer open.
    fn cleanup_closed_review_cache(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        self.reviewed_prs.retain(|pr| open_set.contains(pr));
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

        // Also clean up review cache for closed PRs
        self.cleanup_closed_review_cache(open_pr_numbers);
    }

    /// Schedule a review spawn after a delay (in seconds from now).
    ///
    /// Deduplicates: if a spawn for this PR is already pending, it is not added again.
    pub fn schedule_review_spawn(&mut self, pr_number: u64, delay_secs: u64) {
        if self
            .pending_review_spawns
            .iter()
            .any(|p| p.pr_number == pr_number)
        {
            debug!(
                "Review spawn for PR #{} already pending, skipping duplicate",
                pr_number
            );
            return;
        }
        let spawn_after = Utc::now() + chrono::Duration::seconds(delay_secs as i64);
        self.pending_review_spawns.push(PendingReviewSpawn {
            pr_number,
            spawn_after,
        });
        debug!(
            "Scheduled review spawn for PR #{} at {}",
            pr_number, spawn_after
        );
    }

    /// Drain and return PR numbers whose delay has elapsed.
    pub fn take_ready_review_spawns(&mut self) -> Vec<u64> {
        let now = Utc::now();
        let (ready, pending): (Vec<_>, Vec<_>) = self
            .pending_review_spawns
            .drain(..)
            .partition(|p| now >= p.spawn_after);
        self.pending_review_spawns = pending;
        ready.into_iter().map(|p| p.pr_number).collect()
    }

    /// Remove pending review spawns for PRs that are no longer open.
    pub fn cleanup_pending_spawns(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        self.pending_review_spawns
            .retain(|p| open_set.contains(&p.pr_number));
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

    #[test]
    fn test_is_assigned_expires_after_timeout() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");

        // Fresh assignment should be considered assigned
        assert!(state.is_assigned(42));

        // Manually backdate the assignment to exceed the timeout
        if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
            assignment.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        // Expired assignment should NOT be considered assigned
        assert!(
            !state.is_assigned(42),
            "Expired persistent assignment should not be considered assigned"
        );
    }

    #[test]
    fn test_cleanup_expired_assignments() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        // Backdate PR 42's assignment past the timeout
        if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
            assignment.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        state.cleanup_expired_assignments();

        // PR 42 should be removed (expired), PR 43 should remain (fresh)
        assert!(!state.pr_reviewers.contains_key(&42));
        assert!(state.pr_reviewers.contains_key(&43));
    }

    #[test]
    fn test_cleanup_expired_preserves_active_coworkers() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "broadway");
        state.assign_reviewer(43, "park");

        // Backdate broadway's assignment past the timeout
        if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
            assignment.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        // broadway is still active
        let active: std::collections::HashSet<String> =
            ["broadway".to_string()].into_iter().collect();

        state.cleanup_expired_preserving(&active);

        // broadway's expired assignment should be preserved (still active coworker)
        assert!(state.pr_reviewers.contains_key(&42));
        // park's fresh assignment should also be there
        assert!(state.pr_reviewers.contains_key(&43));
    }

    #[test]
    fn test_cleanup_expired_removes_inactive_expired() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "broadway");

        // Backdate assignment past timeout
        if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
            assignment.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        // broadway is NOT active
        let active: std::collections::HashSet<String> = std::collections::HashSet::new();

        state.cleanup_expired_preserving(&active);

        // Should be removed (expired + inactive)
        assert!(!state.pr_reviewers.contains_key(&42));
    }

    #[test]
    fn test_active_count() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        assert_eq!(state.active_count(), 2);

        // Expire one assignment
        if let Some(a) = state.pr_reviewers.get_mut(&42) {
            a.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        assert_eq!(state.active_count(), 1);
    }

    #[test]
    fn test_active_reviewers() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");
        state.assign_reviewer(44, "lexington"); // duplicate reviewer name

        let reviewers = state.active_reviewers();
        assert!(reviewers.contains("lexington"));
        assert!(reviewers.contains("park"));
        assert_eq!(reviewers.len(), 2); // deduped

        // Expire lexington's assignment on PR 44
        if let Some(a) = state.pr_reviewers.get_mut(&44) {
            a.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        // lexington still has PR 42 (fresh)
        let reviewers = state.active_reviewers();
        assert!(reviewers.contains("lexington"));
        assert!(reviewers.contains("park"));
    }

    #[test]
    fn test_active_assignments() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington");
        state.assign_reviewer(43, "park");

        let assignments = state.active_assignments();
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[&42].reviewer, "lexington");
        assert_eq!(assignments[&43].reviewer, "park");

        // Expire one
        if let Some(a) = state.pr_reviewers.get_mut(&42) {
            a.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }

        let assignments = state.active_assignments();
        assert_eq!(assignments.len(), 1);
        assert!(!assignments.contains_key(&42));
        assert!(assignments.contains_key(&43));
    }

    #[test]
    fn test_schedule_review_spawn() {
        let mut state = GitHubState::default();
        state.schedule_review_spawn(42, 60);
        assert_eq!(state.pending_review_spawns.len(), 1);
        assert_eq!(state.pending_review_spawns[0].pr_number, 42);
    }

    #[test]
    fn test_schedule_review_spawn_dedup() {
        let mut state = GitHubState::default();
        state.schedule_review_spawn(42, 60);
        state.schedule_review_spawn(42, 60); // duplicate
        assert_eq!(state.pending_review_spawns.len(), 1);
    }

    #[test]
    fn test_take_ready_review_spawns() {
        let mut state = GitHubState::default();

        // Schedule one that's already past due
        state.pending_review_spawns.push(PendingReviewSpawn {
            pr_number: 10,
            spawn_after: Utc::now() - chrono::Duration::seconds(1),
        });
        // Schedule one that's still in the future
        state.pending_review_spawns.push(PendingReviewSpawn {
            pr_number: 20,
            spawn_after: Utc::now() + chrono::Duration::seconds(3600),
        });

        let ready = state.take_ready_review_spawns();
        assert_eq!(ready, vec![10]);
        // The future one should remain
        assert_eq!(state.pending_review_spawns.len(), 1);
        assert_eq!(state.pending_review_spawns[0].pr_number, 20);
    }

    #[test]
    fn test_cleanup_pending_spawns_removes_closed() {
        let mut state = GitHubState::default();
        state.schedule_review_spawn(10, 60);
        state.schedule_review_spawn(20, 60);
        state.schedule_review_spawn(30, 60);

        // Only PR 10 and 30 are still open
        state.cleanup_pending_spawns(&[10, 30]);

        let prs: Vec<u64> = state
            .pending_review_spawns
            .iter()
            .map(|p| p.pr_number)
            .collect();
        assert_eq!(prs, vec![10, 30]);
    }

    #[test]
    fn test_pending_review_spawns_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("github-state.json");

        let mut state = GitHubState::default();
        state.schedule_review_spawn(42, 60);
        state.save(&path).unwrap();

        let loaded = GitHubState::load(&path).unwrap();
        assert_eq!(loaded.pending_review_spawns.len(), 1);
        assert_eq!(loaded.pending_review_spawns[0].pr_number, 42);
    }
}
