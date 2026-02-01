//! Pure decision functions and shared types for the daemon tick loop.
//!
//! Each `decide_*` function takes pre-collected state snapshots and returns
//! a decision enum or struct — no side effects, no async, fully testable.
//!
//! The [`CooldownTracker`] provides a unified cooldown mechanism.
//! The [`CoworkerPhase`] enum tracks per-coworker lifecycle state.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Lightweight snapshot of a coworker at a point in time.
#[derive(Debug, Clone)]
pub(crate) struct CoworkerSnapshot {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub isolated_tasks: bool,
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
}

// ---------------------------------------------------------------------------
// CoworkerPhase — the per-coworker state machine
// ---------------------------------------------------------------------------

/// The current phase of a coworker in the daemon's lifecycle.
///
/// Replaces three separate HashMaps (`idle_since`, `interrupted_since`,
/// `prompted_nudged`) with a single enum per coworker. A coworker can only
/// be in one phase at a time — the enum enforces mutual exclusivity.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CoworkerPhase {
    /// Coworker has no tasks and is waiting for the idle timeout to expire.
    Idle { since: Instant },
    /// Coworker's session shows an interruption marker; waiting for the
    /// nudge timeout to fire.
    Interrupted { since: Instant },
    /// Coworker's session is blocked on an interactive prompt (plan approval,
    /// permission dialog, etc.). The fingerprint deduplicates nudges for the
    /// same prompt.
    Prompted { fingerprint: String },
}

/// Consolidated per-coworker lifecycle state.
///
/// Bundles the coworker's current phase (idle/interrupted/prompted) and their
/// last channel activity timestamp into a single entry. This replaces the
/// separate `coworker_phases` and `last_coworker_activity` HashMaps with one
/// `HashMap<String, CoworkerLifecycle>`, ensuring both are cleared together
/// on spawn and shutdown.
#[derive(Debug, Clone)]
pub(crate) struct CoworkerLifecycle {
    /// Current lifecycle phase (idle, interrupted, prompted), or `None` if
    /// the coworker is actively working (no special phase).
    pub phase: Option<CoworkerPhase>,
    /// When the coworker last posted to the channel. `None` if no activity
    /// has been recorded yet (e.g., freshly spawned).
    pub last_activity: Option<Instant>,
}

impl CoworkerLifecycle {
    /// Create a fresh lifecycle entry for a newly spawned coworker.
    pub fn new_spawn() -> Self {
        Self {
            phase: None,
            last_activity: Some(Instant::now()),
        }
    }
}

/// Get the current phase for a coworker, if any.
pub(crate) fn get_phase(
    lifecycles: &HashMap<String, CoworkerLifecycle>,
    name: &str,
) -> Option<CoworkerPhase> {
    lifecycles.get(name).and_then(|l| l.phase.clone())
}

/// Set the phase for a coworker, creating the lifecycle entry if needed.
pub(crate) fn set_phase(
    lifecycles: &mut HashMap<String, CoworkerLifecycle>,
    name: &str,
    phase: CoworkerPhase,
) {
    lifecycles
        .entry(name.to_string())
        .or_insert_with(|| CoworkerLifecycle {
            phase: None,
            last_activity: None,
        })
        .phase = Some(phase);
}

/// Clear the phase for a coworker (without removing the lifecycle entry).
pub(crate) fn clear_phase(lifecycles: &mut HashMap<String, CoworkerLifecycle>, name: &str) {
    if let Some(lc) = lifecycles.get_mut(name) {
        lc.phase = None;
    }
}

// ---------------------------------------------------------------------------
// Lifecycle decision types
// ---------------------------------------------------------------------------

/// Decision to shut down an idle coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownDecision {
    pub name: String,
    pub is_isolated: bool,
    /// When true, the daemon should save the coworker's session ID before shutdown
    /// so it can be resumed later (e.g., PR break-and-resume when CI passes).
    pub save_session: bool,
}

/// Decision to nudge an interrupted coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterruptNudge {
    pub name: String,
}

/// Decision to alert the lead about a prompted coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptNudge {
    pub name: String,
    pub label: String,
}

