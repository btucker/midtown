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
/// were last recorded. Prefer [`check_and_record`](CooldownTracker::check_and_record)
/// to atomically test and claim a cooldown slot — using separate `check()` then
/// `record()` calls introduces a TOCTOU window where concurrent callers can both
/// see the cooldown as expired.
///
/// Currently used in DaemonState's `cooldowns` field.
#[derive(Debug, Default)]
pub(crate) struct CooldownTracker {
    entries: HashMap<(String, String), Instant>,
}

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

    /// Returns `true` and records the current instant if the cooldown has expired (or was never
    /// recorded). Returns `false` without mutating state if the cooldown is still active.
    ///
    /// Equivalent to `if check(…) { record(…); true } else { false }`.
    pub fn check_and_record(&mut self, rule_name: &str, key: &str, duration: Duration) -> bool {
        if self.check(rule_name, key, duration) {
            self.record(rule_name, key);
            true
        } else {
            false
        }
    }

    /// Removes entries whose cooldown has long expired (2× duration),
    /// preventing unbounded growth.
    pub fn cleanup(&mut self, max_age: Duration) {
        self.entries.retain(|_k, v| v.elapsed() < max_age);
    }

    /// Number of tracked entries (test diagnostics).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tracker has no entries (test diagnostics).
    #[cfg(test)]
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

/// Default progress percentage for a workflow phase transition.
///
/// Returns `Some(pct)` for phases that represent meaningful milestones, or
/// `None` for phases like Idle and Debugging that don't map to progress.
fn phase_default_progress(phase: crate::coworker_state::WorkflowPhase) -> Option<u8> {
    use crate::coworker_state::WorkflowPhase;
    match phase {
        WorkflowPhase::Claiming => Some(5),
        WorkflowPhase::Developing => Some(25),
        WorkflowPhase::Testing => Some(65),
        WorkflowPhase::PullRequest => Some(85),
        WorkflowPhase::Reviewing => Some(50),
        WorkflowPhase::Completed => Some(100),
        WorkflowPhase::Idle | WorkflowPhase::Debugging => None,
    }
}

/// Update the workflow phase for a coworker (from RPC state report).
///
/// When `progress` is `None` and the phase changes, injects the default
/// progress for the new phase so time estimates have data points even when
/// coworkers don't explicitly report. Explicit `--progress` values always
/// override phase defaults. Within the same phase, `None` preserves existing
/// progress (e.g., hook fires without progress info).
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

    let effective_progress = progress.or_else(|| {
        if phase_changed {
            // Phase default should only advance progress, never regress it.
            // E.g. PullRequest (85%) → Reviewing (default 50%) keeps 85%.
            phase_default_progress(phase).map(|d| d.max(record.progress.unwrap_or(0)))
        } else {
            None
        }
    });

    match effective_progress {
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
            // Phase changed to one with no default (Idle, Debugging) — clear progress
            record.progress = None;
            record.progress_history.clear();
        }
        None => {} // preserve existing progress within same phase
    }
    record.workflow_updated_at = Some(now);
}

// ---------------------------------------------------------------------------
// Detection types and functions
// ---------------------------------------------------------------------------

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
// Dead process detection
// ---------------------------------------------------------------------------

/// A coworker whose process has died and should be respawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadProcessRespawn {
    pub name: String,
    pub task_id: String,
    pub task_subject: String,
    pub exit_code: i32,
    /// Session ID from the name→session map, if the coworker had an active session.
    /// Used for traceability in shutdown effects and logging.
    pub session_id: Option<String>,
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
    name_session_map: &HashMap<String, String>,
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
            session_id: name_session_map.get(name).cloned(),
        });
    }

    respawns
}

// ---------------------------------------------------------------------------
// Dead fork detection (test-only pure function)
// ---------------------------------------------------------------------------
//
// In production, fork crash recovery is handled in the session_drain handler
// (mod.rs) which captures fork bindings before cleanup_coworker_state removes
// them. This pure function is kept for unit testing the detection logic.

