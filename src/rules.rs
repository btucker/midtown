//! Pure decision functions and shared types for the daemon tick loop.
//!
//! Each `decide_*` function takes pre-collected state snapshots and returns
//! a decision enum or struct — no side effects, no async, fully testable.
//!
//! The [`CooldownTracker`] provides a unified cooldown mechanism.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Lightweight snapshot of a coworker at a point in time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CoworkerSnapshot {
    pub name: String,
    pub started_at: DateTime<Utc>,
    /// Claude Code session UUID, if known. Enables session-first lookups
    /// alongside name-based lookups during the multi-session migration.
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Case-insensitive membership check for a name in a slice of `String`.
///
/// O(n) scan — appropriate for `&[String]` and `&Vec<String>`.
fn contains_icase(items: &[String], name: &str) -> bool {
    items.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// Case-insensitive membership check for a name in a `HashSet<String>`.
///
/// O(1) lookup — requires that the set stores **lowercase-normalized** names
/// (which all snapshot-derived sets do).
fn hashset_contains_icase(set: &HashSet<String>, name: &str) -> bool {
    set.contains(&name.to_lowercase())
}

// ---------------------------------------------------------------------------
// CooldownTracker
// ---------------------------------------------------------------------------

/// Unified cooldown tracker that replaces six separate mechanisms in DaemonState.
///
/// Keys are `(rule_name, context_key)` pairs mapped to the [`Instant`] they
/// were last recorded. Call [`check`](CooldownTracker::check) before firing
/// and [`record`](CooldownTracker::record) after a successful fire.
///
/// Currently used in DaemonState's `cooldowns` field.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct CooldownTracker {
    entries: HashMap<(String, String), Instant>,
}

#[allow(dead_code)]
impl CooldownTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the cooldown has expired (or was never recorded).
    pub fn check(&self, rule_name: &str, key: &str, duration: Duration) -> bool {
        match self.entries.get(&(rule_name.to_owned(), key.to_owned())) {
            None => true,
            Some(last) => last.elapsed() >= duration,
        }
    }

    /// Records the current instant for the given rule/key pair.
    pub fn record(&mut self, rule_name: &str, key: &str) {
        self.entries
            .insert((rule_name.to_owned(), key.to_owned()), Instant::now());
    }

    /// Removes entries whose cooldown has long expired (2× duration),
    /// preventing unbounded growth.
    pub fn cleanup(&mut self, max_age: Duration) {
        self.entries.retain(|_k, v| v.elapsed() < max_age);
    }

    /// Number of tracked entries (useful for tests / diagnostics).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tracker has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all entries for a specific key (e.g., coworker name).
    ///
    /// Called when a coworker is shut down to prevent stale cooldown state
    /// from affecting future operations if they're respawned.
    pub fn clear_for_key(&mut self, key: &str) {
        self.entries.retain(|(_, k), _| k != key);
    }

    /// Check if an entry exists for a given rule/key pair.
    ///
    /// Returns true if there's an entry (regardless of whether cooldown expired).
    /// Useful for distinguishing "first detection" from "cooldown expired".
    pub fn has_entry(&self, rule_name: &str, key: &str) -> bool {
        self.entries
            .contains_key(&(rule_name.to_owned(), key.to_owned()))
    }
}

/// Unified per-coworker record in daemon state.
///
/// Bundles a coworker's channel activity and workflow state into a single
/// entry, ensuring both are cleared together on spawn and shutdown.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoworkerRecord {
    /// When the coworker last posted to the channel. `None` if no activity
    /// has been recorded yet (e.g., freshly spawned).
    pub last_activity: Option<Instant>,
    /// Coworker-reported workflow phase (developing, testing, PR, etc.).
    /// Set via RPC when coworker calls `midtown state <phase>`.
    pub workflow_phase: Option<crate::coworker_state::WorkflowPhase>,
    /// Task number the coworker is working on (from RPC state report).
    pub task_id: Option<u32>,
    /// When the workflow phase was last updated via RPC.
    pub workflow_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Progress percentage (0-100) reported by the coworker.
    /// Set via RPC when coworker calls `midtown state <phase> --progress <0-100>`.
    pub progress: Option<u8>,
    /// History of progress updates for time estimation (last 5 updates).
    /// Each entry is (progress_percentage, timestamp).
    pub progress_history: Vec<(u8, chrono::DateTime<chrono::Utc>)>,
}

impl CoworkerRecord {
    /// Create a fresh record entry for a newly spawned coworker.
    pub fn new_spawn() -> Self {
        Self {
            last_activity: Some(Instant::now()),
            ..Default::default()
        }
    }

    /// Format for status display (e.g. "dev#42", "test#7").
    ///
    /// Note: Task ID 0 is treated as "no task" since it's often used as a
    /// placeholder for taskless work (e.g., PR reviews without a formal task).
    pub fn display_status(&self) -> Option<String> {
        self.workflow_phase.map(|phase| match self.task_id {
            Some(id) if id > 0 => format!("{}#{}", phase.abbreviation(), id),
            _ => phase.abbreviation().to_string(),
        })
    }

    /// Estimate remaining time in seconds based on recent progress pace.
    ///
    /// Uses linear extrapolation from the most recent progress updates.
    /// Returns None if insufficient data or progress hasn't changed.
    pub fn estimated_time_remaining(&self) -> Option<u64> {
        let current_progress = self.progress?;

        // Need at least 100% - current progress to complete
        if current_progress >= 100 {
            return Some(0);
        }

        // Need at least 2 data points to calculate a rate
        if self.progress_history.len() < 2 {
            return None;
        }

        // Use the most recent two points for rate calculation
        let recent = &self.progress_history[self.progress_history.len() - 1];
        let prev = &self.progress_history[self.progress_history.len() - 2];

        let progress_delta = recent.0.saturating_sub(prev.0);
        if progress_delta == 0 {
            // No progress change - can't estimate
            return None;
        }

        let time_delta_secs = (recent.1 - prev.1).num_seconds();
        if time_delta_secs <= 0 {
            return None;
        }

        // Calculate rate: percentage points per second
        let rate = progress_delta as f64 / time_delta_secs as f64;

        // Calculate remaining percentage
        let remaining = 100 - current_progress;

        // Estimate time: remaining / rate
        let estimated_secs = (remaining as f64 / rate).round() as u64;

        Some(estimated_secs)
    }

    /// Format time remaining as a human-readable string (e.g., "~3m", "~30s").
    pub fn format_time_remaining(&self) -> Option<String> {
        let secs = self.estimated_time_remaining()?;

        if secs < 60 {
            Some(format!("~{}s", secs))
        } else if secs < 3600 {
            let mins = secs / 60;
            Some(format!("~{}m", mins))
        } else {
            let hours = secs / 3600;
            Some(format!("~{}h", hours))
        }
    }
}

/// Update the workflow phase for a coworker (from RPC state report).
///
/// When `progress` is `None`, existing progress is preserved if the phase
/// hasn't changed (e.g., hook fires without progress info). When the phase
/// changes, progress is cleared since it belonged to the previous phase.
pub(crate) fn set_workflow(
    records: &mut HashMap<String, CoworkerRecord>,
    name: &str,
    phase: crate::coworker_state::WorkflowPhase,
    task_id: Option<u32>,
    progress: Option<u8>,
) {
    let record = records.entry(name.to_string()).or_default();
    let phase_changed = record.workflow_phase != Some(phase);
    let now = chrono::Utc::now();

    record.workflow_phase = Some(phase);
    record.task_id = task_id;

    match progress {
        Some(p) => {
            record.progress = Some(p);
            // Add to progress history for time estimation
            record.progress_history.push((p, now));
            // Keep only the last 5 updates
            if record.progress_history.len() > 5 {
                record.progress_history.remove(0);
            }
        }
        None if phase_changed => {
            record.progress = None;
            record.progress_history.clear();
        }
        None => {} // preserve existing progress within same phase
    }
    record.workflow_updated_at = Some(now);
}

// ---------------------------------------------------------------------------
// Lifecycle decision types
// ---------------------------------------------------------------------------

/// Decision to shut down an idle coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownDecision {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Lifecycle decision functions (pure — no async, no side effects)
// ---------------------------------------------------------------------------

/// Context for idle shutdown decisions — bundles the many HashSet parameters
/// into a single struct to keep the function signature manageable.
pub(crate) struct IdleShutdownContext<'a> {
    pub coworkers: &'a [CoworkerSnapshot],
    pub busy_coworkers: &'a HashSet<String>,
    pub coworkers_with_open_prs: &'a HashSet<String>,
    pub active_reviewers: &'a HashSet<String>,
    pub coworkers_with_unblocked_deps: &'a HashSet<String>,
    pub ci_passed_pr_coworkers: &'a HashSet<String>,
    pub usage_limited_coworkers: &'a HashSet<String>,
    pub api_error_coworkers: &'a HashSet<String>,
    pub auth_error_coworkers: &'a HashSet<String>,
    pub pending_task_owners: &'a HashSet<String>,
    pub review_feedback_pr_coworkers: &'a HashSet<String>,
    pub now_utc: DateTime<Utc>,
    pub minimum_lifetime: Duration,
}