// ---------------------------------------------------------------------------
// Lifecycle decision functions (pure — no async, no side effects)
// ---------------------------------------------------------------------------

/// Decide which coworkers should be shut down due to idleness.
///
/// Takes pre-collected state snapshots and mutable idle tracking.
/// Returns shutdown decisions without performing any side effects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_idle_shutdowns(
    coworkers: &[CoworkerSnapshot],
    busy_coworkers: &HashSet<String>,
    coworkers_with_open_prs: &HashSet<String>,
    active_reviewers: &HashSet<String>,
    coworkers_with_unblocked_deps: &HashSet<String>,
    ci_passed_pr_coworkers: &HashSet<String>,
    lifecycles: &mut HashMap<String, CoworkerLifecycle>,
    now: Instant,
    now_utc: DateTime<Utc>,
    idle_break_duration: Duration,
    minimum_lifetime: Duration,
) -> Vec<ShutdownDecision> {
    let mut to_shutdown = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        // Check minimum lifetime
        let lifetime = now_utc.signed_duration_since(cw.started_at);
        if lifetime < chrono::Duration::from_std(minimum_lifetime).unwrap_or_default() {
            if matches!(
                get_phase(lifecycles, coworker),
                Some(CoworkerPhase::Idle { .. })
            ) {
                clear_phase(lifecycles, coworker);
            }
            continue;
        }

        let is_busy = busy_coworkers
            .iter()
            .any(|b| b.eq_ignore_ascii_case(coworker));
        let has_open_pr = coworkers_with_open_prs
            .iter()
            .any(|c| c.eq_ignore_ascii_case(coworker));
        let is_reviewing = active_reviewers
            .iter()
            .any(|r| r.eq_ignore_ascii_case(coworker));
        let has_unblocked_deps = coworkers_with_unblocked_deps
            .iter()
            .any(|d| d.eq_ignore_ascii_case(coworker));
        let has_ci_passed_pr = ci_passed_pr_coworkers
            .iter()
            .any(|c| c.eq_ignore_ascii_case(coworker));

        // PR owners with passing CI are eligible for break (save_session=true)
        if has_open_pr && has_ci_passed_pr && !is_busy && !is_reviewing && !has_unblocked_deps {
            to_shutdown.push(ShutdownDecision {
                name: coworker.clone(),
                is_isolated: cw.isolated_tasks,
                save_session: true,
            });
        } else if is_busy || has_open_pr || is_reviewing || has_unblocked_deps {
            if matches!(
                get_phase(lifecycles, coworker),
                Some(CoworkerPhase::Idle { .. })
            ) {
                clear_phase(lifecycles, coworker);
            }
        } else if cw.isolated_tasks {
            // Isolated coworkers (reviewers) go on break immediately when idle
            to_shutdown.push(ShutdownDecision {
                name: coworker.clone(),
                is_isolated: true,
                save_session: false,
            });
        } else {
            match get_phase(lifecycles, coworker) {
                Some(CoworkerPhase::Idle { since }) => {
                    if now.duration_since(since) >= idle_break_duration {
                        to_shutdown.push(ShutdownDecision {
                            name: coworker.clone(),
                            is_isolated: false,
                            save_session: false,
                        });
                    }
                }
                // Don't overwrite Interrupted or Prompted — those take priority
                Some(CoworkerPhase::Interrupted { .. } | CoworkerPhase::Prompted { .. }) => {}
                None => {
                    set_phase(lifecycles, coworker, CoworkerPhase::Idle { since: now });
                }
            }
        }
    }

    // Clear phase for shutdown coworkers (entry preserved for last_activity)
    for decision in &to_shutdown {
        clear_phase(lifecycles, &decision.name);
    }

    to_shutdown
}

