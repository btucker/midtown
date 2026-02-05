//! PR issue and review tracking types.
//!
//! These trackers prevent the daemon from spamming the same PR issues
//! or assigning duplicate reviews.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::constants::{PR_NUDGE_COOLDOWN_SECS, STUCK_NUDGE_COOLDOWN_SECS};

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
// LeadTypingState
// ---------------------------------------------------------------------------

/// Consolidated state for the lead's typing indicator.
///
/// Groups pane hash and last-activity timestamp into a single struct so that
/// `check_lead_typing` can acquire one lock instead of three. The `working`
/// state is derived from `last_activity` (within grace period), not stored.
#[derive(Default)]
pub(crate) struct LeadTypingState {
    pub pane_hash: u64,
    pub last_activity: Option<Instant>,
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
/// "5 checks passed on PR #42: Clippy, Test, E2E - foo, ..."
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
    pub fn add(&mut self, check: CiCheckPassed) {
        let now = Instant::now();

        // Update oldest_entry if this is the first item
        if self.oldest_entry.is_none() {
            self.oldest_entry = Some(now);
        }

        self.pending.entry(check.target).or_default().push((
            check.check_name,
            check.mention_prefix,
            now,
        ));
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

    /// Check if the buffer is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Format a batched CI notification into a channel message.
///
/// Examples:
/// - Single check: "Check 'Clippy' passed on main"
/// - Multiple checks: "5 checks passed on PR #42: Clippy, Test, E2E - foo, ..."
pub fn format_batched_ci_notification(batch: &BatchedCiNotification) -> String {
    let count = batch.check_names.len();
    let names = batch.check_names.join(", ");

    if count == 1 {
        // Single check: use original format
        format!(
            "{}Check '{}' passed on {}",
            batch.mention_prefix, batch.check_names[0], batch.target
        )
    } else {
        // Multiple checks: batched format
        format!(
            "{}{} checks passed on {}: {}",
            batch.mention_prefix, count, batch.target, names
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Graceful degradation tests: deduplication between webhook and polling
    //
    // When webhooks ARE working, they fire first and record nudges. Polling
    // then sees the nudge is on cooldown and skips duplicate action.
    //
    // When webhooks are NOT working (degraded), polling is the first to detect
    // issues and record nudges. These tests verify both paths use the same
    // tracker and respect cooldowns.
    // =========================================================================

    // -------------------------------------------------------------------------
    // PrIssueTracker — prevents double-nudging for PR issues
    // -------------------------------------------------------------------------

    #[test]
    fn tracker_allows_first_nudge() {
        let tracker = PrIssueTracker::new();

        assert!(
            tracker.should_nudge(42, PrIssueType::CiFailed),
            "first nudge for an issue should be allowed"
        );
    }

    #[test]
    fn tracker_blocks_immediate_repeat_nudge() {
        let mut tracker = PrIssueTracker::new();

        // Webhook fires first and records the nudge
        tracker.record_nudge(42, PrIssueType::CiFailed);

        // Polling runs shortly after — should be blocked
        assert!(
            !tracker.should_nudge(42, PrIssueType::CiFailed),
            "immediate repeat nudge should be blocked (webhook then polling)"
        );
    }

    #[test]
    fn tracker_allows_different_issue_types() {
        let mut tracker = PrIssueTracker::new();

        // Webhook records CI failure nudge
        tracker.record_nudge(42, PrIssueType::CiFailed);

        // Different issue type should still be allowed
        assert!(
            tracker.should_nudge(42, PrIssueType::MergeConflict),
            "different issue type on same PR should be allowed"
        );
    }

    #[test]
    fn tracker_allows_different_prs() {
        let mut tracker = PrIssueTracker::new();

        // Webhook records nudge for PR #42
        tracker.record_nudge(42, PrIssueType::Approved);

        // Same issue type on different PR should be allowed
        assert!(
            tracker.should_nudge(43, PrIssueType::Approved),
            "same issue type on different PR should be allowed"
        );
    }

    #[test]
    fn tracker_cleanup_removes_expired() {
        let mut tracker = PrIssueTracker::new();

        // Insert an entry with an expired timestamp
        tracker.nudged.insert(
            (42, PrIssueType::CiFailed),
            Instant::now() - Duration::from_secs(PR_NUDGE_COOLDOWN_SECS + 1),
        );

        tracker.cleanup();

        assert!(
            tracker.nudged.is_empty(),
            "expired entries should be removed by cleanup"
        );
    }

    // -------------------------------------------------------------------------
    // StuckConditionTracker — polling-only stuck detection
    // -------------------------------------------------------------------------

    #[test]
    fn stuck_tracker_tracks_condition() {
        let mut tracker = StuckConditionTracker::new();

        let first_detected = tracker.track("42", StuckConditionType::NoReview);

        // Should return a reasonable timestamp (not too far in the past)
        assert!(
            first_detected.elapsed() < Duration::from_secs(1),
            "first detected should be approximately now"
        );
    }

    #[test]
    fn stuck_tracker_allows_first_nudge() {
        let mut tracker = StuckConditionTracker::new();

        tracker.track("42", StuckConditionType::NoReview);

        assert!(
            tracker.should_nudge("42", StuckConditionType::NoReview),
            "should allow first nudge for tracked condition"
        );
    }

    #[test]
    fn stuck_tracker_blocks_repeat_nudge() {
        let mut tracker = StuckConditionTracker::new();

        tracker.track("42", StuckConditionType::NoReview);
        tracker.record_nudge("42", StuckConditionType::NoReview);

        assert!(
            !tracker.should_nudge("42", StuckConditionType::NoReview),
            "should block immediate repeat nudge"
        );
    }

    #[test]
    fn stuck_tracker_increments_nudge_count() {
        let mut tracker = StuckConditionTracker::new();

        tracker.track("york", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("york", StuckConditionType::SilentCoworker),
            0
        );

        tracker.record_nudge("york", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("york", StuckConditionType::SilentCoworker),
            1
        );

        // Manually reset cooldown to allow another nudge
        if let Some(entry) = tracker
            .conditions
            .get_mut(&("york".to_string(), StuckConditionType::SilentCoworker))
        {
            entry.1 = Some(Instant::now() - Duration::from_secs(STUCK_NUDGE_COOLDOWN_SECS + 1));
        }

        tracker.record_nudge("york", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("york", StuckConditionType::SilentCoworker),
            2,
            "nudge count should escalate for repeated stuck conditions"
        );
    }

    #[test]
    fn stuck_tracker_clear_removes_condition() {
        let mut tracker = StuckConditionTracker::new();

        tracker.track("42", StuckConditionType::MergeReady);
        tracker.record_nudge("42", StuckConditionType::MergeReady);

        tracker.clear("42", StuckConditionType::MergeReady);

        assert!(
            !tracker.should_nudge("42", StuckConditionType::MergeReady),
            "cleared condition should not be nudgeable (not tracked)"
        );

        // But if we track it again, it should be fresh
        tracker.track("42", StuckConditionType::MergeReady);
        assert!(
            tracker.should_nudge("42", StuckConditionType::MergeReady),
            "re-tracked condition should be nudgeable again"
        );
    }

    // -------------------------------------------------------------------------
    // OrphanTracker — orphaned worktree detection (polling-only)
    // -------------------------------------------------------------------------

    #[test]
    fn orphan_tracker_blocks_warn_during_grace_period() {
        // Bug: orphan detection runs every 10s, PR poll every 30s.
        // If a coworker opens a PR and goes idle, the orphan check can fire
        // before the PR poll updates open_pr_owners, causing a false positive.
        // Fix: don't warn until grace period has elapsed since first detection.
        let mut tracker = OrphanTracker::new();

        tracker.track("lexington".to_string());

        assert!(
            !tracker.should_warn("lexington"),
            "should NOT warn during grace period after first detection"
        );
    }

    #[test]
    fn orphan_tracker_allows_warn_after_grace_period() {
        let mut tracker = OrphanTracker::new();

        // Simulate detection that happened long ago (beyond grace period)
        tracker.entries.insert(
            "lexington".to_string(),
            OrphanEntry {
                first_detected: Instant::now()
                    - ORPHAN_INITIAL_GRACE_PERIOD
                    - Duration::from_secs(1),
                warned_at: None,
            },
        );

        assert!(
            tracker.should_warn("lexington"),
            "should allow warning after grace period"
        );
    }

    #[test]
    fn orphan_tracker_blocks_repeat_warn() {
        let mut tracker = OrphanTracker::new();

        tracker.track("lexington".to_string());
        tracker.record_warn("lexington");

        assert!(
            !tracker.should_warn("lexington"),
            "should block immediate repeat warning"
        );
    }

    #[test]
    fn orphan_tracker_prune_removes_resolved() {
        let mut tracker = OrphanTracker::new();

        tracker.track("lexington".to_string());
        tracker.track("amsterdam".to_string());

        // Lexington's worktree is restored — no longer flagged
        tracker.prune(&["amsterdam".to_string()]);

        assert!(tracker.entries.contains_key("amsterdam"));
        assert!(
            !tracker.entries.contains_key("lexington"),
            "pruned orphan should be removed"
        );
    }

    // -------------------------------------------------------------------------
    // CommentTracker — polling fallback for review comment notifications
    // -------------------------------------------------------------------------

    #[test]
    fn comment_tracker_detects_new_comments() {
        let mut tracker = CommentTracker::new();

        // First poll: PR #42 has 2 non-owner comments
        assert!(
            tracker.has_new_comments(42, 2),
            "first poll with comments should return true"
        );
        tracker.record(42, 2);

        // Second poll: same count
        assert!(
            !tracker.has_new_comments(42, 2),
            "same count should return false"
        );

        // Third poll: count increased
        assert!(
            tracker.has_new_comments(42, 3),
            "increased count should return true"
        );
    }

    #[test]
    fn comment_tracker_returns_false_for_new_pr_with_no_comments() {
        let tracker = CommentTracker::new();

        // New PR with 0 comments — no new activity
        assert!(
            !tracker.has_new_comments(42, 0),
            "new PR with no comments should return false"
        );
    }

    #[test]
    fn comment_tracker_cleanup_removes_closed_prs() {
        let mut tracker = CommentTracker::new();

        tracker.record(42, 5);
        tracker.record(43, 3);
        tracker.record(44, 1);

        // Only PRs 42 and 44 are still open
        tracker.cleanup(&[42, 44]);

        assert!(tracker.comment_counts.contains_key(&42));
        assert!(!tracker.comment_counts.contains_key(&43));
        assert!(tracker.comment_counts.contains_key(&44));
    }

    // -------------------------------------------------------------------------
    // Integration scenario: webhook fires before polling
    // -------------------------------------------------------------------------

    #[test]
    fn webhook_before_polling_prevents_duplicate() {
        let mut tracker = PrIssueTracker::new();

        // Scenario: PR #42 gets CI failure
        // 1. Webhook fires and nudges owner
        tracker.record_nudge(42, PrIssueType::CiFailed);

        // 2. ~30s later, polling runs and detects the same issue
        // Polling should see the cooldown and skip
        assert!(
            !tracker.should_nudge(42, PrIssueType::CiFailed),
            "polling should skip when webhook already handled the issue"
        );
    }

    // -------------------------------------------------------------------------
    // Integration scenario: webhook degraded, polling takes over
    // -------------------------------------------------------------------------

    #[test]
    fn polling_handles_issue_when_webhook_missing() {
        let mut tracker = PrIssueTracker::new();

        // Scenario: Webhook is degraded, polling is first to detect CI failure
        // 1. Polling detects issue (no prior webhook)
        assert!(
            tracker.should_nudge(42, PrIssueType::CiFailed),
            "polling should handle issue when webhook hasn't fired"
        );

        // 2. Polling records the nudge
        tracker.record_nudge(42, PrIssueType::CiFailed);

        // 3. Next polling cycle should be blocked
        assert!(
            !tracker.should_nudge(42, PrIssueType::CiFailed),
            "repeat polling should be blocked after first handled"
        );
    }

    // -------------------------------------------------------------------------
    // CiNotificationBuffer — batches CI check notifications
    // -------------------------------------------------------------------------

    #[test]
    fn ci_buffer_batches_checks_by_target() {
        let mut buffer = CiNotificationBuffer::new();

        // Add checks for two different targets
        buffer.add(CiCheckPassed {
            check_name: "Clippy".to_string(),
            target: "main".to_string(),
            mention_prefix: "".to_string(),
        });
        buffer.add(CiCheckPassed {
            check_name: "Test".to_string(),
            target: "main".to_string(),
            mention_prefix: "".to_string(),
        });
        buffer.add(CiCheckPassed {
            check_name: "Build".to_string(),
            target: "PR #42".to_string(),
            mention_prefix: "@columbus ".to_string(),
        });

        // Buffer should not flush immediately
        assert!(
            !buffer.should_flush(),
            "buffer should not flush immediately"
        );

        // Force flush by simulating time passing
        buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));

        assert!(buffer.should_flush(), "buffer should flush after delay");

        let batched = buffer.flush();
        assert_eq!(
            batched.len(),
            2,
            "should have 2 batched notifications (one per target)"
        );

        // Find the "main" batch
        let main_batch = batched.iter().find(|b| b.target == "main").unwrap();
        assert_eq!(main_batch.check_names.len(), 2);
        assert!(main_batch.check_names.contains(&"Clippy".to_string()));
        assert!(main_batch.check_names.contains(&"Test".to_string()));
        assert_eq!(main_batch.mention_prefix, "");

        // Find the "PR #42" batch
        let pr_batch = batched.iter().find(|b| b.target == "PR #42").unwrap();
        assert_eq!(pr_batch.check_names.len(), 1);
        assert!(pr_batch.check_names.contains(&"Build".to_string()));
        assert_eq!(pr_batch.mention_prefix, "@columbus ");
    }

    #[test]
    fn ci_buffer_clears_after_flush() {
        let mut buffer = CiNotificationBuffer::new();

        buffer.add(CiCheckPassed {
            check_name: "Test".to_string(),
            target: "main".to_string(),
            mention_prefix: "".to_string(),
        });

        // Force flush
        buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));
        let _ = buffer.flush();

        assert!(buffer.is_empty(), "buffer should be empty after flush");
        assert!(
            !buffer.should_flush(),
            "should_flush should be false after flush"
        );
    }

