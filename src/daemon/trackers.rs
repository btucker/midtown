//! PR issue and review tracking types.
//!
//! These trackers prevent the daemon from spamming the same PR issues
//! or assigning duplicate reviews.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::constants::{
    ORPHANED_PR_NUDGE_COOLDOWN_SECS, PR_NUDGE_COOLDOWN_SECS, STUCK_NUDGE_COOLDOWN_SECS,
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
    /// PR has all CI checks passing and has review feedback to address
    GreenWithFeedback,
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
            PrIssueType::GreenWithFeedback => write!(f, "CI green with feedback"),
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

    /// Check if we should nudge for this issue (not nudged recently)
    pub fn should_nudge(&self, pr_number: u64, issue_type: PrIssueType) -> bool {
        match self.nudged.get(&(pr_number, issue_type)) {
            Some(last_nudge) => last_nudge.elapsed() >= Duration::from_secs(PR_NUDGE_COOLDOWN_SECS),
            None => true,
        }
    }

    /// Check if we should nudge for this issue using a custom cooldown duration.
    /// Used for orphaned PRs which need a longer suppression window since there's
    /// no active coworker to address the issue.
    pub fn should_nudge_with_cooldown(
        &self,
        pr_number: u64,
        issue_type: PrIssueType,
        cooldown_secs: u64,
    ) -> bool {
        match self.nudged.get(&(pr_number, issue_type)) {
            Some(last_nudge) => last_nudge.elapsed() >= Duration::from_secs(cooldown_secs),
            None => true,
        }
    }

    /// Record that we nudged for this issue
    pub fn record_nudge(&mut self, pr_number: u64, issue_type: PrIssueType) {
        self.nudged.insert((pr_number, issue_type), Instant::now());
    }

    /// Clear a specific nudge entry (e.g., when retrying after coworker death)
    pub fn clear_nudge(&mut self, pr_number: u64, issue_type: PrIssueType) {
        self.nudged.remove(&(pr_number, issue_type));
    }

    /// Check if a nudge has been recorded for this issue
    pub fn has_nudge(&self, pr_number: u64, issue_type: PrIssueType) -> bool {
        self.nudged.contains_key(&(pr_number, issue_type))
    }

    /// Clean up old entries (older than the longest cooldown period).
    /// Uses ORPHANED_PR_NUDGE_COOLDOWN_SECS since orphaned PR alerts use a
    /// longer suppression window than the standard PR_NUDGE_COOLDOWN_SECS.
    pub fn cleanup(&mut self) {
        let cutoff = Duration::from_secs(ORPHANED_PR_NUDGE_COOLDOWN_SECS);
        self.nudged
            .retain(|_, last_nudge| last_nudge.elapsed() < cutoff);
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
    /// Map of (identifier, condition_type) -> (first_detected, last_nudged, nudge_count)
    /// identifier is PR number (as string) or coworker name.
    /// nudge_count tracks how many times we've nudged for this condition,
    /// enabling escalation (e.g., nudge coworker first, then lead).
    conditions: HashMap<(String, StuckConditionType), (Instant, Option<Instant>, u32)>,
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
            .or_insert((now, None, 0))
            .0
    }

    /// Check if we should nudge about this condition (past cooldown).
    pub fn should_nudge(&self, id: &str, condition: StuckConditionType) -> bool {
        match self.conditions.get(&(id.to_string(), condition)) {
            Some((_, Some(last_nudged), _)) => {
                last_nudged.elapsed() >= Duration::from_secs(STUCK_NUDGE_COOLDOWN_SECS)
            }
            Some((_, None, _)) => true, // Never nudged
            None => false,              // Not tracked yet
        }
    }

    /// Get the number of times this condition has been nudged.
    /// Used for escalation logic (e.g., nudge coworker first, then lead).
    pub fn nudge_count(&self, id: &str, condition: StuckConditionType) -> u32 {
        self.conditions
            .get(&(id.to_string(), condition))
            .map(|(_, _, count)| *count)
            .unwrap_or(0)
    }

    /// Record that we nudged about this condition.
    pub fn record_nudge(&mut self, id: &str, condition: StuckConditionType) {
        if let Some(entry) = self.conditions.get_mut(&(id.to_string(), condition)) {
            entry.1 = Some(Instant::now());
            entry.2 += 1;
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
            .retain(|_, (first_detected, _, _)| first_detected.elapsed() < cutoff);
    }
}