/// Decide which coworkers should be nudged due to interrupted sessions.
///
/// Takes pane contents and mutable interruption tracking.
/// Returns nudge decisions without performing any side effects.
pub(crate) fn decide_interrupt_nudges(
    coworkers: &[CoworkerSnapshot],
    pane_contents: &HashMap<String, String>,
    lifecycles: &mut HashMap<String, CoworkerLifecycle>,
    now: Instant,
    nudge_duration: Duration,
) -> Vec<InterruptNudge> {
    let mut to_nudge = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        let pane_content = match pane_contents.get(coworker) {
            Some(content) => content,
            None => {
                if matches!(
                    get_phase(lifecycles, coworker),
                    Some(CoworkerPhase::Interrupted { .. })
                ) {
                    clear_phase(lifecycles, coworker);
                }
                continue;
            }
        };

        let is_interrupted = pane_content.contains("Interrupted")
            || pane_content.contains("What should Claude do instead?");

        if is_interrupted {
            match get_phase(lifecycles, coworker) {
                Some(CoworkerPhase::Interrupted { since }) => {
                    if now.duration_since(since) >= nudge_duration {
                        to_nudge.push(InterruptNudge {
                            name: coworker.clone(),
                        });
                        clear_phase(lifecycles, coworker);
                    }
                }
                _ => {
                    // Transition to Interrupted (overwriting Idle or absent)
                    set_phase(
                        lifecycles,
                        coworker,
                        CoworkerPhase::Interrupted { since: now },
                    );
                }
            }
        } else if matches!(
            get_phase(lifecycles, coworker),
            Some(CoworkerPhase::Interrupted { .. })
        ) {
            // No longer interrupted — clear the phase
            clear_phase(lifecycles, coworker);
        }
    }

    to_nudge
}

/// Decide which coworkers should trigger a lead prompt nudge.
///
/// Takes pane contents and mutable prompt tracking.
/// Returns nudge decisions without performing any side effects.
pub(crate) fn decide_prompt_nudges(
    coworkers: &[CoworkerSnapshot],
    pane_contents: &HashMap<String, String>,
    lifecycles: &mut HashMap<String, CoworkerLifecycle>,
) -> Vec<PromptNudge> {
    let mut to_nudge = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        // Skip the lead — they're the human
        if coworker == "lead" {
            continue;
        }

        let pane_content = match pane_contents.get(coworker) {
            Some(content) => content,
            None => {
                if matches!(
                    get_phase(lifecycles, coworker),
                    Some(CoworkerPhase::Prompted { .. })
                ) {
                    clear_phase(lifecycles, coworker);
                }
                continue;
            }
        };

        match detect_interactive_prompt(pane_content) {
            Some(label) => {
                let fingerprint = label.to_string();
                let already_nudged = matches!(
                    get_phase(lifecycles, coworker),
                    Some(CoworkerPhase::Prompted { fingerprint: prev }) if prev == fingerprint
                );
                if !already_nudged {
                    set_phase(
                        lifecycles,
                        coworker,
                        CoworkerPhase::Prompted { fingerprint },
                    );
                    to_nudge.push(PromptNudge {
                        name: coworker.clone(),
                        label: label.to_string(),
                    });
                }
            }
            None => {
                if matches!(
                    get_phase(lifecycles, coworker),
                    Some(CoworkerPhase::Prompted { .. })
                ) {
                    clear_phase(lifecycles, coworker);
                }
            }
        }
    }

    to_nudge
}

/// Patterns that indicate a coworker is waiting on an interactive prompt.
const INTERACTIVE_PROMPT_PATTERNS: &[(&str, &str)] = &[
    ("Yes, and don't ask again for this project", "plan approval"),
    ("Yes, and bypass permissions", "plan approval"),
    ("Yes, clear context and bypass permissions", "plan approval"),
    ("Do you want to proceed?", "confirmation prompt"),
    ("Would you like to proceed?", "confirmation prompt"),
    ("Allow once", "permission request"),
    ("Allow always", "permission request"),
    ("Select an option", "question prompt"),
];