/// Decide which coworkers should be shut down due to idleness.
///
/// Takes pre-collected state snapshots and returns shutdown decisions
/// without performing any side effects or mutations.
///
/// A coworker is protected from break if:
/// - They have in-progress tasks (busy)
/// - They have a pending task assigned to them
/// - They have open unmerged PRs with CI not yet passed (waiting for CI)
/// - They have open PRs with CI passed AND review feedback to address
/// - They are actively reviewing a PR
/// - They have unblocked dependent tasks
/// - They have a subagent (Task tool) currently running
///
/// Coworkers with open PRs where CI has passed and NO review feedback CAN go on
/// break - they're just waiting for human review, and the daemon will respawn
/// them when feedback arrives.
///
/// Note: Pane content changes are NOT used as a protection signal. Idle coworkers
/// may have pane activity from daemon nudges, Claude Code UI updates, etc. The
/// other flags (busy, reviewing, subagent) already cover all legitimate work
/// scenarios, and `minimum_lifetime` protects freshly spawned coworkers.
pub(crate) fn decide_idle_shutdowns(ctx: &IdleShutdownContext<'_>) -> Vec<ShutdownDecision> {
    let min_lifetime = chrono::Duration::from_std(ctx.minimum_lifetime).unwrap_or_default();

    ctx.coworkers
        .iter()
        .filter(|cw| {
            let name = &cw.name;

            // The lead session should never be idle-shutdown — it's the
            // human-facing session that must always be running.
            if name.eq_ignore_ascii_case("lead") {
                return false;
            }

            // Young coworkers are protected regardless of other state.
            if ctx.now_utc.signed_duration_since(cw.started_at) < min_lifetime {
                return false;
            }

            // A coworker is protected from break if any of these hold:
            let protected_by_open_pr = hashset_contains_icase(ctx.coworkers_with_open_prs, name)
                && (!hashset_contains_icase(ctx.ci_passed_pr_coworkers, name)
                    || hashset_contains_icase(ctx.review_feedback_pr_coworkers, name));

            let is_protected = hashset_contains_icase(ctx.busy_coworkers, name)
                || hashset_contains_icase(ctx.pending_task_owners, name)
                || protected_by_open_pr
                || hashset_contains_icase(ctx.active_reviewers, name)
                || hashset_contains_icase(ctx.coworkers_with_unblocked_deps, name)
                || hashset_contains_icase(ctx.usage_limited_coworkers, name)
                || hashset_contains_icase(ctx.api_error_coworkers, name)
                || hashset_contains_icase(ctx.auth_error_coworkers, name);

            !is_protected
        })
        .map(|cw| ShutdownDecision {
            name: cw.name.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Detection types and functions
// ---------------------------------------------------------------------------

// Re-export pane detection functions for backward compatibility.
// The implementation lives in the `pane_detection` module.
pub use crate::pane_detection::has_usage_limit_pattern;

/// Decision output for usage limit expiry check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageLimitExpiryDecision {
    /// Nudge time has arrived — nudge all coworkers.
    NudgeNow,
    /// Nudge is scheduled but not yet due.
    NotYet,
    /// No nudge is scheduled.
    NoNudge,
}

/// Decide whether a scheduled usage limit nudge should fire.
pub(crate) fn decide_usage_limit_expiry(
    nudge_at: Option<tokio::time::Instant>,
    now: tokio::time::Instant,
) -> UsageLimitExpiryDecision {
    match nudge_at {
        Some(at) if now >= at => UsageLimitExpiryDecision::NudgeNow,
        Some(_) => UsageLimitExpiryDecision::NotYet,
        None => UsageLimitExpiryDecision::NoNudge,
    }
}

// ---------------------------------------------------------------------------
// Stuck coworker detection
// ---------------------------------------------------------------------------

/// A coworker detected as stuck (pane unchanged for the stuck duration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StuckCoworkerRestart {
    pub name: String,
    pub task_id: String,
    pub task_subject: String,
}

/// Coworker sets that are exempt from stuck detection.
///
/// Bundles the three HashSet parameters shared by `is_process_stuck`,
/// `decide_stuck_coworker_restarts`, and `decide_stuck_reviewer_restarts`.
pub(crate) struct StuckExemptions<'a> {
    pub usage_limited: &'a HashSet<String>,
    pub api_error: &'a HashSet<String>,
    pub auth_error: &'a HashSet<String>,
    pub attached: &'a HashSet<String>,
}

/// Check if a process should be considered stuck.
///
/// Returns `true` if the process is alive, not exempt (usage-limited, API error,
/// auth error, attached, subagent running, pending tool), and has not emitted events for
/// longer than `stuck_threshold`.
fn is_process_stuck(
    name: &str,
    health: &crate::daemon::snapshot::ProcessHealth,
    exemptions: &StuckExemptions<'_>,
    now_utc: DateTime<Utc>,
    stuck_threshold: chrono::Duration,
) -> bool {
    let is_exempt = !health.is_alive
        || health.has_running_subagent
        || health.has_pending_tool
        || hashset_contains_icase(exemptions.usage_limited, name)
        || hashset_contains_icase(exemptions.api_error, name)
        || hashset_contains_icase(exemptions.auth_error, name)
        || hashset_contains_icase(exemptions.attached, name);

    !is_exempt
        && health
            .last_event_at
            .is_some_and(|t| now_utc.signed_duration_since(t) >= stuck_threshold)
}

/// Detect coworkers whose headless process has not emitted events for
/// `stuck_duration`, indicating a stuck/hung process.
///
/// A coworker is only considered stuck if the process is alive, not exempt
/// (usage-limited, API error, attached, subagent, pending tool), idle for
/// longer than `stuck_duration`, and has an in-progress task.
///
/// Pure function: takes ProcessHealth data and returns restart decisions.
pub(crate) fn decide_stuck_coworker_restarts(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    in_progress_tasks: &[(String, String, String)],
    exemptions: &StuckExemptions<'_>,
    now_utc: DateTime<Utc>,
    stuck_duration: Duration,
) -> Vec<StuckCoworkerRestart> {
    let threshold = chrono::Duration::from_std(stuck_duration).unwrap_or_default();
    let mut restarts = Vec::new();

    for (name, health) in process_health {
        if !is_process_stuck(name, health, exemptions, now_utc, threshold) {
            continue;
        }

        let Some((task_id, task_subject, _owner)) = in_progress_tasks
            .iter()
            .find(|(_id, _subject, owner)| owner.eq_ignore_ascii_case(name))
        else {
            continue;
        };

        restarts.push(StuckCoworkerRestart {
            name: name.clone(),
            task_id: task_id.clone(),
            task_subject: task_subject.clone(),
        });
    }

    restarts
}

// ---------------------------------------------------------------------------
// Dead process detection
// ---------------------------------------------------------------------------

/// A coworker whose process has died and should be respawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadProcessRespawn {
    pub name: String,
    pub task_id: String,
    pub task_subject: String,
    pub exit_code: i32,
}

/// Detect coworkers whose headless process has exited unexpectedly.
///
/// A coworker's process is considered dead if:
/// - `is_alive` is false
/// - `exit_code` is present
/// - The coworker has an in-progress task
///
/// Pure function: takes ProcessHealth data and returns respawn decisions.
pub(crate) fn decide_dead_process_respawns(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    in_progress_tasks: &[(String, String, String)],
) -> Vec<DeadProcessRespawn> {
    let mut respawns = Vec::new();

    for (name, health) in process_health {
        // Only care about processes that died (not alive, has exit code)
        if health.is_alive || health.exit_code.is_none() {
            continue;
        }

        // Check if this coworker has an in-progress task
        let Some((task_id, task_subject, _owner)) = in_progress_tasks
            .iter()
            .find(|(_id, _subject, owner)| owner.eq_ignore_ascii_case(name))
        else {
            continue;
        };

        respawns.push(DeadProcessRespawn {
            name: name.clone(),
            task_id: task_id.clone(),
            task_subject: task_subject.clone(),
            exit_code: health.exit_code.unwrap_or(-1),
        });
    }

    respawns
}

// ---------------------------------------------------------------------------
// PR owner resume decision
// ---------------------------------------------------------------------------

/// Session mode for resuming a coworker for PR feedback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrOwnerResumeMode {
    /// Resume with saved session ID (preserves conversation history)
    WithSavedSession(String),
    /// Resume without saved session (fresh session)
    WithoutSavedSession,
}

/// Decide how to resume a PR owner when PR feedback arrives.
///
/// If the owner has a saved session in `pr_break_sessions`, use ResumeSession mode
/// to preserve conversation history. Otherwise, use fresh Resume mode.
///
/// Pure function: takes saved sessions map and returns resume mode.
pub(crate) fn decide_pr_owner_resume_mode(
    owner: &str,
    pr_break_sessions: &HashMap<String, String>,
) -> PrOwnerResumeMode {
    if let Some(session_id) = pr_break_sessions.get(owner) {
        PrOwnerResumeMode::WithSavedSession(session_id.clone())
    } else {
        PrOwnerResumeMode::WithoutSavedSession
    }
}

// ---------------------------------------------------------------------------
// Stuck reviewer detection
// ---------------------------------------------------------------------------

/// A reviewer detected as stuck (no events for the stuck duration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StuckReviewerRestart {
    pub name: String,
    pub pr_number: u64,
    pub restart_count: u32,
}

/// Detect reviewers whose headless process has not emitted events for
/// `stuck_duration`, indicating a stuck/hung reviewer session.
///
/// Parallel to `decide_stuck_coworker_restarts()` but for reviewers tracked
/// by PR number in `GitHubState`. Adds a `max_restarts` limit to prevent
/// infinite restart loops for the same PR.
///
/// Pure function: takes ProcessHealth data and returns restart decisions.
pub(crate) fn decide_stuck_reviewer_restarts(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    reviewer_pr_assignments: &HashMap<String, u64>,
    reviewer_restart_counts: &HashMap<u64, u32>,
    exemptions: &StuckExemptions<'_>,
    now_utc: DateTime<Utc>,
    stuck_duration: Duration,
    max_restarts: u32,
) -> Vec<StuckReviewerRestart> {
    let threshold = chrono::Duration::from_std(stuck_duration).unwrap_or_default();
    let mut restarts = Vec::new();

    for (name, health) in process_health {
        let pr_number = match reviewer_pr_assignments.get(name) {
            Some(pr) => *pr,
            None => continue,
        };

        if !is_process_stuck(name, health, exemptions, now_utc, threshold) {
            continue;
        }

        let current_count = reviewer_restart_counts
            .get(&pr_number)
            .copied()
            .unwrap_or(0);
        if current_count >= max_restarts {
            continue;
        }

        restarts.push(StuckReviewerRestart {
            name: name.clone(),
            pr_number,
            restart_count: current_count,
        });
    }

    restarts
}

// ---------------------------------------------------------------------------
// PR/review decision types and functions
// ---------------------------------------------------------------------------

/// Action to take for a PR issue or comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrAction {
    /// Owner is active — nudge them with a message.
    NudgeOwner { owner: String, message: String },
    /// Owner is inactive — spawn them with a message.
    SpawnOwner { owner: String, message: String },
    /// Hand off to any available coworker with the original author's session context.
    ///
    /// Used when the PR needs work but the original author is unavailable.
    /// A different coworker resumes the original session to preserve context.
    HandoffToCoworker {
        /// The coworker to assign (typically the first idle one)
        assignee: String,
        /// The original PR author
        original_author: String,
        /// The PR number
        pr_number: u64,
        /// The branch name
        branch: String,
        /// The session ID to resume
        session_id: String,
        /// The nudge message
        message: String,
    },
    /// No identifiable owner — post to channel.
    PostToChannel { message: String },
    /// Skip — dev limit reached, self-comment, on cooldown, or no owner.
    Skip { reason: String },
}