// ---------------------------------------------------------------------------
// OrphanTracker
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// CommentTracker
// ---------------------------------------------------------------------------

/// Tracks the last seen comment count per PR for polling-based detection.
///
/// This enables the polling path to detect new review comments when webhooks
/// are degraded. We track comment count rather than individual comment IDs
/// because it's simpler and sufficient for "has activity since last poll".
#[derive(Debug, Default)]
pub struct CommentTracker {
    /// Map of pr_number -> (last_seen_count, last_checked_time)
    /// We track count of non-owner comments to detect when new feedback arrives.
    comment_counts: HashMap<u64, (usize, Instant)>,
}

impl CommentTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a PR has new non-owner comments since last poll.
    ///
    /// Returns `true` if the comment count increased, indicating new activity.
    /// Always returns `true` for newly seen PRs (first poll).
    pub fn has_new_comments(&self, pr_number: u64, current_count: usize) -> bool {
        match self.comment_counts.get(&pr_number) {
            Some((prev_count, _)) => current_count > *prev_count,
            None => current_count > 0, // New PR with comments
        }
    }

    /// Record the current comment count for a PR.
    pub fn record(&mut self, pr_number: u64, count: usize) {
        self.comment_counts
            .insert(pr_number, (count, Instant::now()));
    }

    /// Clean up entries for PRs that are no longer open.
    pub fn cleanup(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        self.comment_counts.retain(|pr, _| open_set.contains(pr));
    }
}

// ---------------------------------------------------------------------------
// OrphanTracker
// ---------------------------------------------------------------------------

/// Grace period before first orphan warning. Allows PR poll (30s interval)
/// to update open_pr_owners cache before we flag a worktree as orphaned.
/// Without this, orphan checks (10s interval) can fire before the cache
/// is updated, causing false positive warnings for worktrees with open PRs.
pub(super) const ORPHAN_INITIAL_GRACE_PERIOD: Duration = Duration::from_secs(60);

/// How long before re-warning about the same orphaned worktree (1 hour).
const ORPHAN_WARN_COOLDOWN: Duration = Duration::from_secs(3600);

/// Tracks orphaned worktrees with detection time and warning cooldown.
///
/// Unlike a plain `HashSet<String>`, this allows re-warning after a cooldown
/// period and automatically prunes entries for worktrees that no longer exist.
#[derive(Debug, Default)]
pub struct OrphanTracker {
    entries: HashMap<String, OrphanEntry>,
}

#[derive(Debug)]
struct OrphanEntry {
    first_detected: Instant,
    warned_at: Option<Instant>,
}

impl OrphanTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track an orphaned worktree. No-op if already tracked.
    pub fn track(&mut self, name: String) {
        self.entries.entry(name).or_insert(OrphanEntry {
            first_detected: Instant::now(),
            warned_at: None,
        });
    }

    /// Check if we should warn about this orphan.
    ///
    /// Returns false during the initial grace period after first detection,
    /// giving the PR poll time to update the open_pr_owners cache. After the
    /// grace period, allows the first warning and then rate-limits re-warnings.
    pub fn should_warn(&self, name: &str) -> bool {
        match self.entries.get(name) {
            Some(entry) => {
                // Don't warn during grace period — PR poll may not have run yet
                if entry.first_detected.elapsed() < ORPHAN_INITIAL_GRACE_PERIOD {
                    return false;
                }
                match entry.warned_at {
                    None => true,
                    Some(warned) => warned.elapsed() >= ORPHAN_WARN_COOLDOWN,
                }
            }
            None => false,
        }
    }

    /// Record that we warned about this orphan.
    pub fn record_warn(&mut self, name: &str) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.warned_at = Some(Instant::now());
        }
    }

    /// Remove entries for worktrees that are no longer in the flagged set.
    pub fn prune(&mut self, still_flagged: &[String]) {
        self.entries.retain(|name, _| still_flagged.contains(name));
    }
}