    #[test]
    fn ci_buffer_single_check_returns_single_result() {
        let mut buffer = CiNotificationBuffer::new();

        buffer.add(CiCheckPassed {
            check_name: "Clippy".to_string(),
            target: "main".to_string(),
            mention_prefix: "".to_string(),
        });

        // Force flush
        buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));
        let batched = buffer.flush();

        assert_eq!(batched.len(), 1);
        assert_eq!(batched[0].check_names.len(), 1);
        assert_eq!(batched[0].check_names[0], "Clippy");
    }

    // -------------------------------------------------------------------------
    // format_batched_ci_notification tests
    // -------------------------------------------------------------------------

    #[test]
    fn format_batched_ci_single_check() {
        let batch = BatchedCiNotification {
            target: "main".to_string(),
            mention_prefix: "".to_string(),
            check_names: vec!["Clippy".to_string()],
        };
        let msg = format_batched_ci_notification(&batch);
        assert_eq!(msg, "Check 'Clippy' passed on main");
    }

    #[test]
    fn format_batched_ci_single_check_with_mention() {
        let batch = BatchedCiNotification {
            target: "PR #42".to_string(),
            mention_prefix: "@columbus ".to_string(),
            check_names: vec!["Build".to_string()],
        };
        let msg = format_batched_ci_notification(&batch);
        assert_eq!(msg, "@columbus Check 'Build' passed on PR #42");
    }

    #[test]
    fn format_batched_ci_multiple_checks() {
        let batch = BatchedCiNotification {
            target: "main".to_string(),
            mention_prefix: "".to_string(),
            check_names: vec![
                "Clippy".to_string(),
                "Test".to_string(),
                "E2E - foo".to_string(),
            ],
        };
        let msg = format_batched_ci_notification(&batch);
        assert_eq!(msg, "3 checks passed on main: Clippy, Test, E2E - foo");
    }

    #[test]
    fn format_batched_ci_multiple_checks_with_mention() {
        let batch = BatchedCiNotification {
            target: "PR #99".to_string(),
            mention_prefix: "@park ".to_string(),
            check_names: vec!["Build".to_string(), "Test".to_string()],
        };
        let msg = format_batched_ci_notification(&batch);
        assert_eq!(msg, "@park 2 checks passed on PR #99: Build, Test");
    }
}