/// Context for PR session handoff — the stored session info for a PR.
///
/// When a coworker opens a PR, we store their session ID so that any
/// coworker can later resume work on that PR with full context.
#[derive(Debug, Clone)]
pub struct PrSessionContext {
    /// The Claude session ID (UUID) from the original author's session.
    pub session_id: String,
    /// The git branch for this PR.
    pub branch: String,
    /// The coworker who originally authored the PR.
    pub original_author: String,
    /// The PR number.
    pub pr_number: u64,
}

/// Core handoff logic shared by PR issue, comment, and review-complete actions.
///
/// Given the owner's active/idle status and optional session context, decides
/// whether to nudge, spawn, hand off, or skip. The `reason_label` is used in
/// skip messages (e.g., "PR issue", "PR comment").
///
/// When the owner is empty, returns `empty_owner_fallback` (PostToChannel for
/// issue actions, SpawnOwner for comment actions).
#[allow(clippy::too_many_arguments)]
fn resolve_pr_handoff(
    owner: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    session_context: Option<&PrSessionContext>,
    message: &str,
    reason_label: &str,
    empty_owner_fallback: PrAction,
) -> PrAction {
    if owner.is_empty() {
        return empty_owner_fallback;
    }

    let is_active = contains_icase(active_coworkers, owner);
    let is_idle = contains_icase(idle_coworkers, owner);

    // Owner is active and idle — nudge directly (they're available).
    if is_active && is_idle {
        return PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        };
    }

    // Owner is either active-but-busy or inactive. Try handoff first;
    // fallback depends on whether the owner is active:
    // - Active but busy → nudge (spawning an active coworker fails)
    // - Inactive → spawn (they need a new session)
    if !is_active && at_dev_limit {
        return PrAction::Skip {
            reason: format!(
                "dev limit reached, cannot spawn {} for {}",
                owner, reason_label
            ),
        };
    }

    if let Some(ctx) = session_context {
        let assignee = idle_coworkers
            .iter()
            .find(|c| !c.eq_ignore_ascii_case(owner))
            .cloned();

        if let Some(assignee) = assignee {
            return PrAction::HandoffToCoworker {
                assignee,
                original_author: ctx.original_author.clone(),
                pr_number: ctx.pr_number,
                branch: ctx.branch.clone(),
                session_id: ctx.session_id.clone(),
                message: message.to_string(),
            };
        }
    }

    if is_active {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else {
        PrAction::SpawnOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    }
}

/// Decide what action to take for a PR issue detected by polling.
///
/// Considers handing off the PR to a different coworker when the original
/// author is unavailable. Only nudges the owner if they are both active and idle.
pub fn decide_pr_issue_action_with_handoff(
    owner: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    session_context: Option<&PrSessionContext>,
    message: &str,
) -> PrAction {
    resolve_pr_handoff(
        owner,
        active_coworkers,
        idle_coworkers,
        at_dev_limit,
        session_context,
        message,
        "PR issue",
        PrAction::PostToChannel {
            message: message.to_string(),
        },
    )
}

/// Decide what action to take for a PR comment nudge (webhook-driven).
///
/// Considers handing off the PR to a different coworker when the original
/// author is unavailable. Only nudges the owner if they are both active and idle.
pub fn decide_pr_comment_action_with_handoff(
    owner: &str,
    actor: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    session_context: Option<&PrSessionContext>,
    message: &str,
) -> PrAction {
    if owner == actor {
        return PrAction::Skip {
            reason: format!("PR comment is from owner {}, skipping self-nudge", owner),
        };
    }

    resolve_pr_handoff(
        owner,
        active_coworkers,
        idle_coworkers,
        at_dev_limit,
        session_context,
        message,
        "PR comment",
        PrAction::SpawnOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        },
    )
}

/// Decide what action to take when a PR has a completed review and the
/// author needs to address feedback.
///
/// Nudge if active (idle or busy), spawn if inactive,
/// skip if inactive and at dev limit. No handoff — review feedback
/// goes to the original author.
pub fn decide_review_complete_action(
    owner: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    message: &str,
) -> PrAction {
    // Review complete doesn't use handoff — always route to the original author
    resolve_pr_handoff(
        owner,
        active_coworkers,
        idle_coworkers,
        at_dev_limit,
        None, // no session context = no handoff
        message,
        "review complete",
        PrAction::SpawnOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        },
    )
}

// ---------------------------------------------------------------------------
// Task assignment decision types and functions
// ---------------------------------------------------------------------------

/// Action to take for a pending task with an assigned owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingTaskAction {
    /// Owner is active — nudge them about the pending task.
    NudgeOwner {
        owner: String,
        task_id: String,
        task_subject: String,
    },
    /// Owner is inactive — spawn them for the pending task.
    SpawnOwner {
        owner: String,
        task_id: String,
        task_subject: String,
    },
    /// Skip — owner is lead/empty, at dev limit, or nudge on cooldown.
    Skip { reason: String },
}

/// Decide what action to take for a pending task with an assigned owner.
///
/// Pure function: determines whether to nudge an active owner, spawn an
/// inactive one, or skip.
///
/// # Arguments
/// * `is_owner_reviewer` - If true, the owner is an active reviewer. Reviewers should
///   not be nudged about main task list updates — they have their own review work.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_pending_task_action(
    task_id: &str,
    task_subject: &str,
    owner: &str,
    active_names: &HashSet<String>,
    at_dev_limit: bool,
    on_nudge_cooldown: bool,
    is_owner_reviewer: bool,
    has_in_progress_task: bool,
) -> PendingTaskAction {
    // Skip empty or lead-owned tasks
    if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
        return PendingTaskAction::Skip {
            reason: format!("task !{} owner is lead or empty", task_id),
        };
    }

    // Skip invalid coworker names — can't spawn or nudge an invalid name
    if !crate::coworker::is_coworker_name(&owner.to_lowercase()) {
        return PendingTaskAction::Skip {
            reason: format!(
                "task !{} owner '{}' is not a valid coworker name",
                task_id, owner
            ),
        };
    }

    // Skip coworkers that already have an in_progress task.
    // Enforces the one-task-per-coworker invariant: never assign a new task
    // to a coworker that already owns an in_progress task. This prevents the
    // double-assignment bug where a coworker ends up with two active tasks.
    if has_in_progress_task {
        return PendingTaskAction::Skip {
            reason: format!(
                "task !{} owner '{}' already has an in_progress task",
                task_id, owner
            ),
        };
    }

    // Skip active reviewers — they have their own review assignments and should
    // not be nudged about main task list updates.
    if is_owner_reviewer {
        return PendingTaskAction::Skip {
            reason: format!(
                "task !{} owner '{}' is an active reviewer (has review assignment)",
                task_id, owner
            ),
        };
    }

    // Owner is active → nudge (unless on cooldown)
    if active_names.contains(&owner.to_lowercase()) {
        if on_nudge_cooldown {
            return PendingTaskAction::Skip {
                reason: format!("task !{} nudge on cooldown for {}", task_id, owner),
            };
        }
        return PendingTaskAction::NudgeOwner {
            owner: owner.to_string(),
            task_id: task_id.to_string(),
            task_subject: task_subject.to_string(),
        };
    }

    // Owner is inactive → check dev limit
    if at_dev_limit {
        return PendingTaskAction::Skip {
            reason: format!(
                "dev limit reached, deferring spawn for task !{} owned by {}",
                task_id, owner
            ),
        };
    }

    PendingTaskAction::SpawnOwner {
        owner: owner.to_string(),
        task_id: task_id.to_string(),
        task_subject: task_subject.to_string(),
    }
}

/// Result of orphan recovery decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrphanRecovery {
    pub task_id: String,
    pub task_subject: String,
    pub owner: String,
}

/// Context for orphan recovery decisions — bundles the many HashSet parameters
/// into a single struct to keep the function signature manageable.
pub(crate) struct OrphanRecoveryContext<'a> {
    pub in_progress: &'a [(String, String, String)], // (task_id, task_subject, owner)
    pub active_names: &'a HashSet<String>,
    pub at_dev_limit: bool,
    pub coworkers_with_open_prs: &'a HashSet<String>,
    pub review_feedback_pr_coworkers: &'a HashSet<String>,
    pub recently_stopped: &'a HashSet<String>,
    pub attached_coworkers: &'a HashSet<String>,
}

impl OrphanRecoveryContext<'_> {
    /// Check if a task owner should be skipped for orphan recovery.
    ///
    /// Returns `true` if any of these conditions hold:
    /// - Owner is active (running session)
    /// - Owner is attached (interactive session)
    /// - Owner recently stopped (within grace period — task may not be marked done yet)
    /// - Owner has an open PR awaiting review without feedback (recovery would loop)
    fn should_skip_owner(&self, owner_lower: &str) -> bool {
        self.active_names.contains(owner_lower)
            || self.attached_coworkers.contains(owner_lower)
            || self.recently_stopped.contains(owner_lower)
            || (self.coworkers_with_open_prs.contains(owner_lower)
                && !self.review_feedback_pr_coworkers.contains(owner_lower))
    }
}

/// Decide which orphaned task (if any) to recover.
///
/// An orphaned task is `in_progress` but its owner is not active.
/// Returns at most ONE recovery action (rate-limited to one per tick).
///
/// Skips recovery when the owner recently stopped (within grace period),
/// regardless of PR status. This covers two cases:
/// 1. Coworker finished work and went idle → shutdown, but the task hasn't
///    been marked done yet. The grace period prevents false recovery.
/// 2. Coworker opened a PR and went on break waiting for review. They're
///    correctly idle and should not be recovered until the grace period expires.
///
/// After the grace period expires (or if the coworker was killed/crashed and
/// never recorded in recently_stopped), recovery fires unconditionally. This
/// ensures dead coworkers are always recovered — even if they have an open PR
/// without review feedback. CI failures on open PRs are handled separately
/// by the webhook/PR poll pathway.
pub(crate) fn decide_orphan_recovery(ctx: &OrphanRecoveryContext<'_>) -> Option<OrphanRecovery> {
    if ctx.at_dev_limit {
        return None;
    }

    for (task_id, task_subject, owner) in ctx.in_progress {
        let owner_clean = owner.trim().trim_matches('"').to_string();
        let owner_lower = owner_clean.to_lowercase();

        // Skip non-coworker owners and owners that shouldn't be recovered.
        let is_valid_coworker = !owner_clean.is_empty()
            && !owner_clean.eq_ignore_ascii_case("lead")
            && crate::coworker::is_coworker_name(&owner_lower);

        if !is_valid_coworker || ctx.should_skip_owner(&owner_lower) {
            continue;
        }

        // Found an orphan — return the first one (rate-limited).
        return Some(OrphanRecovery {
            task_id: task_id.clone(),
            task_subject: task_subject.clone(),
            owner: owner_clean,
        });
    }

    None
}