/// A fork session whose process has died and should be respawned.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadForkRespawn {
    /// Name of the dead fork process.
    pub name: String,
    /// Thread parent ID the fork was bound to.
    pub thread_parent_id: String,
    /// Session ID of the dead fork.
    pub session_id: String,
    /// Exit code of the dead process.
    pub exit_code: i32,
    /// Channel the fork was in.
    pub channel: Option<String>,
    /// Working directory of the fork.
    pub working_dir: Option<String>,
    /// Auth provider for the fork.
    pub auth_provider: crate::auth::AuthProvider,
    /// Whether the fork was a channel lead.
    pub is_channel_lead: bool,
}

/// Detect fork sessions whose headless process has exited unexpectedly.
///
/// A fork is considered dead if:
/// - It appears in `topic_sessions` (thread_parent_id → session_id)
/// - Its corresponding session record has a `current_name`
/// - That name's process has `is_alive == false`
///
/// Unlike task-bound coworkers, forks are thread-bound — they don't appear in
/// `in_progress_tasks`, so `decide_dead_process_respawns()` misses them.
///
/// Pure function: takes snapshot data and returns respawn decisions.
/// Only used by tests — production detection is in session_drain handler.
#[cfg(test)]
pub(crate) fn decide_dead_fork_respawns(
    topic_sessions: &HashMap<String, String>,
    sessions: &HashMap<String, crate::daemon::state::SessionRecord>,
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
) -> Vec<DeadForkRespawn> {
    let mut respawns = Vec::new();

    for (thread_parent_id, session_id) in topic_sessions {
        // Look up the session record for this fork
        let Some(record) = sessions.get(session_id) else {
            continue;
        };

        // Get the fork's process name
        let Some(name) = record
            .current_name
            .as_ref()
            .or(record.preferred_name.as_ref())
        else {
            continue;
        };

        // Check if the process is dead.
        // Note: `exit_code` is not reliably populated by `collect_health()` (always None
        // for running sessions). Use `is_alive` as the primary dead-process indicator.
        let Some(health) = process_health.get(name) else {
            continue;
        };
        if health.is_alive {
            continue;
        }

        let working_dir = if record.working_dir.is_empty() {
            None
        } else {
            Some(record.working_dir.clone())
        };

        respawns.push(DeadForkRespawn {
            name: name.clone(),
            thread_parent_id: thread_parent_id.clone(),
            session_id: session_id.clone(),
            exit_code: health.exit_code.unwrap_or(-1),
            channel: record.channel.clone(),
            working_dir,
            auth_provider: record.provider.unwrap_or(crate::auth::AuthProvider::Claude),
            is_channel_lead: record.coworker_type == "channel-lead",
        });
    }

    respawns
}

/// Maximum number of times a fork session will be respawned for the same thread.
/// After this many attempts, the daemon stops respawning and cleans up the
/// topic_session entry so thread replies fall back to the channel lead.
pub(crate) const MAX_FORK_RESPAWN_ATTEMPTS: u32 = 3;

/// Check whether a fork respawn is allowed given the current attempt count.
/// Returns `true` if the count is below [`MAX_FORK_RESPAWN_ATTEMPTS`].
///
/// Pure function — the caller is responsible for looking up and incrementing
/// the count in `DaemonState::fork_respawn_counts`.
pub(crate) fn is_fork_respawn_allowed(current_count: u32) -> bool {
    current_count < MAX_FORK_RESPAWN_ATTEMPTS
}

// ---------------------------------------------------------------------------
// Dead reviewer detection
// ---------------------------------------------------------------------------

/// A reviewer that needs to be respawned (either dead or previously stuck).
///
/// Used as the return type for both `decide_dead_reviewer_respawns` and
/// as the respawn record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StuckReviewerRestart {
    pub name: String,
    pub pr_number: u64,
    pub restart_count: u32,
    /// Session ID from the name→session map, if the reviewer has an active session.
    /// Used for traceability in shutdown effects and logging.
    pub session_id: Option<String>,
}

