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
/// Groups pane hash, working flag, and last-activity timestamp into a single
/// struct so that `check_lead_typing` can acquire one lock instead of three.
#[derive(Default)]
pub(crate) struct LeadTypingState {
    pub pane_hash: u64,
    pub working: bool,
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
