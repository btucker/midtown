//! PR issue and review tracking types.
//!
//! These trackers prevent the daemon from spamming the same PR issues
//! or assigning duplicate reviews.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::constants::{
    PR_NUDGE_COOLDOWN_SECS, PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, STUCK_NUDGE_COOLDOWN_SECS,
};

// ---------------------------------------------------------------------------
// PrIssueType
// ---------------------------------------------------------------------------

/// Types of actionable PR issues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrIssueType {
    /// PR has merge conflicts
    MergeConflict,
    /// CI checks failed
    CiFailed,
    /// Review requested changes
    ChangesRequested,
    /// PR is approved and ready to merge
    Approved,
    /// PR needs code review (no Claude review comment yet)
    NeedsReview,
    /// PR has review comments from non-owners
    ReviewComment,
    /// PR review is complete (Claude review posted), author should act
    ReviewComplete,
}

impl std::fmt::Display for PrIssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrIssueType::MergeConflict => write!(f, "merge conflict"),
            PrIssueType::CiFailed => write!(f, "CI failed"),
            PrIssueType::ChangesRequested => write!(f, "changes requested"),
            PrIssueType::Approved => write!(f, "approved"),
            PrIssueType::NeedsReview => write!(f, "needs review"),
            PrIssueType::ReviewComment => write!(f, "review comment"),
            PrIssueType::ReviewComplete => write!(f, "review complete"),
        }
    }
}

// ---------------------------------------------------------------------------
// PrIssueTracker
// ---------------------------------------------------------------------------

/// Tracks which PR issues have been nudged to avoid spamming
#[derive(Debug, Default)]
pub struct PrIssueTracker {
    /// Map of (pr_number, issue_type) -> last_nudge_time
    nudged: HashMap<(u64, PrIssueType), Instant>,
}

impl PrIssueTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a PR has been recently tracked for any issue
    pub fn is_recently_tracked(&self, pr_number: u64) -> bool {
        self.nudged.keys().any(|(num, _)| {
            *num == pr_number
                && self
                    .nudged
                    .get(&(*num, PrIssueType::NeedsReview))
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(PR_NUDGE_COOLDOWN_SECS))
        })
    }

    /// Check if we should nudge for this issue (not nudged recently)
    pub fn should_nudge(&self, pr_number: u64, issue_type: PrIssueType) -> bool {
        match self.nudged.get(&(pr_number, issue_type)) {
            Some(last_nudge) => last_nudge.elapsed() >= Duration::from_secs(PR_NUDGE_COOLDOWN_SECS),
            None => true,
        }
    }

    /// Record that we nudged for this issue
    pub fn record_nudge(&mut self, pr_number: u64, issue_type: PrIssueType) {
        self.nudged.insert((pr_number, issue_type), Instant::now());
    }

    /// Clean up old entries (older than cooldown period)
    pub fn cleanup(&mut self) {
        let cutoff = Duration::from_secs(PR_NUDGE_COOLDOWN_SECS);
        self.nudged
            .retain(|_, last_nudge| last_nudge.elapsed() < cutoff);
    }
}

// ---------------------------------------------------------------------------
// PrReviewTracker
// ---------------------------------------------------------------------------

/// Tracks which PRs have been assigned for review to avoid duplicates.
#[derive(Debug, Default)]
pub struct PrReviewTracker {
    /// Map of pr_number -> (assigned_coworker, assignment_time)
    assigned: HashMap<u64, (String, Instant)>,
}