/// Check if pane content contains an interactive prompt that needs human input.
pub(crate) fn detect_interactive_prompt(pane_content: &str) -> Option<&'static str> {
    for (pattern, label) in INTERACTIVE_PROMPT_PATTERNS {
        if pane_content.contains(pattern) {
            return Some(label);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Detection types and functions
// ---------------------------------------------------------------------------

/// Patterns that indicate a coworker has hit a usage/rate limit.
const USAGE_LIMIT_PATTERNS: &[&str] = &[
    "usage limit",
    "rate limit",
    "Usage limit reached",
    "rate_limit_error",
    "You've hit your",
    "limit resets",
];

/// Decision output for usage limit detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageLimitDecision {
    /// Usage limit detected in pane — schedule a nudge.
    Detected { coworker: String },
    /// No usage limit found in any pane.
    NoneDetected,
}

/// Decide whether pane contents indicate a usage limit.
///
/// Scans pane contents for known usage/rate limit patterns.
/// The caller is responsible for skipping this call when a nudge is already scheduled.
pub(crate) fn decide_usage_limit_detection(
    pane_contents: &HashMap<String, String>,
) -> UsageLimitDecision {
    for (name, content) in pane_contents {
        let has_limit = USAGE_LIMIT_PATTERNS
            .iter()
            .any(|p| content.to_lowercase().contains(&p.to_lowercase()));

        if has_limit {
            return UsageLimitDecision::Detected {
                coworker: name.clone(),
            };
        }
    }

    UsageLimitDecision::NoneDetected
}

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
// Blank-pane zombie detection
// ---------------------------------------------------------------------------

/// Identify coworkers with blank panes that have been running long enough
/// to rule out normal startup delays.
///
/// Returns the names of coworkers that should be respawned. A coworker is
/// considered a zombie if:
/// 1. Its pane is entirely blank (no terminal output)
/// 2. It has been running for at least `min_age` seconds
///
/// The age threshold prevents false positives during the ~3-8s window after
/// spawn where the TUI hasn't rendered yet.
pub(crate) fn detect_blank_pane_zombies(
    blank_pane_coworkers: &HashSet<String>,
    coworker_start_times: &HashMap<String, DateTime<Utc>>,
    now_utc: DateTime<Utc>,
    min_age: chrono::Duration,
) -> Vec<String> {
    blank_pane_coworkers
        .iter()
        .filter(|name| {
            // The lead window has its own health check (check_and_respawn_lead)
            // and must never be treated as a zombie coworker.
            if *name == "lead" {
                return false;
            }
            coworker_start_times
                .get(*name)
                .map(|started| now_utc.signed_duration_since(*started) >= min_age)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Try to parse a duration from usage limit text.
///
/// Looks for patterns like "try again in 15 minutes", "resets in 2 hours",
/// "available after 30 minutes". Returns a default of 15 minutes if no
/// parseable duration is found.
pub(crate) fn parse_usage_limit_duration(pane_content: &str) -> Duration {
    let lower = pane_content.to_lowercase();

    for keyword in &["in ", "after "] {
        let mut search_from = 0;
        while let Some(rel_idx) = lower[search_from..].find(keyword) {
            let idx = search_from + rel_idx;
            let after = &lower[idx + keyword.len()..];
            search_from = idx + keyword.len();

            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = num_str.parse::<u64>() {
                if num == 0 {
                    continue;
                }
                let remaining = after[num_str.len()..].trim_start();
                if remaining.starts_with("hour") {
                    return Duration::from_secs(num * 3600);
                } else if remaining.starts_with("min") {
                    return Duration::from_secs(num * 60);
                } else if remaining.starts_with("sec") {
                    return Duration::from_secs(num);
                }
            }
        }
    }

    // Look for HH:MM timestamp pattern like "resets at 3:45" or "at 15:30"
    if let Some(idx) = lower.find("at ") {
        let after = &lower[idx + 3..];
        let time_str: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ':')
            .collect();
        if let Some((h, m)) = time_str.split_once(':')
            && let (Ok(hour), Ok(min)) = (h.parse::<u32>(), m.parse::<u32>())
        {
            let now = chrono::Utc::now();
            let mut target = now
                .date_naive()
                .and_hms_opt(hour, min, 0)
                .unwrap_or_default();
            if target < now.naive_utc() {
                target += chrono::Duration::days(1);
            }
            let diff = target - now.naive_utc();
            if let Ok(std_diff) = diff.to_std() {
                return std_diff;
            }
        }
    }

    // Default: 15 minutes
    Duration::from_secs(15 * 60)
}

/// Check if pane content contains any usage/rate limit patterns.
///
/// Used directly in tests and indirectly via `decide_usage_limit_detection`.
#[allow(dead_code)]
pub(crate) fn has_usage_limit_pattern(pane_content: &str) -> bool {
    let lower = pane_content.to_lowercase();
    USAGE_LIMIT_PATTERNS
        .iter()
        .any(|p| lower.contains(&p.to_lowercase()))
}

// ---------------------------------------------------------------------------
// PR/review decision types and functions
// ---------------------------------------------------------------------------

/// Action to take for a PR issue or comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrAction {
    /// Owner is active — nudge them with a message.
    NudgeOwner { owner: String, message: String },
    /// Owner is inactive — spawn them with a message.
    SpawnOwner { owner: String, message: String },
    /// No identifiable owner — post to channel.
    PostToChannel { message: String },
    /// Skip — dev limit reached, self-comment, on cooldown, or no owner.
    Skip { reason: String },
}

/// Decide what action to take for a PR issue detected by polling.
///
/// Pure function: takes the issue context and returns a `PrAction`.
/// The caller handles side effects (nudge/spawn/post).
pub(crate) fn decide_pr_issue_action(
    owner: &str,
    active_coworkers: &[String],
    at_dev_limit: bool,
    message: &str,
) -> PrAction {
    let is_active = active_coworkers
        .iter()
        .any(|c| c.eq_ignore_ascii_case(owner));

    if is_active {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if !owner.is_empty() {
        if at_dev_limit {
            PrAction::Skip {
                reason: format!("dev limit reached, cannot spawn {} for PR issue", owner),
            }
        } else {
            PrAction::SpawnOwner {
                owner: owner.to_string(),
                message: message.to_string(),
            }
        }
    } else {
        PrAction::PostToChannel {
            message: message.to_string(),
        }
    }
}

/// Decide what action to take for a PR comment nudge (webhook-driven).
///
/// Pure function: determines whether to nudge, spawn, or skip based on
/// whether the owner is active and whether the comment is a self-comment.
pub(crate) fn decide_pr_comment_action(
    owner: &str,
    actor: &str,
    is_active: bool,
    at_dev_limit: bool,
    message: &str,
) -> PrAction {
    // Don't nudge about own comments
    if owner == actor {
        return PrAction::Skip {
            reason: format!("PR comment is from owner {}, skipping self-nudge", owner),
        };
    }

    if is_active {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if at_dev_limit {
        PrAction::Skip {
            reason: format!("dev limit reached, cannot spawn {} for PR comment", owner),
        }
    } else {
        PrAction::SpawnOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    }
}

/// Decide what action to take when a PR has a completed review and the
/// author needs to address feedback.
///
/// Same logic as `decide_pr_issue_action` — nudge if active, spawn if not,
/// skip if at dev limit.
pub(crate) fn decide_review_complete_action(
    owner: &str,
    active_coworkers: &[String],
    at_dev_limit: bool,
    message: &str,
) -> PrAction {
    let is_active = active_coworkers
        .iter()
        .any(|c| c.eq_ignore_ascii_case(owner));

    if is_active {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if at_dev_limit {
        PrAction::Skip {
            reason: format!(
                "dev limit reached, cannot spawn {} for review complete",
                owner
            ),
        }
    } else {
        PrAction::SpawnOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    }
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
pub(crate) fn decide_pending_task_action(
    task_id: &str,
    task_subject: &str,
    owner: &str,
    active_names: &HashSet<String>,
    at_dev_limit: bool,
    on_nudge_cooldown: bool,
) -> PendingTaskAction {
    // Skip empty or lead-owned tasks
    if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
        return PendingTaskAction::Skip {
            reason: format!("task #{} owner is lead or empty", task_id),
        };
    }

    // Owner is active → nudge (unless on cooldown)
    if active_names.contains(&owner.to_lowercase()) {
        if on_nudge_cooldown {
            return PendingTaskAction::Skip {
                reason: format!("task #{} nudge on cooldown for {}", task_id, owner),
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
                "dev limit reached, deferring spawn for task #{} owned by {}",
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

/// Decide which orphaned task (if any) to recover.
///
/// An orphaned task is `in_progress` but its owner is not active.
/// Returns at most ONE recovery action (rate-limited to one per tick).
pub(crate) fn decide_orphan_recovery(
    in_progress: &[(String, String, String)], // (task_id, task_subject, owner)
    active_names: &HashSet<String>,
    at_dev_limit: bool,
) -> Option<OrphanRecovery> {
    if at_dev_limit {
        return None;
    }

    for (task_id, task_subject, owner) in in_progress {
        let owner_clean = owner.trim().trim_matches('"').to_string();
        if owner_clean.is_empty() || owner_clean.eq_ignore_ascii_case("lead") {
            continue;
        }
        if active_names.contains(&owner_clean.to_lowercase()) {
            continue;
        }
        // Found an orphan — return the first one (rate-limited)
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

    // -----------------------------------------------------------------------
    // Helpers for lifecycle decision tests
    // -----------------------------------------------------------------------

    fn cw(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(minutes_old),
            isolated_tasks: false,
        }
    }

    fn cw_isolated(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(minutes_old),
            isolated_tasks: true,
        }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Create a lifecycle map with a single coworker in the given phase.
    fn lifecycle_with(name: &str, phase: CoworkerPhase) -> HashMap<String, CoworkerLifecycle> {
        let mut map = HashMap::new();
        map.insert(
            name.to_string(),
            CoworkerLifecycle {
                phase: Some(phase),
                last_activity: None,
            },
        );
        map
    }

    // -----------------------------------------------------------------------
    // decide_idle_shutdowns tests
    // -----------------------------------------------------------------------

    #[test]
    fn idle_shutdown_after_timeout() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(!decisions[0].is_isolated);
        assert!(!decisions[0].save_session);
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_busy_coworker() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
        // Busy coworker removed from idle tracking
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_active_reviewer() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_unblocked_deps() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
        // Coworker with unblocked deps removed from idle tracking
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_young_coworker() {
        let coworkers = vec![cw("york", 2)]; // Only 2 minutes old
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
        // Young coworker also removed from idle tracking
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_isolated_coworker_immediate() {
        let coworkers = vec![cw_isolated("reviewer", 10)];
        let mut phases: HashMap<String, CoworkerLifecycle> = HashMap::new();

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
        assert!(decisions[0].is_isolated);
    }

    #[test]
    fn idle_shutdown_starts_tracking_newly_idle() {
        let coworkers = vec![cw("york", 10)];
        let mut phases: HashMap<String, CoworkerLifecycle> = HashMap::new();

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        // No shutdown yet — just started tracking
        assert!(decisions.is_empty());
        assert!(get_phase(&phases, "york").is_some());
    }

    #[test]
    fn idle_shutdown_pr_break_with_ci_passed() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR AND CI is passing — should trigger PR break (save_session=true)
        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(decisions[0].save_session);
    }

    #[test]
    fn idle_shutdown_pr_break_not_triggered_without_ci() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR but CI has NOT passed — should NOT shutdown (protected by open PR)
        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_pr_break_not_triggered_if_busy() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has open PR, CI passed, but is busy — should NOT trigger break
        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&["york"]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &mut phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    // -----------------------------------------------------------------------
    // decide_interrupt_nudges tests
    // -----------------------------------------------------------------------

    #[test]
    fn interrupt_nudge_after_duration() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Some output\nInterrupted\n".to_string());
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Interrupted {
                since: Instant::now() - Duration::from_secs(90),
            },
        );

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut phases,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].name, "york");
        // Tracking reset after nudge
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn interrupt_nudge_not_yet_due() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Interrupted".to_string());
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Interrupted {
                since: Instant::now() - Duration::from_secs(10),
            },
        );

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut phases,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        // Still tracking
        assert!(get_phase(&phases, "york").is_some());
    }

    #[test]
    fn interrupt_tracking_cleared_when_no_longer_interrupted() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "All good, working fine".to_string());
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Interrupted {
                since: Instant::now() - Duration::from_secs(90),
            },
        );

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut phases,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        // Tracking cleared
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn interrupt_starts_tracking_newly_interrupted() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "What should Claude do instead?".to_string(),
        );
        let mut phases: HashMap<String, CoworkerLifecycle> = HashMap::new();

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut phases,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        assert!(get_phase(&phases, "york").is_some());
    }

    // -----------------------------------------------------------------------
    // decide_prompt_nudges tests
    // -----------------------------------------------------------------------

    #[test]
    fn prompt_nudge_new_prompt_detected() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "Some output\nAllow once\nAllow always".to_string(),
        );
        let mut phases: HashMap<String, CoworkerLifecycle> = HashMap::new();

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut phases);

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].name, "york");
        assert_eq!(nudges[0].label, "permission request");
        assert!(
            matches!(get_phase(&phases, "york"), Some(CoworkerPhase::Prompted { fingerprint }) if fingerprint == "permission request")
        );
    }

    #[test]
    fn prompt_nudge_skips_already_nudged_same_type() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Allow once\nAllow always".to_string());
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Prompted {
                fingerprint: "permission request".to_string(),
            },
        );

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut phases);

        assert!(nudges.is_empty());
    }

    #[test]
    fn prompt_nudge_fires_for_different_prompt_type() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "Yes, and don't ask again for this project".to_string(),
        );
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Prompted {
                fingerprint: "permission request".to_string(),
            },
        );

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut phases);

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].label, "plan approval");
    }

    #[test]
    fn prompt_nudge_clears_when_no_prompt() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Working normally".to_string());
        let mut phases = lifecycle_with(
            "york",
            CoworkerPhase::Prompted {
                fingerprint: "permission request".to_string(),
            },
        );

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut phases);

        assert!(nudges.is_empty());
        assert!(get_phase(&phases, "york").is_none());
    }

    #[test]
    fn prompt_nudge_skips_lead() {
        let coworkers = vec![CoworkerSnapshot {
            name: "lead".to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(10),
            isolated_tasks: false,
        }];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("lead".to_string(), "Allow once\nAllow always".to_string());
        let mut phases: HashMap<String, CoworkerLifecycle> = HashMap::new();

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut phases);

        assert!(nudges.is_empty());
    }

    // -----------------------------------------------------------------------
    // decide_pr_issue_action tests
    // -----------------------------------------------------------------------

    fn active(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pr_issue_nudges_active_owner() {
        let action =
            decide_pr_issue_action("york", &active(&["york", "amsterdam"]), false, "fix checks");
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_issue_spawns_inactive_owner() {
        let action = decide_pr_issue_action("york", &active(&["amsterdam"]), false, "fix checks");
        assert_eq!(
            action,
            PrAction::SpawnOwner {
                owner: "york".to_string(),
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_issue_skips_at_dev_limit() {
        let action = decide_pr_issue_action("york", &active(&["amsterdam"]), true, "fix checks");
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    #[test]
    fn pr_issue_posts_to_channel_no_owner() {
        let action = decide_pr_issue_action("", &active(&["amsterdam"]), false, "fix checks");
        assert_eq!(
            action,
            PrAction::PostToChannel {
                message: "fix checks".to_string(),
            }
        );
    }

    #[test]
    fn pr_issue_case_insensitive_active_check() {
        let action = decide_pr_issue_action("York", &active(&["york"]), false, "fix checks");
        assert!(matches!(action, PrAction::NudgeOwner { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_pr_comment_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn pr_comment_nudges_active_owner() {
        let action = decide_pr_comment_action("york", "amsterdam", true, false, "review feedback");
        assert_eq!(
            action,
            PrAction::NudgeOwner {
                owner: "york".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_spawns_inactive_owner() {
        let action = decide_pr_comment_action("york", "amsterdam", false, false, "review feedback");
        assert_eq!(
            action,
            PrAction::SpawnOwner {
                owner: "york".to_string(),
                message: "review feedback".to_string(),
            }
        );
    }

    #[test]
    fn pr_comment_skips_self_comment() {
        let action = decide_pr_comment_action("york", "york", true, false, "review feedback");
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    #[test]
    fn pr_comment_skips_at_dev_limit_when_inactive() {
        let action = decide_pr_comment_action("york", "amsterdam", false, true, "review feedback");
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_review_complete_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn review_complete_nudges_active_owner() {
        let action =
            decide_review_complete_action("york", &active(&["york"]), false, "review complete");
        assert!(matches!(action, PrAction::NudgeOwner { .. }));
    }

    #[test]
    fn review_complete_spawns_inactive_owner() {
        let action = decide_review_complete_action(
            "york",
            &active(&["amsterdam"]),
            false,
            "review complete",
        );
        assert!(matches!(action, PrAction::SpawnOwner { .. }));
    }

    #[test]
    fn review_complete_skips_at_dev_limit() {
        let action =
            decide_review_complete_action("york", &active(&["amsterdam"]), true, "review complete");
        assert!(matches!(action, PrAction::Skip { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_pending_task_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn pending_task_nudges_active_owner() {
        let names = set(&["york"]);
        let action = decide_pending_task_action("42", "Fix bug", "york", &names, false, false);
        assert!(matches!(action, PendingTaskAction::NudgeOwner { .. }));
    }

    #[test]
    fn pending_task_skips_nudge_on_cooldown() {
        let names = set(&["york"]);
        let action = decide_pending_task_action("42", "Fix bug", "york", &names, false, true);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_spawns_inactive_owner() {
        let names = set(&["amsterdam"]);
        let action = decide_pending_task_action("42", "Fix bug", "york", &names, false, false);
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
        let action = decide_pending_task_action("42", "Fix bug", "york", &names, true, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_lead_owner() {
        let names = set(&["york"]);
        let action = decide_pending_task_action("42", "Fix bug", "lead", &names, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_empty_owner() {
        let names = set(&["york"]);
        let action = decide_pending_task_action("42", "Fix bug", "", &names, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_orphan_recovery tests
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_recovery_finds_orphan() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
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
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["york"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_at_dev_limit() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, true);
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_lead_owner() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "lead".to_string())];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_returns_first_only() {
        let tasks = vec![
            ("1".to_string(), "Fix bug".to_string(), "york".to_string()),
            (
                "2".to_string(),
                "Add test".to_string(),
                "broadway".to_string(),
            ),
        ];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
        assert_eq!(result.unwrap().task_id, "1");
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

    // -----------------------------------------------------------------------
    // Blank-pane zombie detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn zombie_detection_flags_old_blank_coworker() {
        let mut blank = HashSet::new();
        blank.insert("york".to_string());

        let mut start_times = HashMap::new();
        let now = chrono::Utc::now();
        // Started 30 seconds ago — older than 20s threshold
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(30));

        let zombies =
            detect_blank_pane_zombies(&blank, &start_times, now, chrono::Duration::seconds(20));
        assert_eq!(zombies, vec!["york"]);
    }

    #[test]
    fn zombie_detection_skips_young_coworker() {
        let mut blank = HashSet::new();
        blank.insert("york".to_string());

        let mut start_times = HashMap::new();
        let now = chrono::Utc::now();
        // Started 5 seconds ago — younger than 20s threshold
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(5));

        let zombies =
            detect_blank_pane_zombies(&blank, &start_times, now, chrono::Duration::seconds(20));
        assert!(zombies.is_empty());
    }

    #[test]
    fn zombie_detection_skips_non_blank_coworker() {
        // Empty blank set — no zombies
        let blank = HashSet::new();

        let mut start_times = HashMap::new();
        let now = chrono::Utc::now();
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(60));

        let zombies =
            detect_blank_pane_zombies(&blank, &start_times, now, chrono::Duration::seconds(20));
        assert!(zombies.is_empty());
    }

    #[test]
    fn zombie_detection_skips_lead_window() {
        let mut blank = HashSet::new();
        blank.insert("lead".to_string());

        let mut start_times = HashMap::new();
        let now = chrono::Utc::now();
        // Lead has been running long enough to pass the age threshold
        start_times.insert("lead".to_string(), now - chrono::Duration::seconds(60));

        let zombies =
            detect_blank_pane_zombies(&blank, &start_times, now, chrono::Duration::seconds(20));
        assert!(
            zombies.is_empty(),
            "lead window must never be treated as a zombie"
        );
    }
}