/// Action to take for an @mention of a coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionAction {
    /// Coworker is active — nudge them.
    Nudge { name: String, message: String },
    /// Coworker is inactive — spawn them.
    Spawn { name: String, message: String },
    /// Skip — self-mention or at dev limit.
    Skip { reason: String },
}

/// Decide what action to take for an @mention of a coworker.
pub(crate) fn decide_mention_action(
    mentioned_name: &str,
    sender: &str,
    is_running: bool,
    at_dev_limit: bool,
    nudge_text: &str,
) -> MentionAction {
    // Skip self-mentions
    if mentioned_name.eq_ignore_ascii_case(sender) {
        return MentionAction::Skip {
            reason: format!("{} mentioned themselves, skipping", mentioned_name),
        };
    }

    if is_running {
        MentionAction::Nudge {
            name: mentioned_name.to_string(),
            message: nudge_text.to_string(),
        }
    } else if at_dev_limit {
        MentionAction::Skip {
            reason: format!(
                "dev limit reached, cannot spawn {} for @mention",
                mentioned_name
            ),
        }
    } else {
        MentionAction::Spawn {
            name: mentioned_name.to_string(),
            message: nudge_text.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn check_returns_true_when_never_recorded() {
        let tracker = CooldownTracker::new();
        assert!(tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn check_returns_false_within_cooldown() {
        let mut tracker = CooldownTracker::new();
        tracker.record("idle_shutdown", "coworker:york");
        assert!(!tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn check_returns_true_after_cooldown_expires() {
        let mut tracker = CooldownTracker::new();
        // Manually insert an expired entry.
        tracker.entries.insert(
            ("idle_shutdown".to_owned(), "coworker:york".to_owned()),
            Instant::now() - Duration::from_secs(120),
        );
        assert!(tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn record_overwrites_previous_entry() {
        let mut tracker = CooldownTracker::new();
        // Insert an old entry.
        tracker.entries.insert(
            ("orphan".to_owned(), "global".to_owned()),
            Instant::now() - Duration::from_secs(300),
        );
        assert!(tracker.check("orphan", "global", Duration::from_secs(60)));

        // Record fresh — should now be in cooldown.
        tracker.record("orphan", "global");
        assert!(!tracker.check("orphan", "global", Duration::from_secs(60)));
    }

    #[test]
    fn different_keys_are_independent() {
        let mut tracker = CooldownTracker::new();
        tracker.record("idle_shutdown", "coworker:york");
        // Same rule, different key — should be clear.
        assert!(tracker.check(
            "idle_shutdown",
            "coworker:broadway",
            Duration::from_secs(60)
        ));
        // Different rule, same key — should be clear.
        assert!(tracker.check("prompt_nudge", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn cleanup_removes_expired_entries() {
        let mut tracker = CooldownTracker::new();
        // Old entry.
        tracker.entries.insert(
            ("old_rule".to_owned(), "key1".to_owned()),
            Instant::now() - Duration::from_secs(600),
        );
        // Fresh entry.
        tracker.record("fresh_rule", "key2");

        assert_eq!(tracker.len(), 2);
        tracker.cleanup(Duration::from_secs(300));
        assert_eq!(tracker.len(), 1);
        assert!(tracker.check("old_rule", "key1", Duration::from_secs(1)));
        assert!(!tracker.check("fresh_rule", "key2", Duration::from_secs(60)));
    }

    #[test]
    fn cleanup_keeps_recent_entries() {
        let mut tracker = CooldownTracker::new();
        tracker.record("rule_a", "k1");
        tracker.record("rule_b", "k2");
        tracker.cleanup(Duration::from_secs(300));
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn len_and_is_empty() {
        let mut tracker = CooldownTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);

        tracker.record("r", "k");
        assert!(!tracker.is_empty());
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn check_respects_short_durations() {
        let mut tracker = CooldownTracker::new();
        tracker.record("fast", "k");

        // Should be in cooldown right after recording (10ms window).
        assert!(!tracker.check("fast", "k", Duration::from_millis(10)));

        // Sleep past the cooldown.
        thread::sleep(Duration::from_millis(15));
        assert!(tracker.check("fast", "k", Duration::from_millis(10)));
    }

    #[test]
    fn clear_for_key_removes_matching_entries() {
        let mut tracker = CooldownTracker::new();

        // Add entries for multiple coworkers across different rules
        tracker.record("compaction_recovery", "york");
        tracker.record("queued_prompt_recovery", "york");
        tracker.record("compaction_recovery", "amsterdam");
        tracker.record("idle_timeout", "amsterdam");

        assert_eq!(tracker.len(), 4);

        // Clear entries for york
        tracker.clear_for_key("york");

        // Only amsterdam's entries should remain
        assert_eq!(tracker.len(), 2);
        assert!(!tracker.check("compaction_recovery", "amsterdam", Duration::from_secs(60)));
        assert!(!tracker.check("idle_timeout", "amsterdam", Duration::from_secs(60)));

        // york's entries should be cleared (check returns true = no cooldown active)
        assert!(tracker.check("compaction_recovery", "york", Duration::from_secs(60)));
        assert!(tracker.check("queued_prompt_recovery", "york", Duration::from_secs(60)));
    }

    // -----------------------------------------------------------------------
    // Helpers for lifecycle decision tests
    // -----------------------------------------------------------------------

    fn cw(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(minutes_old),
            session_id: None,
        }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // Builder for decide_idle_shutdowns — eliminates 13-arg boilerplate
    // -----------------------------------------------------------------------

    /// Test context builder for `decide_idle_shutdowns`.
    ///
    /// All sets default to empty; callers only set the fields they care about.
    #[derive(Default)]
    struct IdleShutdownCtx {
        coworkers: Vec<CoworkerSnapshot>,
        busy: HashSet<String>,
        open_prs: HashSet<String>,
        reviewers: HashSet<String>,
        unblocked_deps: HashSet<String>,
        ci_passed: HashSet<String>,
        usage_limited: HashSet<String>,
        api_error: HashSet<String>,
        auth_error: HashSet<String>,
        pending_tasks: HashSet<String>,
        review_feedback: HashSet<String>,
        minimum_lifetime: Duration,
    }

    impl IdleShutdownCtx {
        /// Start with a single coworker (10 min old) and 5 min minimum lifetime.
        fn one(name: &str) -> Self {
            Self {
                coworkers: vec![cw(name, 10)],
                minimum_lifetime: Duration::from_secs(300),
                ..Default::default()
            }
        }

        /// Start with a young coworker (2 min old).
        fn one_young(name: &str) -> Self {
            Self {
                coworkers: vec![cw(name, 2)],
                minimum_lifetime: Duration::from_secs(300),
                ..Default::default()
            }
        }

        fn busy(mut self, names: &[&str]) -> Self {
            self.busy = set(names);
            self
        }
        fn open_prs(mut self, names: &[&str]) -> Self {
            self.open_prs = set(names);
            self
        }
        fn reviewers(mut self, names: &[&str]) -> Self {
            self.reviewers = set(names);
            self
        }
        fn unblocked_deps(mut self, names: &[&str]) -> Self {
            self.unblocked_deps = set(names);
            self
        }
        fn ci_passed(mut self, names: &[&str]) -> Self {
            self.ci_passed = set(names);
            self
        }
        fn usage_limited(mut self, names: &[&str]) -> Self {
            self.usage_limited = set(names);
            self
        }
        fn api_error(mut self, names: &[&str]) -> Self {
            self.api_error = set(names);
            self
        }
        fn pending_tasks(mut self, names: &[&str]) -> Self {
            self.pending_tasks = set(names);
            self
        }
        fn review_feedback(mut self, names: &[&str]) -> Self {
            self.review_feedback = set(names);
            self
        }

        fn run(&self) -> Vec<ShutdownDecision> {
            let ctx = IdleShutdownContext {
                coworkers: &self.coworkers,
                busy_coworkers: &self.busy,
                coworkers_with_open_prs: &self.open_prs,
                active_reviewers: &self.reviewers,
                coworkers_with_unblocked_deps: &self.unblocked_deps,
                ci_passed_pr_coworkers: &self.ci_passed,
                usage_limited_coworkers: &self.usage_limited,
                api_error_coworkers: &self.api_error,
                auth_error_coworkers: &self.auth_error,
                pending_task_owners: &self.pending_tasks,
                review_feedback_pr_coworkers: &self.review_feedback,
                now_utc: Utc::now(),
                minimum_lifetime: self.minimum_lifetime,
            };
            decide_idle_shutdowns(&ctx)
        }
    }

    // -----------------------------------------------------------------------
    // Helper for ProcessHealth construction in stuck detection tests
    // -----------------------------------------------------------------------

    /// Create a default `ProcessHealth` for a stuck coworker (alive, no events for 10 min).
    fn stuck_health(now: DateTime<Utc>) -> crate::daemon::snapshot::ProcessHealth {
        crate::daemon::snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            exit_code: None,
        }
    }

    // -----------------------------------------------------------------------
    // decide_idle_shutdowns tests
    // -----------------------------------------------------------------------

    #[test]
    fn idle_shutdown_after_timeout() {
        let decisions = IdleShutdownCtx::one("york").run();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_busy_coworker() {
        let decisions = IdleShutdownCtx::one("york").busy(&["york"]).run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr() {
        let decisions = IdleShutdownCtx::one("york").open_prs(&["york"]).run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_active_reviewer() {
        let decisions = IdleShutdownCtx::one("york").reviewers(&["york"]).run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_unblocked_deps() {
        let decisions = IdleShutdownCtx::one("york").unblocked_deps(&["york"]).run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_young_coworker() {
        let decisions = IdleShutdownCtx::one_young("york").run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_isolated_coworker_immediate() {
        let decisions = IdleShutdownCtx::one("reviewer").run();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
    }

    #[test]
    fn idle_shutdown_immediate_for_unprotected_coworker() {
        let decisions = IdleShutdownCtx::one("york").run();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr_no_ci() {
        let decisions = IdleShutdownCtx::one("york").open_prs(&["york"]).run();
        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_sends_idle_coworker_on_break_despite_pane_activity() {
        // Bug #62: Pane changes for idle coworkers come from daemon nudges, not real work.
        let decisions = IdleShutdownCtx::one("york").run();
        assert_eq!(
            decisions.len(),
            1,
            "idle coworkers with no tasks should be sent on break regardless of pane activity"
        );
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_allows_coworker_with_ci_passed_pr_to_break() {
        // Bug #4: Coworkers waiting for PR review (CI passed) should go on break.
        let decisions = IdleShutdownCtx::one("york")
            .open_prs(&["york"])
            .ci_passed(&["york"])
            .run();
        assert_eq!(
            decisions.len(),
            1,
            "coworkers with CI-passed PRs should be sent on break (waiting for review)"
        );
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_usage_limited_coworker() {
        let decisions = IdleShutdownCtx::one("york").usage_limited(&["york"]).run();
        assert!(
            decisions.is_empty(),
            "usage-limited coworker should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_api_error_coworker() {
        let decisions = IdleShutdownCtx::one("york").api_error(&["york"]).run();
        assert!(
            decisions.is_empty(),
            "API error coworker should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_pending_assigned_task() {
        // Bug #753 (Bug 2): Coworkers with pending tasks should be protected.
        let decisions = IdleShutdownCtx::one("lexington")
            .pending_tasks(&["lexington"])
            .run();
        assert!(
            decisions.is_empty(),
            "coworker with pending task assigned should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_review_feedback_pr() {
        // Bug #753 (Bug 1): CI passed + review feedback → protect (prevents spawn→idle→break loop).
        let decisions = IdleShutdownCtx::one("madison")
            .open_prs(&["madison"])
            .ci_passed(&["madison"])
            .review_feedback(&["madison"])
            .run();
        assert!(
            decisions.is_empty(),
            "coworker with CI-passed PR and review feedback should be protected from \
             idle shutdown (prevents spawn→idle→break loop)"
        );
    }

    #[test]
    fn idle_shutdown_still_allows_break_for_ci_passed_pr_without_feedback() {
        let decisions = IdleShutdownCtx::one("york")
            .open_prs(&["york"])
            .ci_passed(&["york"])
            .run();
        assert_eq!(
            decisions.len(),
            1,
            "coworker with CI-passed PR but no review feedback should still go on break"
        );
    }

    #[test]
    fn idle_shutdown_never_shuts_down_the_lead() {
        let decisions = IdleShutdownCtx::one("lead").run();
        assert!(
            decisions.is_empty(),
            "The lead session should never be idle-shutdown"
        );
    }

    // -----------------------------------------------------------------------
    // PR action helper and tests (comment/issue/review with handoff)
    // -----------------------------------------------------------------------

    fn active(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pr_comment_handoff_hands_off_active_busy_owner() {
        // Active-but-busy owner (not in idle list) should be handed off
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]), // york not idle — busy on another task
            false,
            Some(&session),
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_hands_off_when_owner_inactive() {
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["amsterdam"]), // york not active
            &active(&["amsterdam"]), // amsterdam idle
            false,
            Some(&session),
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_spawns_owner_when_no_session() {
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            false,
            None, // no session
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::SpawnOwner {
                owner: "york".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_skips_self_comment() {
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "york", // self-comment
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]),
            false,
            Some(&session),
            "review feedback",
        );
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    #[test]
    fn pr_comment_handoff_skips_at_dev_limit() {
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            true, // at dev limit
            Some(&session),
            "review feedback",
        );
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_review_complete_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn review_complete_nudges_active_idle_owner() {
        let action = decide_review_complete_action(
            "york",
            &active(&["york"]),
            &active(&["york"]), // york is idle too
            false,
            "review complete",
        );
        assert!(matches!(action, PrAction::NudgeOwner { .. }));
    }

    #[test]
    fn review_complete_spawns_inactive_owner() {
        let action = decide_review_complete_action(
            "york",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            false,
            "review complete",
        );
        assert!(matches!(action, PrAction::SpawnOwner { .. }));
    }

    #[test]
    fn review_complete_skips_at_dev_limit() {
        let action = decide_review_complete_action(
            "york",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            true,
            "review complete",
        );
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    #[test]
    fn review_complete_nudges_when_owner_active_but_busy() {
        // york is active but not idle — should nudge, not spawn.
        // Spawning an already-active coworker fails ("call-in failed")
        // because they already have a running session.
        let action = decide_review_complete_action(
            "york",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]), // york is NOT idle
            false,
            "review complete",
        );
        assert!(matches!(action, PrAction::NudgeOwner { .. }));
    }

    fn make_session_context(owner: &str, pr_number: u64) -> PrSessionContext {
        PrSessionContext {
            session_id: format!("session-{}", pr_number),
            branch: format!("{}/feature", owner),
            original_author: owner.to_string(),
            pr_number,
        }
    }

    #[test]
    fn pr_handoff_hands_off_active_busy_owner_with_session() {
        // Active-but-busy owner (not in idle list) should be handed off, not nudged
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]), // york not idle — busy on another task
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_hands_off_to_idle_coworker_when_owner_inactive() {
        // When owner is inactive and session exists, hand off to idle coworker
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["amsterdam"]), // york not active
            &active(&["amsterdam"]), // amsterdam is idle
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_spawns_owner_when_no_session() {
        // Without session context, fall back to spawning the original owner
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            false,
            None, // no session
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::SpawnOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_spawns_owner_when_no_idle_coworkers() {
        // Even with session, if no idle coworkers, spawn the original owner
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["amsterdam"]), // york not active
            &active(&[]),            // no idle coworkers
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::SpawnOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_skips_original_owner_in_idle_list() {
        // Should not hand off to the original owner (defeats the purpose)
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&[]),                    // no active coworkers
            &active(&["york", "amsterdam"]), // both in idle list
            false,
            Some(&session),
            "fix checks",
        );
        // Should pick amsterdam, not york
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_skips_at_dev_limit() {
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            true, // at dev limit
            Some(&session),
            "fix checks",
        );
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    #[test]
    fn pr_handoff_posts_to_channel_no_owner() {
        let action = decide_pr_issue_action_with_handoff(
            "",
            &active(&["amsterdam"]),
            &active(&["amsterdam"]),
            false,
            None,
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::PostToChannel {
                message: "fix checks".to_string(),
            }
        );
    }

    // -----------------------------------------------------------------------
    // Active-but-busy owner tests (issue #759)
    //
    // When a coworker is active (running session) but busy on a different
    // task, they should NOT be nudged about old PRs. Instead, the daemon
    // should use the handoff/spawn path.
    // -----------------------------------------------------------------------

    #[test]
    fn pr_handoff_uses_handoff_when_owner_active_but_busy() {
        // york is active but busy (not in idle list) — should hand off, not nudge
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["york", "amsterdam"]), // york is active
            &active(&["amsterdam"]),         // york is NOT idle (busy on another task)
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_nudges_owner_when_active_busy_no_idle() {
        // york is active but busy, no idle coworkers — should nudge, not spawn.
        // Spawning an already-active coworker fails ("call-in failed") because
        // they already have a running session.
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["york", "amsterdam"]), // york is active
            &active(&[]),                    // no idle coworkers
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_handoff_nudges_active_idle_owner() {
        // york is active AND idle — should still nudge (they're available)
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["york", "amsterdam"]), // york is active
            &active(&["york", "amsterdam"]), // york is also idle
            false,
            Some(&session),
            "fix checks",
        );
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_uses_handoff_when_owner_active_but_busy() {
        // york is active but busy — should hand off, not nudge
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["york", "amsterdam"]), // york is active
            &active(&["amsterdam"]),         // york is NOT idle
            false,
            Some(&session),
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::HandoffToCoworker {
                assignee: "amsterdam".to_string(),
                original_author: "york".to_string(),
                pr_number: 42,
                branch: "york/feature".to_string(),
                session_id: "session-42".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_nudges_active_idle_owner() {
        // york is active AND idle — should nudge
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["york", "amsterdam"]), // york is active
            &active(&["york", "amsterdam"]), // york is also idle
            false,
            Some(&session),
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_handoff_nudges_active_busy_no_idle() {
        // york is active but busy, no idle coworkers — should nudge, not spawn.
        // Spawning an already-active coworker fails ("call-in failed").
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["york", "amsterdam"]), // york is active
            &active(&[]),                    // no idle coworkers
            false,
            Some(&session),
            "review feedback",
        );
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn review_complete_nudges_active_busy_owner() {
        // Owner is active but busy — should nudge, not spawn.
        // Spawning an already-active coworker fails ("call-in failed").
        let action = decide_review_complete_action(
            "york",
            &active(&["york", "amsterdam"]), // york is active
            &active(&[]),                    // york is NOT idle
            false,
            "review done, please address",
        );
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "review done, please address".to_string(),
            }
        );
    }

    // -----------------------------------------------------------------------
    // decide_pending_task_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn pending_task_nudges_active_owner() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, false, false, false);
        assert!(matches!(action, PendingTaskAction::NudgeOwner { .. }));
    }

    #[test]
    fn pending_task_skips_nudge_on_cooldown() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, true, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_spawns_inactive_owner() {
        let names = set(&["amsterdam"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, false, false, false);
        assert_eq!(
            action,
            PendingTaskAction::SpawnOwner {
                owner: "york".to_string(),
                task_id: "42".to_string(),
                task_subject: "Fix bug".to_string(),
            }
        );
    }

    #[test]
    fn pending_task_skips_at_dev_limit() {
        let names = set(&["amsterdam"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, true, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_lead_owner() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "lead", &names, false, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_empty_owner() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "", &names, false, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_invalid_coworker_name() {
        // "fix" is not a valid coworker name (not an avenue name)
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "fix", &names, false, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_owner_with_in_progress_task() {
        // Bug: coworker york has an in_progress task (#832). The daemon creates
        // a new task (#835) and assigns it to york. York now has two in_progress
        // tasks, violating the one-task-per-coworker invariant.
        //
        // Fix: skip task assignment for coworkers that already have an in_progress task.
        let names = set(&[]);
        let action = decide_pending_task_action(
            "835",
            "Fix false orphan recovery",
            "york",
            &names,
            false,
            false,
            false,
            true, // has_in_progress_task = true
        );
        assert!(
            matches!(action, PendingTaskAction::Skip { .. }),
            "Should not assign a new task to a coworker that already has an in_progress task"
        );
    }

    #[test]
    fn pending_task_spawns_owner_without_in_progress_task() {
        // Normal case: owner has no in_progress tasks, should be spawned
        let names = set(&[]);
        let action = decide_pending_task_action(
            "835",
            "Fix false orphan recovery",
            "york",
            &names,
            false,
            false,
            false,
            false, // has_in_progress_task = false
        );
        assert!(
            matches!(action, PendingTaskAction::SpawnOwner { .. }),
            "Should spawn owner when they have no in_progress task"
        );
    }

    // -----------------------------------------------------------------------
    // Builder for decide_orphan_recovery — eliminates 7-arg boilerplate
    // -----------------------------------------------------------------------

    /// Test context builder for `decide_orphan_recovery`.
    ///
    /// All sets default to empty; callers only set the fields they care about.
    #[derive(Default)]
    struct OrphanCtx {
        tasks: Vec<(String, String, String)>,
        active: HashSet<String>,
        at_dev_limit: bool,
        open_prs: HashSet<String>,
        review_feedback: HashSet<String>,
        recently_stopped: HashSet<String>,
        attached: HashSet<String>,
    }

    impl OrphanCtx {
        /// Start with a single task owned by `owner`.
        fn task(id: &str, subject: &str, owner: &str) -> Self {
            Self {
                tasks: vec![(id.to_string(), subject.to_string(), owner.to_string())],
                ..Default::default()
            }
        }

        fn tasks(mut self, tasks: Vec<(String, String, String)>) -> Self {
            self.tasks = tasks;
            self
        }
        fn active(mut self, names: &[&str]) -> Self {
            self.active = set(names);
            self
        }
        fn at_dev_limit(mut self) -> Self {
            self.at_dev_limit = true;
            self
        }
        fn open_prs(mut self, names: &[&str]) -> Self {
            self.open_prs = set(names);
            self
        }
        fn review_feedback(mut self, names: &[&str]) -> Self {
            self.review_feedback = set(names);
            self
        }
        fn recently_stopped(mut self, names: &[&str]) -> Self {
            self.recently_stopped = set(names);
            self
        }
        fn attached(mut self, names: &[&str]) -> Self {
            self.attached = set(names);
            self
        }
        fn run(&self) -> Option<OrphanRecovery> {
            let ctx = OrphanRecoveryContext {
                in_progress: &self.tasks,
                active_names: &self.active,
                at_dev_limit: self.at_dev_limit,
                coworkers_with_open_prs: &self.open_prs,
                review_feedback_pr_coworkers: &self.review_feedback,
                recently_stopped: &self.recently_stopped,
                attached_coworkers: &self.attached,
            };
            decide_orphan_recovery(&ctx)
        }
    }

    fn task(id: &str, subject: &str, owner: &str) -> (String, String, String) {
        (id.to_string(), subject.to_string(), owner.to_string())
    }

    // -----------------------------------------------------------------------
    // decide_orphan_recovery tests
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_recovery_finds_orphan() {
        let result = OrphanCtx::task("1", "Fix bug", "york")
            .active(&["amsterdam"])
            .run();
        assert_eq!(
            result,
            Some(OrphanRecovery {
                task_id: "1".to_string(),
                task_subject: "Fix bug".to_string(),
                owner: "york".to_string(),
            })
        );
    }

    #[test]
    fn orphan_recovery_skips_active_owner() {
        let result = OrphanCtx::task("1", "Fix bug", "york")
            .active(&["york"])
            .run();
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_at_dev_limit() {
        let result = OrphanCtx::task("1", "Fix bug", "york")
            .active(&["amsterdam"])
            .at_dev_limit()
            .run();
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_lead_owner() {
        let result = OrphanCtx::task("1", "Fix bug", "lead")
            .active(&["amsterdam"])
            .run();
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_returns_first_only() {
        let result = OrphanCtx::default()
            .tasks(vec![
                task("1", "Fix bug", "york"),
                task("2", "Add test", "broadway"),
            ])
            .active(&["amsterdam"])
            .run();
        assert_eq!(result.unwrap().task_id, "1");
    }

    #[test]
    fn orphan_recovery_skips_invalid_coworker_name() {
        // Bug: task with invalid owner "fix" (not an avenue name) should be skipped
        let result = OrphanCtx::task("42", "Fix bug", "fix")
            .active(&["amsterdam"])
            .run();
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_handles_uppercase_owner() {
        let result = OrphanCtx::task("1", "Fix bug", "YORK")
            .active(&["amsterdam"])
            .run();
        assert!(result.is_some());
        assert_eq!(result.unwrap().owner, "YORK");
    }

    #[test]
    fn orphan_recovery_skips_coworker_awaiting_review() {
        // Bug: coworker opened a PR with green CI and is awaiting review.
        let result = OrphanCtx::task("789", "Add usage bars", "amsterdam")
            .open_prs(&["amsterdam"])
            .recently_stopped(&["amsterdam"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker awaiting review on green PR"
        );
    }

    #[test]
    fn orphan_recovery_recovers_coworker_with_review_feedback() {
        let result = OrphanCtx::task("789", "Add usage bars", "amsterdam")
            .open_prs(&["amsterdam"])
            .review_feedback(&["amsterdam"])
            .run();
        assert!(result.is_some());
        assert_eq!(result.unwrap().task_id, "789");
    }

    #[test]
    fn orphan_recovery_skips_coworker_with_failed_ci_and_open_pr() {
        // CI failures are handled by webhook/PR poll, not orphan recovery.
        let result = OrphanCtx::task("789", "Add usage bars", "amsterdam")
            .open_prs(&["amsterdam"])
            .recently_stopped(&["amsterdam"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR (CI failures handled separately)"
        );
    }

    #[test]
    fn orphan_recovery_recovers_coworker_without_pr() {
        let result = OrphanCtx::task("789", "Add usage bars", "amsterdam").run();
        assert!(result.is_some());
        assert_eq!(result.unwrap().task_id, "789");
    }

    #[test]
    fn orphan_recovery_skips_coworker_with_open_pr_before_ci_cached() {
        // Bug: lexington recovery loop (task !810) — orphan check fires before
        // PR poll has cached CI status.
        let result = OrphanCtx::task("810", "Fix auth endpoint", "lexington")
            .open_prs(&["lexington"])
            .recently_stopped(&["lexington"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even when CI status is not yet cached"
        );
    }

    #[test]
    fn orphan_recovery_skips_multi_task_coworker_with_open_pr_before_ci() {
        // Bug: coworker has TWO in_progress tasks and open PR before CI cached
        let result = OrphanCtx::default()
            .tasks(vec![
                task("810", "Fix auth endpoint", "lexington"),
                task("811", "Address review feedback", "lexington"),
            ])
            .open_prs(&["lexington"])
            .recently_stopped(&["lexington"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even when CI status is not yet cached"
        );
    }

    #[test]
    fn orphan_recovery_skips_recently_stopped_coworker() {
        // Bug: coworker completes work, goes idle, gets shut down. Task still
        // in_progress. Grace period prevents false recovery.
        let result = OrphanCtx::task("832", "Review feedback", "york")
            .recently_stopped(&["york"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker that recently stopped (within grace period)"
        );
    }

    #[test]
    fn orphan_recovery_recovers_after_grace_period() {
        let result = OrphanCtx::task("832", "Review feedback", "york").run();
        assert!(
            result.is_some(),
            "Should recover coworker after grace period expires"
        );
        assert_eq!(result.unwrap().task_id, "832");
    }

    /// Regression test for #874: RPC idle handler false orphan recovery.
    #[test]
    fn orphan_recovery_skips_coworker_that_just_reported_idle() {
        let result = OrphanCtx::task("861", "Review PR #705", "madison")
            .recently_stopped(&["madison"])
            .run();
        assert!(
            result.is_none(),
            "Should NOT recover coworker that just reported idle (recently stopped)"
        );
    }

    /// Regression test for #874: verify false recovery WOULD occur without stop time.
    #[test]
    fn orphan_recovery_false_positive_without_stop_time() {
        let result = OrphanCtx::task("861", "Review PR #705", "madison").run();
        assert!(
            result.is_some(),
            "Without stop time recording, orphan recovery falsely triggers (the bug)"
        );
        assert_eq!(result.unwrap().owner, "madison");
    }

    /// Regression test for #874: auth switch shuts down multiple coworkers.
    #[test]
    fn orphan_recovery_skips_coworkers_shut_down_by_auth_switch() {
        let result = OrphanCtx::default()
            .tasks(vec![
                task("861", "Review PR #705", "madison"),
                task("862", "Fix auth bug", "park"),
            ])
            .recently_stopped(&["madison", "park"])
            .run();
        assert!(
            result.is_none(),
            "Should NOT recover coworkers shut down by auth switch (recently stopped)"
        );
    }

    // -----------------------------------------------------------------------
    // CooldownTracker spawn failure tests
    // -----------------------------------------------------------------------

    #[test]
    fn spawn_failure_cooldown_blocks_retry() {
        let mut tracker = CooldownTracker::new();
        let cooldown = Duration::from_secs(120);

        // Before any failure, check passes
        assert!(tracker.check("spawn_failure", "park", cooldown));

        // Record a spawn failure
        tracker.record("spawn_failure", "park");

        // Now the cooldown blocks retries for "park"
        assert!(!tracker.check("spawn_failure", "park", cooldown));

        // But other coworkers are not affected
        assert!(tracker.check("spawn_failure", "broadway", cooldown));
    }

    #[test]
    fn spawn_failure_cooldown_expires() {
        let mut tracker = CooldownTracker::new();

        // Record a failure, then manually insert an old timestamp
        tracker.record("spawn_failure", "park");

        // Overwrite with an old instant (3 minutes ago > 120s cooldown)
        tracker.entries.insert(
            ("spawn_failure".to_string(), "park".to_string()),
            Instant::now() - Duration::from_secs(180),
        );

        assert!(tracker.check("spawn_failure", "park", Duration::from_secs(120)));
    }

    #[test]
    fn has_entry_returns_false_when_no_entry() {
        let tracker = CooldownTracker::new();
        assert!(!tracker.has_entry("api_error_nudge", "york"));
    }

    #[test]
    fn has_entry_returns_true_after_record() {
        let mut tracker = CooldownTracker::new();
        tracker.record("api_error_nudge", "york");
        assert!(tracker.has_entry("api_error_nudge", "york"));
    }

    #[test]
    fn has_entry_returns_true_even_when_cooldown_expired() {
        let mut tracker = CooldownTracker::new();
        tracker.record("api_error_nudge", "york");

        // Overwrite with an old timestamp (entry expired but still exists)
        tracker.entries.insert(
            ("api_error_nudge".to_string(), "york".to_string()),
            Instant::now() - Duration::from_secs(300),
        );

        // has_entry returns true because entry still exists (even if expired)
        // This is intentional - cleanup removes entries, not expiration
        assert!(tracker.has_entry("api_error_nudge", "york"));
    }

    // -----------------------------------------------------------------------
    // decide_mention_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn mention_nudges_running_coworker() {
        let action = decide_mention_action("york", "amsterdam", true, false, "hey york");
        assert_eq!(
            action,
            MentionAction::Nudge {
                name: "york".to_string(),
                message: "hey york".to_string(),
            }
        );
    }

    #[test]
    fn mention_spawns_inactive_coworker() {
        let action = decide_mention_action("york", "amsterdam", false, false, "hey york");
        assert_eq!(
            action,
            MentionAction::Spawn {
                name: "york".to_string(),
                message: "hey york".to_string(),
            }
        );
    }

    #[test]
    fn mention_skips_self_mention() {
        let action = decide_mention_action("york", "york", true, false, "hey @york");
        assert!(matches!(action, MentionAction::Skip { .. }));
    }

    #[test]
    fn mention_skips_at_dev_limit() {
        let action = decide_mention_action("york", "amsterdam", false, true, "hey york");
        assert!(matches!(action, MentionAction::Skip { .. }));
    }

    /// Snapshot from bug #756: madison was in a break/respawn loop for 4+ hours.
    /// She had an open PR (#649) with CI passed AND review feedback. The fix
    /// (PR #650) added review_feedback_pr_coworkers protection.
    #[test]
    fn snapshot_20260205_madison_break_loop_protected_by_review_feedback() {
        let fixture = include_str!(
            "../tests/fixtures/snapshot/snapshot-madison-break-loop-pr-not-merging-20260205-130328.json"
        );
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();

        // Verify snapshot state
        let active = &snapshot["active_coworkers"];
        assert!(
            active
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "madison"),
            "madison should be in active_coworkers"
        );

        // Extract sets from snapshot
        let coworkers_with_open_prs: HashSet<String> = snapshot["coworkers_with_open_prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let ci_passed: HashSet<String> = snapshot["ci_passed_pr_coworkers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(coworkers_with_open_prs.contains("madison"));
        assert!(ci_passed.contains("madison"));

        let decisions = IdleShutdownCtx::one("madison")
            .open_prs(
                &coworkers_with_open_prs
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>(),
            )
            .ci_passed(&ci_passed.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .review_feedback(&["madison"])
            .run();

        assert!(
            decisions.is_empty(),
            "madison should NOT be sent on break when she has review feedback on her PR. \
             This caused the break/respawn loop in bug #756 (4 duplicate sessions). \
             Decisions: {:?}",
            decisions
        );
    }

    // -----------------------------------------------------------------------
    // decide_stuck_coworker_restarts tests (ProcessHealth-based)
    // -----------------------------------------------------------------------

    /// Run `decide_stuck_coworker_restarts` with a single health entry and task.
    fn run_stuck_check(
        name: &str,
        health: crate::daemon::snapshot::ProcessHealth,
        now: DateTime<Utc>,
        usage_limited: &HashSet<String>,
        api_error: &HashSet<String>,
        attached: &HashSet<String>,
    ) -> Vec<StuckCoworkerRestart> {
        let mut map = HashMap::new();
        map.insert(name.to_string(), health);
        let tasks = vec![("42".to_string(), "Fix bug".to_string(), name.to_string())];
        let exemptions = StuckExemptions {
            usage_limited,
            api_error,
            auth_error: &HashSet::new(),
            attached,
        };
        decide_stuck_coworker_restarts(&map, &tasks, &exemptions, now, Duration::from_secs(180))
    }

    #[test]
    fn stuck_detection_triggers_for_no_events() {
        let now = Utc::now();
        let restarts = run_stuck_check(
            "riverside",
            stuck_health(now),
            now,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0].name, "riverside");
    }

    #[test]
    fn stuck_detection_skips_recent_events() {
        let now = Utc::now();
        let mut h = stuck_health(now);
        h.last_event_at = Some(now - chrono::Duration::seconds(30));
        let restarts = run_stuck_check(
            "riverside",
            h,
            now,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "recent events should not trigger stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_usage_limited() {
        let now = Utc::now();
        let restarts = run_stuck_check(
            "york",
            stuck_health(now),
            now,
            &set(&["york"]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "usage-limited coworker should be skipped"
        );
    }

    #[test]
    fn stuck_detection_skips_exempt_mixed_case() {
        let now = Utc::now();
        // Set stores lowercase "lexington", but coworker name has mixed case.
        // hashset_contains_icase should still match via O(1) lowercase lookup.
        let restarts = run_stuck_check(
            "Lexington",
            stuck_health(now),
            now,
            &set(&["lexington"]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "mixed-case coworker should be recognized as exempt"
        );
    }

    #[test]
    fn stuck_detection_skips_api_error() {
        let now = Utc::now();
        let restarts = run_stuck_check(
            "madison",
            stuck_health(now),
            now,
            &HashSet::new(),
            &set(&["madison"]),
            &HashSet::new(),
        );
        assert!(restarts.is_empty(), "API error coworker should be skipped");
    }

    #[test]
    fn stuck_detection_skips_running_subagent() {
        let now = Utc::now();
        let mut h = stuck_health(now);
        h.has_running_subagent = true;
        let restarts = run_stuck_check(
            "park",
            h,
            now,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "coworker with running subagent should not be flagged as stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_dead_processes() {
        let now = Utc::now();
        let mut h = stuck_health(now);
        h.is_alive = false;
        h.exit_code = Some(1);
        let restarts = run_stuck_check(
            "broadway",
            h,
            now,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "dead processes are handled by check_and_respawn_dead_processes"
        );
    }

    #[test]
    fn stuck_detection_skips_attached_coworkers() {
        let now = Utc::now();
        let restarts = run_stuck_check(
            "park",
            stuck_health(now),
            now,
            &HashSet::new(),
            &HashSet::new(),
            &set(&["park"]),
        );
        assert!(
            restarts.is_empty(),
            "attached coworker should not be flagged as stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_pending_tool_execution() {
        let now = Utc::now();
        let mut h = stuck_health(now);
        h.has_pending_tool = true;
        let restarts = run_stuck_check(
            "broadway",
            h,
            now,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "coworker with pending tool execution should not be flagged as stuck"
        );
    }

    #[test]
    fn orphan_recovery_skips_attached_coworkers() {
        let result = OrphanCtx::task("1", "Fix bug", "york")
            .active(&["amsterdam"])
            .attached(&["york"])
            .run();
        assert!(
            result.is_none(),
            "attached coworker should not be treated as orphan"
        );
    }

    #[test]
    fn orphan_recovery_skips_killed_coworker_with_open_pr() {
        // Killed coworker with open PR — work is done, task should be auto-completed
        // by PR management pathway, not orphan recovery.
        let result = OrphanCtx::task("952", "Fix PR handling", "broadway")
            .open_prs(&["broadway"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover killed coworker if PR is open (work already done)"
        );
    }

    #[test]
    fn orphan_recovery_skips_recently_stopped_coworker_awaiting_review() {
        let result = OrphanCtx::task("952", "Fix PR handling", "broadway")
            .open_prs(&["broadway"])
            .recently_stopped(&["broadway"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker who recently stopped and is awaiting review"
        );
    }

    #[test]
    fn orphan_recovery_skips_coworker_after_grace_period_with_open_pr() {
        // Regression test for task !1011: amsterdam opens PR #810, goes idle,
        // grace period expires → no longer in recently_stopped. Without the
        // open-PR check, orphan recovery fires → infinite loop.
        let result = OrphanCtx::task("1008", "Add web UI channel switching", "amsterdam")
            .open_prs(&["amsterdam"])
            .run();
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even after grace period (creates loop)"
        );
    }

    // -----------------------------------------------------------------------
    // decide_dead_process_respawns tests
    // -----------------------------------------------------------------------

    /// Helper to create a ProcessHealth entry for a dead process.
    fn dead_health(exit_code: i32) -> crate::daemon::snapshot::ProcessHealth {
        crate::daemon::snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(exit_code),
            last_event_at: Some(Utc::now() - chrono::Duration::seconds(60)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
        }
    }

    #[test]
    fn dead_process_respawns_with_in_progress_task() {
        let mut health = HashMap::new();
        health.insert("york".to_string(), dead_health(1));

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

        let respawns = decide_dead_process_respawns(&health, &tasks);

        assert_eq!(respawns.len(), 1);
        assert_eq!(respawns[0].name, "york");
        assert_eq!(respawns[0].task_id, "42");
        assert_eq!(respawns[0].task_subject, "Fix bug");
        assert_eq!(respawns[0].exit_code, 1);
    }

    #[test]
    fn dead_process_without_task_not_respawned() {
        let mut health = HashMap::new();
        health.insert("madison".to_string(), dead_health(0));

        let tasks: Vec<(String, String, String)> = vec![];

        let respawns = decide_dead_process_respawns(&health, &tasks);

        assert!(
            respawns.is_empty(),
            "dead process without task should not be respawned"
        );
    }

    #[test]
    fn alive_process_not_respawned() {
        let mut health = HashMap::new();
        health.insert(
            "broadway".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: true,
                exit_code: None,
                last_event_at: Some(Utc::now() - chrono::Duration::seconds(10)),
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
            },
        );

        let tasks = vec![(
            "99".to_string(),
            "Review PR".to_string(),
            "broadway".to_string(),
        )];

        let respawns = decide_dead_process_respawns(&health, &tasks);

        assert!(respawns.is_empty(), "alive process should not be respawned");
    }

    #[test]
    fn dead_process_without_exit_code_not_respawned() {
        let mut health = HashMap::new();
        health.insert(
            "amsterdam".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: None, // No exit code yet
                last_event_at: Some(Utc::now() - chrono::Duration::seconds(5)),
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
            },
        );

        let tasks = vec![(
            "33".to_string(),
            "Add test".to_string(),
            "amsterdam".to_string(),
        )];

        let respawns = decide_dead_process_respawns(&health, &tasks);

        assert!(
            respawns.is_empty(),
            "dead process without exit code should not be respawned (not fully exited yet)"
        );
    }

    #[test]
    fn dead_process_case_insensitive_task_match() {
        let mut health = HashMap::new();
        health.insert("Lexington".to_string(), dead_health(137));

        // Task owner is lowercase, but health entry is mixed case
        let tasks = vec![(
            "7".to_string(),
            "Refactor".to_string(),
            "lexington".to_string(),
        )];

        let respawns = decide_dead_process_respawns(&health, &tasks);

        assert_eq!(
            respawns.len(),
            1,
            "should match task owner case-insensitively"
        );
        assert_eq!(respawns[0].name, "Lexington");
    }

    // -----------------------------------------------------------------------
    // decide_pr_owner_resume_mode tests
    // -----------------------------------------------------------------------

    #[test]
    fn pr_owner_with_saved_session_resumes() {
        let mut sessions = HashMap::new();
        sessions.insert("columbus".to_string(), "session-abc-123".to_string());

        let mode = decide_pr_owner_resume_mode("columbus", &sessions);

        assert_eq!(
            mode,
            PrOwnerResumeMode::WithSavedSession("session-abc-123".to_string())
        );
    }

    #[test]
    fn pr_owner_without_saved_session_fresh_resume() {
        let sessions: HashMap<String, String> = HashMap::new();

        let mode = decide_pr_owner_resume_mode("riverside", &sessions);

        assert_eq!(mode, PrOwnerResumeMode::WithoutSavedSession);
    }

    #[test]
    fn pr_owner_resume_case_sensitive_match() {
        // Session keys are case-sensitive (unlike coworker name matching elsewhere)
        let mut sessions = HashMap::new();
        sessions.insert("Lexington".to_string(), "session-xyz-789".to_string());

        // Exact match
        let mode = decide_pr_owner_resume_mode("Lexington", &sessions);
        assert_eq!(
            mode,
            PrOwnerResumeMode::WithSavedSession("session-xyz-789".to_string())
        );

        // Different case — no match (HashMap uses exact key match)
        let mode = decide_pr_owner_resume_mode("lexington", &sessions);
        assert_eq!(mode, PrOwnerResumeMode::WithoutSavedSession);
    }

    // -----------------------------------------------------------------------
    // decide_pending_task_action tests (reviewer handling)
    // -----------------------------------------------------------------------

    #[test]
    fn pending_task_action_skips_active_reviewer() {
        // Active reviewers should NOT be nudged about main task list updates.
        let active_names: HashSet<String> = ["madison".to_string()].into_iter().collect();

        // Main task !6 has owner="madison", but madison is an active reviewer
        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "madison",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            true,  // IS active reviewer
            false, // no in_progress task
        );

        assert!(
            matches!(action, PendingTaskAction::Skip { .. }),
            "active reviewer should be skipped for main task list updates, got: {:?}",
            action
        );

        // Verify the skip reason mentions reviewer
        if let PendingTaskAction::Skip { reason } = action {
            assert!(
                reason.contains("reviewer"),
                "skip reason should mention reviewer: {}",
                reason
            );
        }
    }

    #[test]
    fn pending_task_action_nudges_non_reviewer_coworker() {
        // Non-reviewer coworkers SHOULD be nudged about their pending tasks
        let active_names: HashSet<String> = ["york".to_string()].into_iter().collect();

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "york",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            false, // NOT a reviewer
            false, // no in_progress task
        );

        assert!(
            matches!(action, PendingTaskAction::NudgeOwner { .. }),
            "non-reviewer coworker should be nudged, got: {:?}",
            action
        );
    }

    #[test]
    fn pending_task_action_spawns_non_reviewer_inactive_owner() {
        // Inactive non-reviewer owners should be spawned
        let active_names: HashSet<String> = HashSet::new(); // york is not active

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "york",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            false, // NOT a reviewer
            false, // no in_progress task
        );

        assert!(
            matches!(action, PendingTaskAction::SpawnOwner { .. }),
            "inactive non-reviewer owner should be spawned, got: {:?}",
            action
        );
    }

    #[test]
    fn pending_task_action_skips_reviewer_inactive_owner() {
        // Reviewer check fires before active check.
        // An inactive reviewer owner should still be skipped, not spawned.
        let active_names: HashSet<String> = HashSet::new(); // madison is NOT active

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "madison",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            true,  // IS reviewer (even though inactive)
            false, // no in_progress task
        );

        assert!(
            matches!(action, PendingTaskAction::Skip { .. }),
            "inactive reviewer owner should still be skipped, got: {:?}",
            action
        );

        // Verify the skip reason mentions reviewer
        if let PendingTaskAction::Skip { reason } = action {
            assert!(
                reason.contains("reviewer"),
                "skip reason should mention reviewer: {}",
                reason
            );
        }
    }

    // -----------------------------------------------------------------------
    // decide_stuck_reviewer_restarts tests
    // -----------------------------------------------------------------------

    /// Run `decide_stuck_reviewer_restarts` with a single reviewer entry.
    fn run_stuck_reviewer_check(
        name: &str,
        health: crate::daemon::snapshot::ProcessHealth,
        pr_number: u64,
        now: DateTime<Utc>,
        restart_counts: &HashMap<u64, u32>,
        usage_limited: &HashSet<String>,
    ) -> Vec<StuckReviewerRestart> {
        let mut map = HashMap::new();
        map.insert(name.to_string(), health);
        let mut assignments = HashMap::new();
        assignments.insert(name.to_string(), pr_number);
        let exemptions = StuckExemptions {
            usage_limited,
            api_error: &HashSet::new(),
            auth_error: &HashSet::new(),
            attached: &HashSet::new(),
        };
        decide_stuck_reviewer_restarts(
            &map,
            &assignments,
            restart_counts,
            &exemptions,
            now,
            Duration::from_secs(300),
            2,
        )
    }

    #[test]
    fn stuck_reviewer_detected() {
        let now = Utc::now();
        let restarts = run_stuck_reviewer_check(
            "riverside",
            stuck_health(now),
            42,
            now,
            &HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0].name, "riverside");
        assert_eq!(restarts[0].pr_number, 42);
        assert_eq!(restarts[0].restart_count, 0);
    }

    #[test]
    fn stuck_reviewer_skipped_usage_limited() {
        let now = Utc::now();
        let restarts = run_stuck_reviewer_check(
            "york",
            stuck_health(now),
            42,
            now,
            &HashMap::new(),
            &set(&["york"]),
        );
        assert!(
            restarts.is_empty(),
            "usage-limited reviewer should be skipped"
        );
    }

    #[test]
    fn stuck_reviewer_skipped_subagent() {
        let now = Utc::now();
        let mut h = stuck_health(now);
        h.has_running_subagent = true;
        let restarts =
            run_stuck_reviewer_check("park", h, 42, now, &HashMap::new(), &HashSet::new());
        assert!(
            restarts.is_empty(),
            "reviewer with running subagent should be skipped"
        );
    }

    #[test]
    fn stuck_reviewer_max_restarts_stops_loop() {
        let now = Utc::now();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(42u64, 2u32);
        let restarts = run_stuck_reviewer_check(
            "broadway",
            stuck_health(now),
            42,
            now,
            &restart_counts,
            &HashSet::new(),
        );
        assert!(
            restarts.is_empty(),
            "reviewer at max restarts should not be flagged (loop broken)"
        );
    }

    #[test]
    fn stuck_reviewer_no_assignment_not_flagged() {
        let now = Utc::now();
        let mut map = HashMap::new();
        map.insert("madison".to_string(), stuck_health(now));
        let exemptions = StuckExemptions {
            usage_limited: &HashSet::new(),
            api_error: &HashSet::new(),
            auth_error: &HashSet::new(),
            attached: &HashSet::new(),
        };
        let restarts = decide_stuck_reviewer_restarts(
            &map,
            &HashMap::new(), // no reviewer assignment
            &HashMap::new(),
            &exemptions,
            now,
            Duration::from_secs(300),
            2,
        );
        assert!(
            restarts.is_empty(),
            "coworker without PR assignment should not be flagged"
        );
    }

    #[test]
    fn set_workflow_none_progress_preserves_existing() {
        // Bug: hook fires coworker_report_state with progress=None, which
        // unconditionally overwrites previously-reported progress.
        let mut records = HashMap::new();

        // First call: coworker reports 50% progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(50),
        );
        assert_eq!(records["york"].progress, Some(50));

        // Second call: hook fires with progress=None (should preserve 50%)
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            None,
        );
        assert_eq!(
            records["york"].progress,
            Some(50),
            "progress should be preserved when caller passes None"
        );
    }

    #[test]
    fn set_workflow_explicit_zero_resets_progress() {
        // Explicitly passing Some(0) should reset progress to 0.
        let mut records = HashMap::new();

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(75),
        );
        assert_eq!(records["york"].progress, Some(75));

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Testing,
            Some(42),
            Some(0),
        );
        assert_eq!(
            records["york"].progress,
            Some(0),
            "explicit Some(0) should reset progress"
        );
    }

    #[test]
    fn set_workflow_phase_change_without_progress_clears_it() {
        // When switching to a new phase (e.g., developing → pull-request)
        // without providing progress, the old progress should be cleared
        // because it's from a different phase.
        let mut records = HashMap::new();

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(80),
        );

        // Phase changes to pull-request without progress — should clear
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::PullRequest,
            Some(42),
            None,
        );
        assert_eq!(
            records["york"].progress, None,
            "progress should be cleared on phase change even with None"
        );
    }
}