// ---------------------------------------------------------------------------
// CiNotificationBuffer
// ---------------------------------------------------------------------------

use crate::webhook::CiCheckPassed;

/// How long to buffer CI notifications before posting a batched message.
const CI_BUFFER_FLUSH_DELAY: Duration = Duration::from_secs(15);

/// Buffers successful CI check notifications for batching.
///
/// When multiple checks pass on the same target (PR or branch) within a short
/// window, we batch them into a single message like:
/// "5 checks passed on PR #42"
#[derive(Debug, Default)]
pub struct CiNotificationBuffer {
    /// Buffered checks grouped by target (e.g., "main" or "PR #42").
    /// Each entry contains: (check_name, mention_prefix, added_time)
    pending: HashMap<String, Vec<(String, String, Instant)>>,
    /// When the oldest item was added (to know when to flush).
    oldest_entry: Option<Instant>,
}

/// A batched CI notification message ready to post.
#[derive(Debug)]
pub struct BatchedCiNotification {
    /// The target (e.g., "main" or "PR #42")
    pub target: String,
    /// The coworker mention prefix (e.g., "@columbus " or "")
    pub mention_prefix: String,
    /// Names of checks that passed
    pub check_names: Vec<String>,
}

impl CiNotificationBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a successful CI check to the buffer.
    ///
    /// Deduplicates by check name within each target — the same check
    /// completing multiple times (e.g., from re-runs) is only counted once.
    pub fn add(&mut self, check: CiCheckPassed) {
        let now = Instant::now();

        let entries = self.pending.entry(check.target).or_default();

        // Skip if this check name already exists for this target
        if entries.iter().any(|(name, _, _)| *name == check.check_name) {
            return;
        }

        // Only set oldest_entry when an entry is actually added
        if self.oldest_entry.is_none() {
            self.oldest_entry = Some(now);
        }

        entries.push((check.check_name, check.mention_prefix, now));
    }

    /// Check if we have buffered items ready to flush.
    pub fn should_flush(&self) -> bool {
        self.oldest_entry
            .is_some_and(|t| t.elapsed() >= CI_BUFFER_FLUSH_DELAY)
    }

    /// Flush all buffered notifications and return batched messages.
    ///
    /// Returns a list of batched notifications, one per target.
    pub fn flush(&mut self) -> Vec<BatchedCiNotification> {
        let mut results = Vec::new();

        for (target, checks) in self.pending.drain() {
            if checks.is_empty() {
                continue;
            }

            // Use the mention prefix from the first check (they should all be the same)
            let mention_prefix = checks
                .first()
                .map(|(_, m, _)| m.clone())
                .unwrap_or_default();
            let check_names: Vec<String> = checks.into_iter().map(|(name, _, _)| name).collect();

            results.push(BatchedCiNotification {
                target,
                mention_prefix,
                check_names,
            });
        }

        self.oldest_entry = None;
        results
    }
}

/// Format a batched CI notification into a channel message.
///
/// Examples:
/// - Single check: "Check 'Clippy' passed on main"
/// - Multiple checks: "5 checks passed on PR #42"
pub fn format_batched_ci_notification(batch: &BatchedCiNotification) -> String {
    let count = batch.check_names.len();

    if count == 1 {
        format!(
            "{}Check '{}' passed on {}",
            batch.mention_prefix, batch.check_names[0], batch.target
        )
    } else {
        format!(
            "{}{} checks passed on {}",
            batch.mention_prefix, count, batch.target
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[path = "trackers_tests.rs"]
#[cfg(test)]
mod tests;
