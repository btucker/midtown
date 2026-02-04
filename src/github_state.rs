//! Persistent GitHub state for the midtown daemon.
//!
//! Stores PR reviewer assignments in a JSON file that survives daemon restarts.
//! This prevents duplicate reviewer assignments and enables the web UI to show
//! which coworker is reviewing each PR.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use tracing::{debug, warn};

/// How long a review assignment is valid before it expires (10 minutes).
/// Mirrors PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS from the in-memory tracker.
pub const PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS: u64 = 600;

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

    /// Pending reviewer spawns from webhook events, waiting for the delay to expire.
    /// Persisted so they survive daemon restarts (unlike the previous tokio::sleep approach).
    #[serde(default)]
    pub pending_review_spawns: Vec<PendingReviewSpawn>,

    /// Per-PR timestamp of the last webhook event that handled this PR.
    /// Polling checks this to defer to webhooks when they're healthy for a specific PR.
    #[serde(default)]
    pub pr_last_webhook_event: HashMap<u64, DateTime<Utc>>,
}

/// A pending reviewer spawn triggered by a webhook event.
///
/// Instead of using `tokio::time::sleep` in a detached task (which is lost on restart),
/// pending spawns are persisted here and checked each tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReviewSpawn {
    /// PR number that needs a reviewer.
    pub pr_number: u64,
    /// Wall-clock time after which the reviewer should be spawned.
    pub spawn_after: DateTime<Utc>,
}

/// How the reviewer assignment was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentSource {
    /// Triggered by a GitHub webhook event (PR opened / ready_for_review).
    Webhook,
    /// Triggered by the periodic polling loop as a fallback.
    PollingFallback,
    /// Manually assigned (e.g., by the lead or via an RPC command).
    Manual,
}

impl fmt::Display for AssignmentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssignmentSource::Webhook => write!(f, "webhook"),
            AssignmentSource::PollingFallback => write!(f, "polling"),
            AssignmentSource::Manual => write!(f, "manual"),
        }
    }
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
    /// How this assignment was triggered (webhook, polling fallback, or manual).
    #[serde(default = "default_assignment_source")]
    pub source: AssignmentSource,
    /// Optional webhook delivery ID for debugging/telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_event_id: Option<String>,
}

fn default_assignment_source() -> AssignmentSource {
    AssignmentSource::PollingFallback
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

    /// Assign a reviewer to a PR with the given source.
    pub fn assign_reviewer(&mut self, pr_number: u64, reviewer: &str, source: AssignmentSource) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            assigned_at: Utc::now(),
            source,
            webhook_event_id: None,
        };
        self.pr_reviewers.insert(pr_number, assignment);
    }

    /// Assign a reviewer to a PR with a webhook event ID for tracing.
    pub fn assign_reviewer_with_event_id(
        &mut self,
        pr_number: u64,
        reviewer: &str,
        source: AssignmentSource,
        webhook_event_id: Option<String>,
    ) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            assigned_at: Utc::now(),
            source,
            webhook_event_id,
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

    /// Remove a reviewer assignment by coworker name (e.g., when coworker session ends).
    ///
    /// Returns the removed assignment if found.
    pub fn remove_assignment_by_reviewer(
        &mut self,
        reviewer: &str,
    ) -> Option<PrReviewerAssignment> {
        if let Some(pr_number) = self.pr_for_reviewer(reviewer) {
            self.pr_reviewers.remove(&pr_number)
        } else {
            None
        }
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

    /// Record that a webhook event handled a specific PR.
    ///
    /// Polling will check this timestamp before acting on the same PR,
    /// deferring to the webhook path when it's been active recently.
    pub fn record_webhook_event(&mut self, pr_number: u64) {
        self.pr_last_webhook_event.insert(pr_number, Utc::now());
    }

    /// Check if a webhook recently handled this PR (within the given window).
    ///
    /// Returns `true` if a webhook event was recorded for this PR within
    /// `window_secs` seconds, meaning polling should defer.
    pub fn webhook_recently_handled(&self, pr_number: u64, window_secs: i64) -> bool {
        match self.pr_last_webhook_event.get(&pr_number) {
            Some(ts) => {
                let elapsed = Utc::now().signed_duration_since(*ts);
                elapsed < chrono::Duration::seconds(window_secs)
            }
            None => false,
        }
    }

    /// Get the assignment source for a PR's reviewer, if assigned.
    pub fn get_assignment_source(&self, pr_number: u64) -> Option<AssignmentSource> {
        self.pr_reviewers.get(&pr_number).map(|a| a.source)
    }

    /// Count assignments by source (for telemetry).
    pub fn count_by_source(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for assignment in self.pr_reviewers.values() {
            *counts.entry(assignment.source.to_string()).or_insert(0) += 1;
        }
        counts
    }

    /// Clean up stale per-PR webhook event timestamps (older than 1 hour).
    pub fn cleanup_stale_webhook_events(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::seconds(3600);
        self.pr_last_webhook_event.retain(|_, ts| *ts > cutoff);
    }

    /// Add a pending review spawn for a PR.
    pub fn add_pending_review_spawn(&mut self, pr_number: u64, spawn_after: DateTime<Utc>) {
        // Don't add duplicates for the same PR
        if !self
            .pending_review_spawns
            .iter()
            .any(|p| p.pr_number == pr_number)
        {
            self.pending_review_spawns.push(PendingReviewSpawn {
                pr_number,
                spawn_after,
            });
        }
    }

    /// Drain pending review spawns that are ready (spawn_after <= now).
    pub fn drain_ready_review_spawns(&mut self) -> Vec<u64> {
        let now = Utc::now();
        let (ready, remaining): (Vec<_>, Vec<_>) = self
            .pending_review_spawns
            .drain(..)
            .partition(|p| p.spawn_after <= now);
        self.pending_review_spawns = remaining;
        ready.into_iter().map(|p| p.pr_number).collect()
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

        // Clean up per-PR webhook event timestamps for closed PRs
        self.pr_last_webhook_event
            .retain(|pr, _| open_set.contains(pr));
    }
}