impl PrReviewTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a PR has been assigned for review recently
    pub fn is_assigned(&self, pr_number: u64) -> bool {
        match self.assigned.get(&pr_number) {
            Some((_, assigned_at)) => {
                assigned_at.elapsed() < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS)
            }
            None => false,
        }
    }

    /// Get the assigned reviewer name for a PR (ignoring timeout).
    /// Used to check if a reviewer is still running before cleaning up assignments.
    pub fn get_reviewer(&self, pr_number: u64) -> Option<&str> {
        self.assigned.get(&pr_number).map(|(name, _)| name.as_str())
    }

    /// Record a review assignment
    pub fn assign(&mut self, pr_number: u64, coworker: &str) {
        self.assigned
            .insert(pr_number, (coworker.to_string(), Instant::now()));
    }

    /// Get the number of active review assignments
    pub fn active_count(&self) -> usize {
        self.assigned
            .values()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS))
            .count()
    }

    /// Clean up stale assignments
    pub fn cleanup(&mut self) {
        let timeout = Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS);
        self.assigned.retain(|_, (_, t)| t.elapsed() < timeout);
    }

    /// Clean up stale assignments, but preserve assignments for active coworkers.
    ///
    /// This prevents reviewers from losing their PR assignment tracking while
    /// they are still actively running (e.g., a review taking longer than the
    /// timeout). Assignments for inactive coworkers are cleaned up normally.
    /// Active coworkers' assignments are refreshed so timeout-based lookups
    /// (active_reviewers, pr_for_coworker) continue to work.
    pub fn cleanup_preserving(&mut self, active_coworkers: &HashSet<String>) {
        let timeout = Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS);
        let now = Instant::now();
        self.assigned.retain(|_, (name, t)| {
            if t.elapsed() < timeout {
                return true;
            }
            // Expired, but coworker is still active — refresh the timestamp
            if active_coworkers.contains(name) {
                *t = now;
                return true;
            }
            false
        });
    }

    /// Mark a PR as reviewed (remove from tracking)
    pub fn mark_reviewed(&mut self, pr_number: u64) {
        self.assigned.remove(&pr_number);
    }

    /// Get the PR number assigned to a specific coworker.
    pub fn pr_for_coworker(&self, coworker: &str) -> Option<u64> {
        self.assigned
            .iter()
            .find(|(_, (name, assigned_at))| {
                name == coworker
                    && assigned_at.elapsed()
                        < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS)
            })
            .map(|(pr_number, _)| *pr_number)
    }

    /// Record a review assignment with a specific timestamp (for testing).
    #[cfg(test)]
    pub fn assign_at(&mut self, pr_number: u64, coworker: &str, at: Instant) {
        self.assigned.insert(pr_number, (coworker.to_string(), at));
    }

    /// Get all active review assignments as (pr_number, (coworker_name, assigned_at)).
    pub fn active_assignments(&self) -> HashMap<u64, (String, Instant)> {
        let timeout = Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS);
        self.assigned
            .iter()
            .filter(|(_, (_, t))| t.elapsed() < timeout)
            .map(|(pr, (name, instant))| (*pr, (name.clone(), *instant)))
            .collect()
    }

    /// Get the set of coworker names that are actively assigned to review PRs.
    pub fn active_reviewers(&self) -> HashSet<String> {
        let timeout = Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS);
        self.assigned
            .values()
            .filter(|(_, t)| t.elapsed() < timeout)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// StuckConditionType
// ---------------------------------------------------------------------------

/// Types of "stuck" conditions that warrant nudging the lead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StuckConditionType {
    /// PR open with no review assigned or posted
    NoReview,
    /// PR has unresolved review feedback (changes requested, no new commits)
    UnresolvedFeedback,
    /// PR is approved + CI green but hasn't merged
    MergeReady,
    /// Coworker claimed a task but no channel activity
    SilentCoworker,
    /// More PRs need review than the daemon can assign reviewers to
    ReviewBacklog,
}

impl std::fmt::Display for StuckConditionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StuckConditionType::NoReview => write!(f, "no review"),
            StuckConditionType::UnresolvedFeedback => write!(f, "unresolved feedback"),
            StuckConditionType::MergeReady => write!(f, "merge-ready but not merged"),
            StuckConditionType::SilentCoworker => write!(f, "silent coworker"),
            StuckConditionType::ReviewBacklog => write!(f, "review backlog"),
        }
    }
}

// ---------------------------------------------------------------------------
// StuckConditionTracker
// ---------------------------------------------------------------------------

/// Tracks when stuck conditions were first detected and when lead was last nudged.
/// Uses a cooldown to avoid spamming the lead with the same stuck condition.
#[derive(Debug, Default)]
pub struct StuckConditionTracker {
    /// Map of (identifier, condition_type) -> (first_detected, last_nudged)
    /// identifier is PR number (as string) or coworker name
    conditions: HashMap<(String, StuckConditionType), (Instant, Option<Instant>)>,
}

impl StuckConditionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a stuck condition was detected. Returns the first-detected time.
    pub fn track(&mut self, id: &str, condition: StuckConditionType) -> Instant {
        let now = Instant::now();
        self.conditions
            .entry((id.to_string(), condition))
            .or_insert((now, None))
            .0
    }

    /// Check if we should nudge the lead about this condition (past cooldown).
    pub fn should_nudge(&self, id: &str, condition: StuckConditionType) -> bool {
        match self.conditions.get(&(id.to_string(), condition)) {
            Some((_, Some(last_nudged))) => {
                last_nudged.elapsed() >= Duration::from_secs(STUCK_NUDGE_COOLDOWN_SECS)
            }
            Some((_, None)) => true, // Never nudged
            None => false,           // Not tracked yet
        }
    }

    /// Record that we nudged the lead about this condition.
    pub fn record_nudge(&mut self, id: &str, condition: StuckConditionType) {
        if let Some(entry) = self.conditions.get_mut(&(id.to_string(), condition)) {
            entry.1 = Some(Instant::now());
        }
    }

    /// Remove a condition that is no longer stuck.
    pub fn clear(&mut self, id: &str, condition: StuckConditionType) {
        self.conditions.remove(&(id.to_string(), condition));
    }

    /// Clean up old entries where last_nudged is older than 2x cooldown.
    pub fn cleanup(&mut self) {
        let cutoff = Duration::from_secs(STUCK_NUDGE_COOLDOWN_SECS * 2);
        self.conditions
            .retain(|_, (first_detected, _)| first_detected.elapsed() < cutoff);
    }
}