/// Detect reviewers whose process has exited without posting a review.
///
/// A reviewer is considered a dead unfinished reviewer if:
/// - `is_alive` is false (process exited)
/// - The coworker has an active reviewer PR assignment
/// - The assigned PR is NOT in `reviewed_prs` (review was not posted)
/// - The restart count is below `max_restarts`
///
/// This covers reviewers that exit due to max turns, rate limits, or
/// natural session end before completing and posting their review.
///
/// Pure function: takes ProcessHealth data and returns respawn decisions.
pub(crate) fn decide_dead_reviewer_respawns(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    reviewer_pr_assignments: &HashMap<String, u64>,
    reviewed_prs: &std::collections::HashSet<u64>,
    reviewer_restart_counts: &HashMap<u64, u32>,
    max_restarts: u32,
    name_session_map: &HashMap<String, String>,
    usage_limited_coworkers: &HashSet<String>,
) -> Vec<StuckReviewerRestart> {
    let mut respawns = Vec::new();

    for (name, health) in process_health {
        // Only handle dead processes — alive reviewers are handled by stuck detection.
        if health.is_alive {
            continue;
        }

        // Must have a reviewer PR assignment.
        let pr_number = match reviewer_pr_assignments.get(name) {
            Some(&pr) => pr,
            None => continue,
        };

        // Review was already posted — no need to respawn.
        if reviewed_prs.contains(&pr_number) {
            continue;
        }

        // Skip rate-limited reviewers — respawning into the same limit would fail
        // immediately and burn the restart budget.
        if hashset_contains_icase(usage_limited_coworkers, name) {
            continue;
        }

        // Check restart limit to prevent infinite loops.
        let current_count = reviewer_restart_counts
            .get(&pr_number)
            .copied()
            .unwrap_or(0);
        if current_count >= max_restarts {
            continue;
        }

        respawns.push(StuckReviewerRestart {
            name: name.clone(),
            pr_number,
            restart_count: current_count,
            session_id: name_session_map.get(name).cloned(),
        });
    }

    respawns
}

/// A dead reviewer that has hit the restart limit and needs ops escalation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadReviewerEscalation {
    pub name: String,
    pub pr_number: u64,
    pub restart_count: u32,
}

