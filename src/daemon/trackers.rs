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
    #[allow(dead_code)]
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

    /// Check if we should warn about this orphan (never warned, or cooldown expired).
    pub fn should_warn(&self, name: &str) -> bool {
        match self.entries.get(name) {
            Some(entry) => match entry.warned_at {
                None => true,
                Some(warned) => warned.elapsed() >= ORPHAN_WARN_COOLDOWN,
            },
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
    fn orphan_tracker_allows_first_warn() {
        let mut tracker = OrphanTracker::new();

        tracker.track("lexington".to_string());

        assert!(
            tracker.should_warn("lexington"),
            "should allow first warning for orphan"
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
}