/// Load GitHub state for a specific repository (legacy file).
///
/// Used only by migration code in `daemon::state`. New code should use
/// `DaemonPersistentState::load_for_repo()` instead.
pub fn load_state_for_repo(repo: &str) -> io::Result<GitHubState> {
    let path = crate::paths::github_state_file_for_repo(repo);
    GitHubState::load(&path)
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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

        assert!(state.is_assigned(42));
        assert_eq!(state.get_reviewer(42), Some("lexington"));
        assert!(!state.is_assigned(43));
    }

    #[test]
    fn test_remove_assignment() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

        let removed = state.remove_assignment(42);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().reviewer, "lexington");
        assert!(!state.is_assigned(42));
    }

    #[test]
    fn test_assigned_reviewers() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

        let reviewers: Vec<_> = state.assigned_reviewers().collect();
        assert_eq!(reviewers.len(), 2);
        assert!(reviewers.contains(&"lexington"));
        assert!(reviewers.contains(&"park"));
    }

    #[test]
    fn test_pr_for_reviewer() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

        assert_eq!(state.pr_for_reviewer("lexington"), Some(42));
        assert_eq!(state.pr_for_reviewer("park"), Some(43));
        assert_eq!(state.pr_for_reviewer("york"), None);

        // After removal, should return None
        state.remove_assignment(42);
        assert_eq!(state.pr_for_reviewer("lexington"), None);
    }

    #[test]
    fn test_remove_assignment_by_reviewer() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

        // Remove lexington's assignment by name
        let removed = state.remove_assignment_by_reviewer("lexington");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().pr_number, 42);

        // Verify it's gone
        assert!(state.pr_for_reviewer("lexington").is_none());
        assert!(state.get_reviewer(42).is_none());

        // park's assignment should be unaffected
        assert_eq!(state.pr_for_reviewer("park"), Some(43));

        // Removing non-existent reviewer returns None
        assert!(state.remove_assignment_by_reviewer("york").is_none());
    }

    #[test]
    fn test_cleanup_closed_prs() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
        state.assign_reviewer(44, "york", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
        state.assign_reviewer(44, "lexington", AssignmentSource::Webhook); // duplicate reviewer name

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
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

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
    fn test_add_pending_review_spawn() {
        let mut state = GitHubState::default();
        let future = Utc::now() + chrono::Duration::seconds(60);

        state.add_pending_review_spawn(42, future);
        assert_eq!(state.pending_review_spawns.len(), 1);
        assert_eq!(state.pending_review_spawns[0].pr_number, 42);

        // Duplicate should be ignored
        state.add_pending_review_spawn(42, future);
        assert_eq!(state.pending_review_spawns.len(), 1);

        // Different PR should be added
        state.add_pending_review_spawn(43, future);
        assert_eq!(state.pending_review_spawns.len(), 2);
    }

    #[test]
    fn test_drain_ready_review_spawns() {
        let mut state = GitHubState::default();
        let past = Utc::now() - chrono::Duration::seconds(10);
        let future = Utc::now() + chrono::Duration::seconds(60);

        state.add_pending_review_spawn(42, past);
        state.add_pending_review_spawn(43, future);
        state.add_pending_review_spawn(44, past);

        let ready = state.drain_ready_review_spawns();
        assert_eq!(ready.len(), 2);
        assert!(ready.contains(&42));
        assert!(ready.contains(&44));

        // Only the future spawn should remain
        assert_eq!(state.pending_review_spawns.len(), 1);
        assert_eq!(state.pending_review_spawns[0].pr_number, 43);
    }

    #[test]
    fn test_pending_review_spawns_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("github-state.json");

        let mut state = GitHubState::default();
        let future = Utc::now() + chrono::Duration::seconds(60);
        state.add_pending_review_spawn(42, future);
        state.add_pending_review_spawn(43, future);
        state.save(&path).unwrap();

        let loaded = GitHubState::load(&path).unwrap();
        assert_eq!(loaded.pending_review_spawns.len(), 2);
        assert_eq!(loaded.pending_review_spawns[0].pr_number, 42);
        assert_eq!(loaded.pending_review_spawns[1].pr_number, 43);
    }

    #[test]
    fn test_assignment_source_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("github-state.json");

        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
        state.save(&path).unwrap();

        let loaded = GitHubState::load(&path).unwrap();
        assert_eq!(
            loaded.get_assignment_source(42),
            Some(AssignmentSource::Webhook)
        );
        assert_eq!(
            loaded.get_assignment_source(43),
            Some(AssignmentSource::PollingFallback)
        );
    }

    #[test]
    fn test_webhook_recently_handled() {
        let mut state = GitHubState::default();

        // No event recorded yet
        assert!(!state.webhook_recently_handled(42, 120));

        // Record an event
        state.record_webhook_event(42);
        assert!(state.webhook_recently_handled(42, 120));
        assert!(!state.webhook_recently_handled(43, 120));
    }

    #[test]
    fn test_webhook_recently_handled_expired() {
        let mut state = GitHubState::default();

        // Manually backdate the event
        state
            .pr_last_webhook_event
            .insert(42, Utc::now() - chrono::Duration::seconds(300));

        // 120s window should not match a 300s-old event
        assert!(!state.webhook_recently_handled(42, 120));
        // 600s window should match
        assert!(state.webhook_recently_handled(42, 600));
    }

    #[test]
    fn test_count_by_source() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);
        state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
        state.assign_reviewer(44, "york", AssignmentSource::Webhook);

        let counts = state.count_by_source();
        assert_eq!(counts.get("webhook"), Some(&2));
        assert_eq!(counts.get("polling"), Some(&1));
    }

    #[test]
    fn test_cleanup_stale_webhook_events() {
        let mut state = GitHubState::default();

        // Fresh event
        state.record_webhook_event(42);
        // Stale event (2 hours ago)
        state
            .pr_last_webhook_event
            .insert(43, Utc::now() - chrono::Duration::seconds(7200));

        state.cleanup_stale_webhook_events();

        assert!(state.pr_last_webhook_event.contains_key(&42));
        assert!(!state.pr_last_webhook_event.contains_key(&43));
    }

    #[test]
    fn test_cleanup_closed_prs_cleans_webhook_events() {
        let mut state = GitHubState::default();
        state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
        state.record_webhook_event(42);
        state.record_webhook_event(43);

        // Only PR 42 is still open
        state.cleanup_closed_prs(&[42]);

        assert!(state.pr_last_webhook_event.contains_key(&42));
        assert!(!state.pr_last_webhook_event.contains_key(&43));
    }

    #[test]
    fn test_assign_reviewer_with_event_id() {
        let mut state = GitHubState::default();
        state.assign_reviewer_with_event_id(
            42,
            "lexington",
            AssignmentSource::Webhook,
            Some("delivery-abc123".to_string()),
        );

        let assignment = state.pr_reviewers.get(&42).unwrap();
        assert_eq!(assignment.source, AssignmentSource::Webhook);
        assert_eq!(
            assignment.webhook_event_id.as_deref(),
            Some("delivery-abc123")
        );
    }

    #[test]
    fn test_default_source_for_legacy_data() {
        // Simulate loading legacy data without the source field
        let json = r#"{
            "pr_reviewers": {
                "42": {
                    "pr_number": 42,
                    "reviewer": "lexington",
                    "assigned_at": "2025-01-01T00:00:00Z"
                }
            }
        }"#;

        let state: GitHubState = serde_json::from_str(json).unwrap();
        let assignment = state.pr_reviewers.get(&42).unwrap();
        // Legacy data defaults to PollingFallback
        assert_eq!(assignment.source, AssignmentSource::PollingFallback);
        assert!(assignment.webhook_event_id.is_none());
    }
}