/// Identify dead reviewers that have exhausted their restart budget.
///
/// Returns escalation entries for reviewers where:
/// - The process has exited (`is_alive` is false)
/// - The reviewer has an active PR assignment
/// - The review was never posted
/// - The restart count is at or above `max_restarts`
/// - No escalation has been posted yet for this PR
///
/// This is the counterpart to `decide_dead_reviewer_respawns` — it catches
/// the cases that function filters out, ensuring ops is always notified
/// instead of PRs silently accumulating without review.
///
/// Pure function: takes snapshot data and returns escalation decisions.
pub(crate) fn decide_dead_reviewer_escalations(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    reviewer_pr_assignments: &HashMap<String, u64>,
    reviewed_prs: &std::collections::HashSet<u64>,
    reviewer_restart_counts: &HashMap<u64, u32>,
    reviewer_escalations_posted: &std::collections::HashSet<u64>,
    max_restarts: u32,
) -> Vec<DeadReviewerEscalation> {
    let mut escalations = Vec::new();

    for (name, health) in process_health {
        // Only escalate dead reviewers — alive ones are handled by the stuck path.
        if health.is_alive {
            continue;
        }

        // Must have a reviewer PR assignment.
        let pr_number = match reviewer_pr_assignments.get(name) {
            Some(&pr) => pr,
            None => continue,
        };

        // Review was already posted — no escalation needed.
        if reviewed_prs.contains(&pr_number) {
            continue;
        }

        // Only escalate when restart budget is exhausted.
        let restart_count = reviewer_restart_counts
            .get(&pr_number)
            .copied()
            .unwrap_or(0);
        if restart_count < max_restarts {
            continue;
        }

        // Skip if escalation already posted for this PR.
        if reviewer_escalations_posted.contains(&pr_number) {
            continue;
        }

        escalations.push(DeadReviewerEscalation {
            name: name.clone(),
            pr_number,
            restart_count,
        });
    }

    escalations
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

/// Returns true if a review comment on this PR's task should create a follow-up
/// task rather than trying to dispatch to the original coworker.
///
/// When a task is `Completed`, the coworker session has ended and the worktree
/// may be cleaned up. Trying to spawn or resume the original coworker with stale
/// session context is unreliable. Creating a new follow-up task lets the normal
/// dispatch system assign it to an available coworker with full context.
pub fn review_comment_creates_followup(task_status: &crate::tasks::TaskStatus) -> bool {
    matches!(task_status, crate::tasks::TaskStatus::Completed)
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
    is_channel_lead: bool,
) -> PendingTaskAction {
    // Skip empty, lead-owned, or channel-lead-owned tasks — these sessions
    // are not managed by the coworker dispatch loop.
    if owner.is_empty() || owner.eq_ignore_ascii_case("lead") || is_channel_lead {
        return PendingTaskAction::Skip {
            reason: format!("task !{} owner is lead, channel lead, or empty", task_id),
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
    pub attached_coworkers: &'a HashMap<String, chrono::DateTime<chrono::Utc>>,
    pub channel_lead_names: &'a HashSet<String>,
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
            || self.attached_coworkers.contains_key(owner_lower)
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
        // Channel leads are domain-expert leads for topic channels — they must
        // not be orphan-recovered (they manage themselves, like the lead).
        let is_valid_coworker = !owner_clean.is_empty()
            && !owner_clean.eq_ignore_ascii_case("lead")
            && !ctx.channel_lead_names.contains(&owner_lower)
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
///
/// `has_reviewer_session` indicates whether the mentioned name has an existing
/// reviewer session that can be resumed. Resuming a reviewer doesn't consume
/// a new dev slot, so the dev limit check is bypassed in that case.
pub(crate) fn decide_mention_action(
    mentioned_name: &str,
    sender: &str,
    is_running: bool,
    at_dev_limit: bool,
    has_reviewer_session: bool,
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
    } else if has_reviewer_session {
        // Reviewer resume: doesn't consume a dev slot, bypass limit check.
        MentionAction::Spawn {
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

#[path = "rules_session_tests.rs"]
#[cfg(test)]
mod rules_session_tests;

#[path = "rules_fork_tests.rs"]
#[cfg(test)]
mod rules_fork_tests;

#[path = "rules_channel_lead_tests.rs"]
#[cfg(test)]
mod rules_channel_lead_tests;

#[path = "rules_cooldown_tests.rs"]
#[cfg(test)]
mod rules_cooldown_tests;

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
            has_pending_api_call: false,
        }
    }

    #[test]
    fn dead_process_respawns_with_in_progress_task() {
        let mut health = HashMap::new();
        health.insert("york".to_string(), dead_health(1));

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

        let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

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

        let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

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
                has_pending_api_call: false,
            },
        );

        let tasks = vec![(
            "99".to_string(),
            "Review PR".to_string(),
            "broadway".to_string(),
        )];

        let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

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
                has_pending_api_call: false,
            },
        );

        let tasks = vec![(
            "33".to_string(),
            "Add test".to_string(),
            "amsterdam".to_string(),
        )];

        let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

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

        let respawns = decide_dead_process_respawns(&health, &tasks, &HashMap::new());

        assert_eq!(
            respawns.len(),
            1,
            "should match task owner case-insensitively"
        );
        assert_eq!(respawns[0].name, "Lexington");
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
            false, // not a channel lead
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
            false, // not a channel lead
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
            false, // not a channel lead
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
            false, // not a channel lead
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
    // decide_dead_reviewer_respawns tests
    // -----------------------------------------------------------------------

    #[test]
    fn dead_reviewer_detected_when_process_exits_without_review() {
        let now = Utc::now();
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                last_event_at: Some(now - chrono::Duration::minutes(5)),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new(); // review NOT posted

        let respawns = decide_dead_reviewer_respawns(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &HashMap::new(),
            2,
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(
            respawns.len(),
            1,
            "dead reviewer without review should be flagged for respawn"
        );
        assert_eq!(respawns[0].name, "riverside");
        assert_eq!(respawns[0].pr_number, 1352u64);
    }

    #[test]
    fn dead_reviewer_not_respawned_if_review_was_posted() {
        let now = Utc::now();
        let mut process_health = HashMap::new();
        process_health.insert(
            "columbus".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                last_event_at: Some(now - chrono::Duration::minutes(5)),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("columbus".to_string(), 1351u64);
        let mut reviewed_prs: HashSet<u64> = HashSet::new();
        reviewed_prs.insert(1351); // review WAS posted

        let respawns = decide_dead_reviewer_respawns(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &HashMap::new(),
            2,
            &HashMap::new(),
            &HashSet::new(),
        );

        assert!(
            respawns.is_empty(),
            "dead reviewer whose review was posted should NOT be respawned"
        );
    }

    #[test]
    fn dead_reviewer_not_respawned_at_max_restarts() {
        let now = Utc::now();
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                last_event_at: Some(now - chrono::Duration::minutes(5)),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 2u32); // already at max

        let respawns = decide_dead_reviewer_respawns(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            2,
            &HashMap::new(),
            &HashSet::new(),
        );

        assert!(
            respawns.is_empty(),
            "dead reviewer at max restarts should NOT be respawned"
        );
    }

    #[test]
    fn alive_reviewer_not_handled_by_dead_reviewer_respawns() {
        // Alive reviewers are handled by the stuck reviewer path, not this one.
        let now = Utc::now();
        let mut process_health = HashMap::new();
        process_health.insert(
            "park".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: true,
                exit_code: None,
                last_event_at: Some(now - chrono::Duration::hours(1)),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("park".to_string(), 42u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();

        let respawns = decide_dead_reviewer_respawns(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &HashMap::new(),
            2,
            &HashMap::new(),
            &HashSet::new(),
        );

        assert!(
            respawns.is_empty(),
            "alive reviewer should not be handled by dead reviewer detection"
        );
    }

    #[test]
    fn dead_reviewer_not_respawned_when_usage_limited() {
        let now = Utc::now();
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                last_event_at: Some(now - chrono::Duration::minutes(5)),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();

        // Riverside is rate-limited — respawning into the same limit would fail.
        let mut usage_limited = HashSet::new();
        usage_limited.insert("riverside".to_string());

        let respawns = decide_dead_reviewer_respawns(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &HashMap::new(),
            2,
            &HashMap::new(),
            &usage_limited,
        );

        assert!(
            respawns.is_empty(),
            "usage-limited dead reviewer should NOT be respawned"
        );
    }

    // -----------------------------------------------------------------------
    // decide_dead_reviewer_escalations tests
    // -----------------------------------------------------------------------

    #[test]
    fn dead_reviewer_at_max_restarts_escalates() {
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 2u32); // at max
        let escalations_posted: HashSet<u64> = HashSet::new();

        let escalations = decide_dead_reviewer_escalations(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            &escalations_posted,
            2,
        );

        assert_eq!(escalations.len(), 1);
        assert_eq!(escalations[0].name, "riverside");
        assert_eq!(escalations[0].pr_number, 1352u64);
        assert_eq!(escalations[0].restart_count, 2);
    }

    #[test]
    fn dead_reviewer_below_max_restarts_not_escalated() {
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 1u32); // below max
        let escalations_posted: HashSet<u64> = HashSet::new();

        let escalations = decide_dead_reviewer_escalations(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            &escalations_posted,
            2,
        );

        assert!(
            escalations.is_empty(),
            "reviewer below max restarts should not be escalated"
        );
    }

    #[test]
    fn dead_reviewer_escalation_skipped_if_already_posted() {
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 2u32);
        let mut escalations_posted: HashSet<u64> = HashSet::new();
        escalations_posted.insert(1352u64); // already posted

        let escalations = decide_dead_reviewer_escalations(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            &escalations_posted,
            2,
        );

        assert!(
            escalations.is_empty(),
            "escalation already posted should not be re-emitted"
        );
    }

    #[test]
    fn dead_reviewer_escalation_skipped_if_review_posted() {
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: false,
                exit_code: Some(0),
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let mut reviewed_prs: HashSet<u64> = HashSet::new();
        reviewed_prs.insert(1352u64); // review was posted
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 2u32);
        let escalations_posted: HashSet<u64> = HashSet::new();

        let escalations = decide_dead_reviewer_escalations(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            &escalations_posted,
            2,
        );

        assert!(
            escalations.is_empty(),
            "reviewer who posted review should not be escalated"
        );
    }

    #[test]
    fn alive_reviewer_not_escalated_by_dead_reviewer_escalations() {
        let mut process_health = HashMap::new();
        process_health.insert(
            "riverside".to_string(),
            crate::daemon::snapshot::ProcessHealth {
                is_alive: true, // alive — handled by stuck reviewer path
                exit_code: None,
                ..Default::default()
            },
        );
        let mut reviewer_pr_assignments = HashMap::new();
        reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);
        let reviewed_prs: HashSet<u64> = HashSet::new();
        let mut restart_counts = HashMap::new();
        restart_counts.insert(1352u64, 2u32);
        let escalations_posted: HashSet<u64> = HashSet::new();

        let escalations = decide_dead_reviewer_escalations(
            &process_health,
            &reviewer_pr_assignments,
            &reviewed_prs,
            &restart_counts,
            &escalations_posted,
            2,
        );

        assert!(
            escalations.is_empty(),
            "alive reviewer should not be escalated by dead reviewer path"
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
    fn set_workflow_phase_change_without_progress_injects_default() {
        // When switching to a new phase without providing explicit progress,
        // the default progress for that phase should be injected so time
        // estimates have data points even when coworkers don't report explicitly.
        let mut records = HashMap::new();

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(80),
        );

        // Phase changes to pull-request without progress — should inject default (85%)
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::PullRequest,
            Some(42),
            None,
        );
        assert_eq!(
            records["york"].progress,
            Some(85),
            "progress should be set to phase default (85% for pull-request) on phase change"
        );
    }

    #[test]
    fn set_workflow_phase_default_claiming() {
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Claiming,
            Some(42),
            None,
        );
        assert_eq!(records["york"].progress, Some(5));
    }

    #[test]
    fn set_workflow_phase_default_developing() {
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            None,
        );
        assert_eq!(records["york"].progress, Some(25));
    }

    #[test]
    fn set_workflow_phase_default_testing() {
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Testing,
            Some(42),
            None,
        );
        assert_eq!(records["york"].progress, Some(65));
    }

    #[test]
    fn set_workflow_phase_default_reviewing() {
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Reviewing,
            Some(42),
            None,
        );
        assert_eq!(records["york"].progress, Some(50));
    }

    #[test]
    fn set_workflow_phase_default_completed() {
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Completed,
            Some(42),
            None,
        );
        assert_eq!(records["york"].progress, Some(100));
    }

    #[test]
    fn set_workflow_explicit_progress_overrides_phase_default() {
        // When explicit progress is provided, it always wins over the default.
        let mut records = HashMap::new();
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(45),
        );
        assert_eq!(
            records["york"].progress,
            Some(45),
            "explicit progress should override phase default"
        );
    }

    #[test]
    fn set_workflow_phase_default_added_to_history() {
        // Phase-default progress should be recorded in progress_history
        // so time estimation can use it as a data point.
        let mut records = HashMap::new();

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Claiming,
            Some(42),
            None,
        );
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            None,
        );

        assert_eq!(
            records["york"].progress_history.len(),
            2,
            "both phase defaults should be in history for time estimation"
        );
        assert_eq!(records["york"].progress_history[0].0, 5);
        assert_eq!(records["york"].progress_history[1].0, 25);
    }

    #[test]
    fn set_workflow_idle_and_debugging_have_no_default() {
        // Idle and Debugging phases don't represent progress milestones,
        // so they shouldn't inject a default — progress should be cleared.
        let mut records = HashMap::new();

        // Start with some progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(40),
        );

        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Idle,
            None,
            None,
        );
        assert_eq!(
            records["york"].progress, None,
            "idle phase should clear progress"
        );
    }

    #[test]
    fn set_workflow_phase_default_does_not_regress_progress() {
        // Transitioning PullRequest (85%) → Reviewing (50%) without explicit
        // progress should NOT drop progress backwards.
        let mut records = HashMap::new();

        // Arrive at PullRequest with explicit 85% progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::PullRequest,
            Some(42),
            Some(85),
        );
        assert_eq!(records["york"].progress, Some(85));

        // Transition to Reviewing (default 50%) without explicit progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Reviewing,
            Some(42),
            None,
        );
        // Phase default (50%) must not overwrite higher existing progress (85%)
        assert_eq!(
            records["york"].progress,
            Some(85),
            "phase default should not regress progress below existing value"
        );
    }

    #[test]
    fn set_workflow_phase_default_advances_low_progress() {
        // If existing progress is lower than the phase default, the default
        // should advance it (the normal case of forward progress).
        let mut records = HashMap::new();

        // Coworker starts in Developing with explicit 30% progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Developing,
            Some(42),
            Some(30),
        );

        // Transition to Testing (default 65%) without explicit progress
        set_workflow(
            &mut records,
            "york",
            crate::coworker_state::WorkflowPhase::Testing,
            Some(42),
            None,
        );
        // Phase default (65%) > existing (30%) → should advance to 65
        assert_eq!(
            records["york"].progress,
            Some(65),
            "phase default should advance progress when higher than existing"
        );
    }
}
