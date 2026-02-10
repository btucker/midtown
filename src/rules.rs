//! Pure decision functions and shared types for the daemon tick loop.
//!
//! Each `decide_*` function takes pre-collected state snapshots and returns
//! a decision enum or struct — no side effects, no async, fully testable.
//!
//! The [`CooldownTracker`] provides a unified cooldown mechanism.
//! The [`SessionHealth`] enum tracks per-coworker lifecycle state.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Lightweight snapshot of a coworker at a point in time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CoworkerSnapshot {
    pub name: String,
    pub started_at: DateTime<Utc>,
    /// Claude Code session UUID, if known. Enables session-first lookups
    /// alongside name-based lookups during the multi-session migration.
    pub session_id: Option<String>,
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

// ---------------------------------------------------------------------------
// SessionHealth — the per-coworker state machine
// ---------------------------------------------------------------------------

/// The current health state of a coworker's session.
///
/// A coworker can only be in one phase at a time — the enum enforces
/// mutual exclusivity. Pane scraping is used only for health checks
/// (stuck detection, zombie detection, usage limits), not workflow state.
///
/// NOTE: The Idle variant is preserved for potential future use but is not
/// currently constructed. Coworkers are now sent on break immediately when
/// unprotected, without any tracking delay.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum SessionHealth {
    /// Coworker has no tasks and is waiting for the idle timeout to expire.
    /// (Currently unused - coworkers go on break immediately)
    Idle { since: Instant },
}

/// Unified per-coworker record in daemon state.
///
/// Bundles the coworker's current health and their last channel activity
/// timestamp into a single entry, ensuring both are cleared together on
/// spawn and shutdown.
#[derive(Debug, Clone)]
pub(crate) struct CoworkerRecord {
    /// Current session health (idle), or `None` if the coworker is actively
    /// working (no special health state).
    pub health: Option<SessionHealth>,
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
}

impl CoworkerRecord {
    /// Create a fresh record entry for a newly spawned coworker.
    pub fn new_spawn() -> Self {
        Self {
            health: None,
            last_activity: Some(Instant::now()),
            workflow_phase: None,
            task_id: None,
            workflow_updated_at: None,
        }
    }

    /// Format for tmux tab display (e.g. "dev#42", "test#7").
    ///
    /// Note: Task ID 0 is treated as "no task" since it's often used as a
    /// placeholder for taskless work (e.g., PR reviews without a formal task).
    pub fn display_status(&self) -> Option<String> {
        self.workflow_phase.map(|phase| match self.task_id {
            Some(id) if id > 0 => format!("{}#{}", phase.abbreviation(), id),
            _ => phase.abbreviation().to_string(),
        })
    }
}

/// Get the current health state for a coworker, if any.
pub(crate) fn get_health(
    records: &HashMap<String, CoworkerRecord>,
    name: &str,
) -> Option<SessionHealth> {
    records.get(name).and_then(|r| r.health.clone())
}

/// Set the health state for a coworker, creating the record entry if needed.
pub(crate) fn set_health(
    records: &mut HashMap<String, CoworkerRecord>,
    name: &str,
    health: SessionHealth,
) {
    records
        .entry(name.to_string())
        .or_insert_with(|| CoworkerRecord {
            health: None,
            last_activity: None,
            workflow_phase: None,
            task_id: None,
            workflow_updated_at: None,
        })
        .health = Some(health);
}

/// Clear the health state for a coworker (without removing the record entry).
pub(crate) fn clear_health(records: &mut HashMap<String, CoworkerRecord>, name: &str) {
    if let Some(rec) = records.get_mut(name) {
        rec.health = None;
    }
}

/// Update the workflow phase for a coworker (from RPC state report).
pub(crate) fn set_workflow(
    records: &mut HashMap<String, CoworkerRecord>,
    name: &str,
    phase: crate::coworker_state::WorkflowPhase,
    task_id: Option<u32>,
) {
    let record = records
        .entry(name.to_string())
        .or_insert_with(|| CoworkerRecord {
            health: None,
            last_activity: None,
            workflow_phase: None,
            task_id: None,
            workflow_updated_at: None,
        });
    record.workflow_phase = Some(phase);
    record.task_id = task_id;
    record.workflow_updated_at = Some(chrono::Utc::now());
}

// ---------------------------------------------------------------------------
// Lifecycle decision types
// ---------------------------------------------------------------------------

/// A phase transition to apply after a decision function returns.
///
/// Decision functions return these alongside their primary decisions so the
/// caller can apply phase mutations *after* the pure decision is complete.
/// This keeps the decision functions free of mutation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HealthTransition {
    /// Set a coworker's phase to a new value.
    /// (Currently unused - coworkers go on break immediately without tracking)
    #[allow(dead_code)]
    Set { name: String, phase: SessionHealth },
    /// Clear a coworker's phase (set to None).
    Clear { name: String },
}

/// Apply a list of health state transitions to the record map.
pub(crate) fn apply_health_transitions(
    records: &mut HashMap<String, CoworkerRecord>,
    transitions: Vec<HealthTransition>,
) {
    for transition in transitions {
        match transition {
            HealthTransition::Set { name, phase } => {
                set_health(records, &name, phase);
            }
            HealthTransition::Clear { name } => {
                clear_health(records, &name);
            }
        }
    }
}

/// Decision to shut down an idle coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownDecision {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Lifecycle decision functions (pure — no async, no side effects)
// ---------------------------------------------------------------------------

/// Decide which coworkers should be shut down due to idleness.
///
/// Takes pre-collected state snapshots and immutable coworker records.
/// Returns shutdown decisions and health state transitions without performing
/// any side effects or mutations.
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_idle_shutdowns(
    coworkers: &[CoworkerSnapshot],
    busy_coworkers: &HashSet<String>,
    coworkers_with_open_prs: &HashSet<String>,
    active_reviewers: &HashSet<String>,
    coworkers_with_unblocked_deps: &HashSet<String>,
    ci_passed_pr_coworkers: &HashSet<String>,
    usage_limited_coworkers: &HashSet<String>,
    api_error_coworkers: &HashSet<String>,
    pending_task_owners: &HashSet<String>,
    review_feedback_pr_coworkers: &HashSet<String>,
    records: &HashMap<String, CoworkerRecord>,
    now_utc: DateTime<Utc>,
    minimum_lifetime: Duration,
) -> (Vec<ShutdownDecision>, Vec<HealthTransition>) {
    let mut to_shutdown = Vec::new();
    let mut transitions = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        // Check minimum lifetime
        let lifetime = now_utc.signed_duration_since(cw.started_at);
        if lifetime < chrono::Duration::from_std(minimum_lifetime).unwrap_or_default() {
            if matches!(
                get_health(records, coworker),
                Some(SessionHealth::Idle { .. })
            ) {
                transitions.push(HealthTransition::Clear {
                    name: coworker.clone(),
                });
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
        let ci_passed = ci_passed_pr_coworkers
            .iter()
            .any(|c| c.eq_ignore_ascii_case(coworker));
        let is_usage_limited = usage_limited_coworkers.contains(&coworker.to_lowercase());
        let has_api_error = api_error_coworkers.contains(&coworker.to_lowercase());
        let has_pending_task = pending_task_owners
            .iter()
            .any(|p| p.eq_ignore_ascii_case(coworker));
        let has_review_feedback = review_feedback_pr_coworkers
            .iter()
            .any(|r| r.eq_ignore_ascii_case(coworker));

        // Coworkers with active/pending tasks, review assignments, unblocked deps,
        // usage limits, or API errors are never sent on break.
        //
        // Coworkers with open PRs CAN go on break if their CI has passed
        // AND they have no review feedback to address. If they DO have review
        // feedback, they're protected (prevents spawn→idle→break loop from #753).
        let protected_by_open_pr = has_open_pr && (!ci_passed || has_review_feedback);
        if is_busy
            || has_pending_task
            || protected_by_open_pr
            || is_reviewing
            || has_unblocked_deps
            || is_usage_limited
            || has_api_error
        {
            if matches!(
                get_health(records, coworker),
                Some(SessionHealth::Idle { .. })
            ) {
                transitions.push(HealthTransition::Clear {
                    name: coworker.clone(),
                });
            }
        } else {
            // All unprotected coworkers go on break immediately.
            // The daemon will recall them when:
            // - A task is assigned to them
            // - Their PR gets review feedback
            // - A blocked task unblocks
            // This avoids coworkers sitting idle waiting for work that may
            // take a while (PR reviews, blocked dependencies, etc.)
            to_shutdown.push(ShutdownDecision {
                name: coworker.clone(),
            });
        }
    }

    // Clear phase for shutdown coworkers (entry preserved for last_activity)
    for decision in &to_shutdown {
        transitions.push(HealthTransition::Clear {
            name: decision.name.clone(),
        });
    }

    (to_shutdown, transitions)
}

// ---------------------------------------------------------------------------
// Detection types and functions
// ---------------------------------------------------------------------------

/// Patterns that indicate a coworker has hit a usage/rate limit (case-insensitive).
///
/// When Claude Code hits a usage limit, it displays a message with "/upgrade"
/// or "/extra-usage" as an action option. We look for contextual patterns to
/// avoid false positives when coworkers edit code containing these in strings:
/// - "- /upgrade" (menu option format in the usage limit screen)
/// - "/upgrade to" (instruction format: "/upgrade to increase your limit")
/// - "/upgrade or" (options format: "/upgrade or wait")
/// - "/extra-usage" (Claude Code v2.1.33+: "/extra-usage to finish what you're working on")
///
/// Previous patterns like "usage limit" caused false positives when coworkers
/// were editing code with those strings in comments.
const USAGE_LIMIT_PATTERNS: &[&str] = &["- /upgrade", "/upgrade to", "/upgrade or", "/extra-usage"];

/// Patterns that indicate a Claude API error in pane content.
///
/// API errors are transient failures (500s, network issues, etc.) that may resolve
/// on retry. Unlike usage limits which have a known reset time, API errors should
/// trigger periodic nudges to encourage retry.
///
/// Patterns detected:
/// - `API Error: 500` - HTTP 500 status code
/// - `"type":"api_error"` - JSON response type field
/// - `"type":"error"` with `api_error` - Structured error response
/// - `Internal server error` - Common error message
#[allow(dead_code)] // Used via has_api_error_pattern (pub(crate)), only called from tests currently
const API_ERROR_PATTERNS: &[&str] = &[
    "API Error: 500",
    "API Error: 502",
    "API Error: 503",
    "API Error: 529",
    r#""type":"api_error""#,
    r#""type":"overloaded_error""#,
    "Internal server error",
];

/// Check if pane content indicates an active (not recovered) usage limit.
///
/// Returns true if the usage limit pattern is present AND there's no significant
/// activity after it. If the coworker has recovered (substantial content after
/// the limit message), returns false.
///
/// Detection is case-insensitive to handle variations like "/Upgrade" or "/UPGRADE".
fn is_at_usage_limit(content: &str) -> bool {
    let content_lower = content.to_lowercase();

    // Find the last occurrence of any usage limit pattern (case-insensitive)
    let Some((limit_pos, pattern_len)) = USAGE_LIMIT_PATTERNS
        .iter()
        .filter_map(|pattern| {
            content_lower
                .rfind(&pattern.to_lowercase())
                .map(|pos| (pos, pattern.len()))
        })
        .max_by_key(|(pos, _)| *pos)
    else {
        return false;
    };

    // Get content after the usage limit message
    let after_limit = &content[limit_pos + pattern_len..];

    // Count significant lines after the limit
    // Skip: empty lines, UI chrome (box-drawing, bullets, prompts), horizontal rules
    let significant_lines: usize = after_limit
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !is_ui_chrome(trimmed)
        })
        .count();

    // If there are more than 5 significant lines after the limit, coworker has recovered
    // (typical Claude Code output has prompts, tool calls, status lines, etc.)
    significant_lines <= 5
}

/// Check if a line is UI chrome (visual elements, not meaningful content).
///
/// Filters out:
/// - Horizontal rules (─, ━, =, -)
/// - Box-drawing characters (│, ┌, ├, └, etc.)
/// - Bullet points and task indicators (◼, ◻, ✔, ●, ○, ■, □)
/// - Cursor prompts (❯, >, $, %)
/// - Claude Code task list lines (◼/◻/✔ + text)
/// - Claude Code cogitation/status indicators (✻, ⏵)
/// - Claude Code UI hints (lines containing "ctrl+" key bindings)
fn is_ui_chrome(line: &str) -> bool {
    // Lines that are entirely horizontal rules
    if line
        .chars()
        .all(|c| matches!(c, '─' | '━' | '=' | '-' | ' '))
    {
        return true;
    }

    // Claude Code task list lines: bullet character followed by text.
    // These appear after usage limit screens and are UI, not recovery content.
    let first_non_ws = line.trim_start();
    if first_non_ws
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '◼' | '◻' | '✔' | '✻' | '⏵'))
    {
        return true;
    }

    // Lines containing Claude Code UI key hints
    if first_non_ws.contains("ctrl+") && first_non_ws.contains(" to ") {
        return true;
    }

    // Lines that are mostly box-drawing or bullet characters
    let ui_chars: usize = line
        .chars()
        .filter(|c| {
            matches!(
                c,
                '│' | '┌'
                    | '├'
                    | '└'
                    | '┐'
                    | '┤'
                    | '┘'
                    | '┬'
                    | '┴'
                    | '┼'
                    | '╭'
                    | '╮'
                    | '╯'
                    | '╰'
                    | '◼'
                    | '◻'
                    | '✔'
                    | '●'
                    | '○'
                    | '■'
                    | '□'
                    | '▪'
                    | '▫'
                    | '❯'
                    | '>'
                    | '$'
                    | '%'
                    | '─'
                    | '━'
                    | ' '
            )
        })
        .count();

    // If more than 80% of non-whitespace chars are UI chrome, consider it chrome
    let non_ws_count = line.chars().filter(|c| !c.is_whitespace()).count();
    non_ws_count > 0 && ui_chars * 100 / non_ws_count >= 80
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
// Stuck coworker detection
// ---------------------------------------------------------------------------

/// A coworker detected as stuck (pane unchanged for the stuck duration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StuckCoworkerRestart {
    pub name: String,
    pub task_id: String,
    pub task_subject: String,
}

/// Detect coworkers whose headless process has not emitted events for
/// `stuck_duration`, indicating a stuck/hung process.
///
/// A coworker is only considered stuck if:
/// 1. Process is alive (`is_alive` = true)
/// 2. No stream events received for `stuck_duration`
/// 3. Has an in-progress task (idle coworkers are handled elsewhere)
///
/// Skips coworkers with usage limits, API errors, or running subagents —
/// they are paused/busy but not stuck.
///
/// Pure function: takes ProcessHealth data and returns restart decisions.
pub(crate) fn decide_stuck_coworker_restarts(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    in_progress_tasks: &[(String, String, String)],
    usage_limited_coworkers: &HashSet<String>,
    api_error_coworkers: &HashSet<String>,
    attached_coworkers: &HashSet<String>,
    now_utc: DateTime<Utc>,
    stuck_duration: Duration,
) -> Vec<StuckCoworkerRestart> {
    let stuck_threshold = chrono::Duration::from_std(stuck_duration).unwrap_or_default();
    let mut restarts = Vec::new();

    for (name, health) in process_health {
        // Only check alive processes
        if !health.is_alive {
            continue;
        }
        // Skip coworkers at usage limit — they're paused but not stuck
        if usage_limited_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip coworkers with API errors — they're waiting but may recover
        if api_error_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip attached coworkers — they're in interactive tmux mode
        if attached_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip coworkers with running subagents — parent session goes quiet
        // while Task tool subagents work, which can take several minutes
        if health.has_running_subagent {
            continue;
        }
        // Skip coworkers with pending tool executions — session goes quiet
        // while waiting for tool results (e.g., long Bash commands, slow API calls)
        if health.has_pending_tool {
            continue;
        }
        // Check last_event_at — no events yet means just spawned, skip
        let last_event = match health.last_event_at {
            Some(t) => t,
            None => continue,
        };
        let elapsed = now_utc.signed_duration_since(last_event);
        if elapsed < stuck_threshold {
            continue;
        }

        // Find the coworker's in-progress task
        let task = in_progress_tasks
            .iter()
            .find(|(_id, _subject, owner)| owner.eq_ignore_ascii_case(name));

        let Some((task_id, task_subject, _owner)) = task else {
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
/// by PR number in `GitHubState`, not by task. Adds a `max_restarts` limit
/// to prevent infinite restart loops for the same PR.
///
/// A reviewer is only considered stuck if:
/// 1. Process is alive (`is_alive` = true)
/// 2. No stream events received for `stuck_duration`
/// 3. Has an active PR assignment (in `reviewer_pr_assignments`)
/// 4. `restart_count < max_restarts` (backoff limit not reached)
///
/// Skips reviewers with usage limits, API errors, running subagents, or
/// pending tools — they are paused/busy but not stuck.
///
/// Pure function: takes ProcessHealth data and returns restart decisions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_stuck_reviewer_restarts(
    process_health: &HashMap<String, crate::daemon::snapshot::ProcessHealth>,
    reviewer_pr_assignments: &HashMap<String, u64>,
    reviewer_restart_counts: &HashMap<u64, u32>,
    usage_limited_coworkers: &HashSet<String>,
    api_error_coworkers: &HashSet<String>,
    attached_coworkers: &HashSet<String>,
    now_utc: DateTime<Utc>,
    stuck_duration: Duration,
    max_restarts: u32,
) -> Vec<StuckReviewerRestart> {
    let stuck_threshold = chrono::Duration::from_std(stuck_duration).unwrap_or_default();
    let mut restarts = Vec::new();

    for (name, health) in process_health {
        // Only check alive processes
        if !health.is_alive {
            continue;
        }
        // Only check coworkers that have a reviewer assignment
        let pr_number = match reviewer_pr_assignments.get(name) {
            Some(pr) => *pr,
            None => continue,
        };
        // Skip coworkers at usage limit
        if usage_limited_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip coworkers with API errors
        if api_error_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip attached coworkers
        if attached_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip coworkers with running subagents
        if health.has_running_subagent {
            continue;
        }
        // Skip coworkers with pending tool executions
        if health.has_pending_tool {
            continue;
        }
        // Check last_event_at — no events yet means just spawned, skip
        let last_event = match health.last_event_at {
            Some(t) => t,
            None => continue,
        };
        let elapsed = now_utc.signed_duration_since(last_event);
        if elapsed < stuck_threshold {
            continue;
        }

        // Check restart count — stop if we've exceeded the limit
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
/// Check if pane content indicates an active (not recovered) usage limit.
///
/// Returns true only if the usage limit pattern is present AND the coworker
/// hasn't recovered (no significant activity after the limit message).
///
/// Used indirectly via `decide_usage_limit_detection` and in snapshot collection.
/// Public (not `pub(crate)`) because integration tests in `dispatch_e2e.rs` call
/// this to verify usage limit detection against captured snapshot pane contents.
pub fn has_usage_limit_pattern(pane_content: &str) -> bool {
    is_at_usage_limit(pane_content)
}

/// Check if pane content indicates an API error (transient failure).
///
/// Returns true if an API error pattern is present AND the coworker hasn't
/// recovered (no significant activity after the error message).
///
/// API errors differ from usage limits:
/// - Usage limits have a known reset time; API errors are transient
/// - Usage limit nudges happen once at reset; API error nudges are periodic
/// - Both should skip stuck detection and idle shutdown
#[allow(dead_code)] // Used in tests; will be needed for Lead pane monitoring
pub(crate) fn has_api_error_pattern(pane_content: &str) -> bool {
    is_at_api_error(pane_content)
}

/// Check if pane content indicates an active (not recovered) API error.
///
/// Uses the same recovery detection as usage limits: if there's significant
/// activity after the error message, the coworker has recovered.
#[allow(dead_code)]
fn is_at_api_error(content: &str) -> bool {
    // Find the last occurrence of any API error pattern (case-insensitive)
    let content_lower = content.to_lowercase();
    let Some((error_pos, pattern_len)) = API_ERROR_PATTERNS
        .iter()
        .filter_map(|pattern| {
            content_lower
                .find(&pattern.to_lowercase())
                .map(|pos| (pos, pattern.len()))
        })
        .max_by_key(|(pos, _)| *pos)
    else {
        return false;
    };

    // Get content after the API error message
    let after_error = &content[error_pos + pattern_len..];

    // Count significant lines after the error (same logic as usage limit recovery)
    let significant_lines: usize = after_error
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !is_ui_chrome(trimmed)
        })
        .count();

    // If there are more than 5 significant lines after the error, coworker has recovered
    significant_lines <= 5
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

/// Decide what action to take for a PR issue detected by polling.
///
/// Pure function: takes the issue context and returns a `PrAction`.
/// The caller handles side effects (nudge/spawn/post).
pub fn decide_pr_issue_action(
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

/// Decide what action to take for a PR issue, with support for handoff.
///
/// This is an enhanced version of `decide_pr_issue_action` that considers
/// handing off the PR to a different coworker when:
/// - The original author is not active, or is active but busy on another task
/// - A stored session context is available
/// - There are idle coworkers available to take over
///
/// Only nudges the owner if they are both active and idle. The handoff
/// preserves the original author's session context so the new coworker
/// has full history of decisions and code understanding.
pub fn decide_pr_issue_action_with_handoff(
    owner: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    session_context: Option<&PrSessionContext>,
    message: &str,
) -> PrAction {
    let is_active = active_coworkers
        .iter()
        .any(|c| c.eq_ignore_ascii_case(owner));
    let is_idle = idle_coworkers.iter().any(|c| c.eq_ignore_ascii_case(owner));

    if is_active && is_idle {
        // Owner is active and idle — nudge them directly
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if (is_active && !is_idle) || !owner.is_empty() {
        // Owner is either active-but-busy or inactive. Try handoff first;
        // fallback depends on whether the owner is active:
        // - Active but busy → nudge (spawning an active coworker fails)
        // - Inactive → spawn (they need a new tmux window)
        if !is_active && at_dev_limit {
            PrAction::Skip {
                reason: format!("dev limit reached, cannot spawn {} for PR issue", owner),
            }
        } else if let Some(ctx) = session_context {
            // We have session context — try to hand off to an idle coworker
            // (excluding the original author who isn't available)
            let assignee = idle_coworkers
                .iter()
                .find(|c| !c.eq_ignore_ascii_case(owner))
                .cloned();

            if let Some(assignee) = assignee {
                PrAction::HandoffToCoworker {
                    assignee,
                    original_author: ctx.original_author.clone(),
                    pr_number: ctx.pr_number,
                    branch: ctx.branch.clone(),
                    session_id: ctx.session_id.clone(),
                    message: message.to_string(),
                }
            } else if is_active {
                // No idle coworkers — nudge the busy owner (already has a window)
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
        } else if is_active {
            // No session context, owner is active — nudge them
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
///
/// Note: Production code now uses `decide_pr_comment_action_with_handoff`.
/// This simpler variant is retained for tests.
#[cfg(test)]
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

/// Decide what action to take for a PR comment nudge, with handoff support.
///
/// Enhanced version of `decide_pr_comment_action` that considers handing off
/// the PR to a different coworker when the original author is unavailable
/// (inactive or active but busy on another task) and session context is
/// available. Only nudges the owner if they are both active and idle.
pub fn decide_pr_comment_action_with_handoff(
    owner: &str,
    actor: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    session_context: Option<&PrSessionContext>,
    message: &str,
) -> PrAction {
    // Don't nudge about own comments
    if owner == actor {
        return PrAction::Skip {
            reason: format!("PR comment is from owner {}, skipping self-nudge", owner),
        };
    }

    let is_active = active_coworkers
        .iter()
        .any(|c| c.eq_ignore_ascii_case(owner));
    let is_idle = idle_coworkers.iter().any(|c| c.eq_ignore_ascii_case(owner));

    if is_active && is_idle {
        // Owner is active and idle — nudge them directly
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if (is_active && !is_idle) || !owner.is_empty() {
        // Owner is either active-but-busy or inactive. Try handoff first;
        // fallback depends on whether the owner is active:
        // - Active but busy → nudge (spawning an active coworker fails)
        // - Inactive → spawn (they need a new tmux window)
        if !is_active && at_dev_limit {
            PrAction::Skip {
                reason: format!("dev limit reached, cannot spawn {} for PR comment", owner),
            }
        } else if let Some(ctx) = session_context {
            let assignee = idle_coworkers
                .iter()
                .find(|c| !c.eq_ignore_ascii_case(owner))
                .cloned();

            if let Some(assignee) = assignee {
                PrAction::HandoffToCoworker {
                    assignee,
                    original_author: ctx.original_author.clone(),
                    pr_number: ctx.pr_number,
                    branch: ctx.branch.clone(),
                    session_id: ctx.session_id.clone(),
                    message: message.to_string(),
                }
            } else if is_active {
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
        } else if is_active {
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
/// Nudge if active (idle or busy), spawn if inactive,
/// skip if inactive and at dev limit.
/// Accessible as `pub` for integration tests that verify snapshot-driven PR decisions.
pub fn decide_review_complete_action(
    owner: &str,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    at_dev_limit: bool,
    message: &str,
) -> PrAction {
    let is_active = active_coworkers
        .iter()
        .any(|c| c.eq_ignore_ascii_case(owner));
    let is_idle = idle_coworkers.iter().any(|c| c.eq_ignore_ascii_case(owner));

    if is_active && is_idle {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if !is_active && at_dev_limit {
        PrAction::Skip {
            reason: format!(
                "dev limit reached, cannot spawn {} for review complete",
                owner
            ),
        }
    } else if is_active {
        // Owner is active but busy — nudge (spawning an active coworker fails)
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
pub(crate) fn decide_orphan_recovery(
    in_progress: &[(String, String, String)], // (task_id, task_subject, owner)
    active_names: &HashSet<String>,
    at_dev_limit: bool,
    coworkers_with_open_prs: &HashSet<String>,
    review_feedback_pr_coworkers: &HashSet<String>,
    recently_stopped: &HashSet<String>,
    attached_coworkers: &HashSet<String>,
) -> Option<OrphanRecovery> {
    if at_dev_limit {
        return None;
    }

    for (task_id, task_subject, owner) in in_progress {
        let owner_clean = owner.trim().trim_matches('"').to_string();
        if owner_clean.is_empty() || owner_clean.eq_ignore_ascii_case("lead") {
            continue;
        }
        // Skip invalid coworker names — can't spawn a coworker with this name
        // Use lowercase to match how avenue names are stored
        let owner_lower = owner_clean.to_lowercase();
        if !crate::coworker::is_coworker_name(&owner_lower) {
            continue;
        }
        if active_names.contains(&owner_lower) {
            continue;
        }
        // Skip attached coworkers — they're in interactive tmux mode, not orphaned
        if attached_coworkers.contains(&owner_lower) {
            continue;
        }
        // Skip coworkers that recently stopped (within grace period).
        // When a coworker completes work and goes idle → shutdown, the task may
        // not yet be marked done. Without this grace period, orphan recovery
        // would immediately respawn the coworker for a task they already finished.
        if recently_stopped.contains(&owner_lower) {
            continue;
        }
        // Skip coworkers with open PRs who are awaiting review (no review feedback yet).
        // Even if the grace period has expired, recovering creates a loop:
        // spawn → coworker sees PR exists → goes idle → shutdown → recovery fires again.
        //
        // However, if the coworker has review feedback, they need to be recovered to
        // address it (handled by the PR pathway, but orphan recovery shouldn't block).
        if coworkers_with_open_prs.contains(&owner_lower)
            && !review_feedback_pr_coworkers.contains(&owner_lower)
        {
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

    // Legacy alias — all coworkers are now isolated
    fn cw_isolated(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        cw(name, minutes_old)
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Create a record map with a single coworker in the given health state.
    fn lifecycle_with(name: &str, health: SessionHealth) -> HashMap<String, CoworkerRecord> {
        let mut map = HashMap::new();
        map.insert(
            name.to_string(),
            CoworkerRecord {
                health: Some(health),
                last_activity: None,
                workflow_phase: None,
                task_id: None,
                workflow_updated_at: None,
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
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(get_health(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_busy_coworker() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert!(decisions.is_empty());
        // Busy coworker removed from idle tracking
        assert!(get_health(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_active_reviewer() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_unblocked_deps() {
        let coworkers = vec![cw("york", 10)];
        let mut phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert!(decisions.is_empty());
        // Coworker with unblocked deps removed from idle tracking
        assert!(get_health(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_skips_young_coworker() {
        let coworkers = vec![cw("york", 2)]; // Only 2 minutes old
        let mut phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert!(decisions.is_empty());
        // Young coworker also removed from idle tracking
        assert!(get_health(&phases, "york").is_none());
    }

    #[test]
    fn idle_shutdown_isolated_coworker_immediate() {
        let coworkers = vec![cw_isolated("reviewer", 10)];
        let phases: HashMap<String, CoworkerRecord> = HashMap::new();

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
    }

    #[test]
    fn idle_shutdown_immediate_for_unprotected_coworker() {
        // Unprotected coworkers are sent on break immediately (no delay)
        let coworkers = vec![cw("york", 10)];
        let phases: HashMap<String, CoworkerRecord> = HashMap::new();

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        // Immediate shutdown — no tracking delay
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr_no_ci() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR but CI has NOT passed — should NOT shutdown (protected by open PR)
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_sends_idle_coworker_on_break_despite_pane_activity() {
        // Bug #62: Idle coworkers with no task should be sent on break even if
        // their pane content changed recently. Pane changes for idle coworkers
        // come from daemon nudges, Claude Code UI updates, etc. — not real work.
        let coworkers = vec![cw("york", 10)];
        let mut phases = HashMap::new();
        phases.insert(
            "york".to_string(),
            CoworkerRecord {
                health: Some(SessionHealth::Idle {
                    since: Instant::now() - Duration::from_secs(60),
                }),
                last_activity: None,
                workflow_phase: None,
                task_id: None,
                workflow_updated_at: None,
            },
        );

        // york is idle, no tasks, no PRs, no reviews — should go on break
        // even though pane content changed recently
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &set(&[]), // pending_task_owners
            &set(&[]), // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

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
        // The daemon will respawn them when review feedback arrives.
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR AND CI is passing — should be ALLOWED to break
        // (waiting for review feedback)
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),       // not busy
            &set(&["york"]), // has open PR
            &set(&[]),       // not reviewing
            &set(&[]),       // no unblocked deps
            &set(&["york"]), // CI PASSED
            &set(&[]),       // usage_limited
            &set(&[]),       // api_error
            &set(&[]),       // pending_task_owners
            &set(&[]),       // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        // The new behavior: coworkers with CI-passed PRs CAN go on break
        assert_eq!(
            decisions.len(),
            1,
            "coworkers with CI-passed PRs should be sent on break (waiting for review)"
        );
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_usage_limited_coworker() {
        // Coworkers at usage limit should be protected from idle shutdown.
        // They're frozen waiting for the limit to reset, not truly idle.
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york is at usage limit — should NOT be sent on break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),       // not busy
            &set(&[]),       // no open PR
            &set(&[]),       // not reviewing
            &set(&[]),       // no unblocked deps
            &set(&[]),       // no ci_passed
            &set(&["york"]), // usage_limited
            &set(&[]),       // api_error
            &set(&[]),       // pending_task_owners
            &set(&[]),       // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(
            decisions.is_empty(),
            "usage-limited coworker should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_api_error_coworker() {
        // Coworkers with API errors should be protected from idle shutdown.
        // They're waiting for the API to recover, not truly idle.
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has API error — should NOT be sent on break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),       // not busy
            &set(&[]),       // no open PR
            &set(&[]),       // not reviewing
            &set(&[]),       // no unblocked deps
            &set(&[]),       // no ci_passed
            &set(&[]),       // not usage_limited
            &set(&["york"]), // HAS API ERROR
            &set(&[]),       // pending_task_owners
            &set(&[]),       // review_feedback_pr_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(
            decisions.is_empty(),
            "API error coworker should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_pending_assigned_task() {
        // Bug #753 (Bug 2): Lexington was sent on break despite having a pending
        // task (#753) assigned to them. The daemon should protect coworkers who
        // have pending tasks assigned, not just in-progress ones.
        let coworkers = vec![cw("lexington", 10)];
        let phases = lifecycle_with(
            "lexington",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // lexington has a pending task assigned — should NOT be sent on break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),            // not busy (task is pending, not in-progress)
            &set(&[]),            // no open PR
            &set(&[]),            // not reviewing
            &set(&[]),            // no unblocked deps
            &set(&[]),            // no ci_passed
            &set(&[]),            // not usage_limited
            &set(&[]),            // no api_error
            &set(&["lexington"]), // HAS PENDING TASK ASSIGNED
            &set(&[]),            // no review_feedback
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(
            decisions.is_empty(),
            "coworker with pending task assigned should be protected from idle shutdown"
        );
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_review_feedback_pr() {
        // Bug #753 (Bug 1): Madison's PR had CI passed + review feedback but she
        // was still sent on break, causing a spawn→idle→break loop. Coworkers whose
        // PRs have review feedback needing action should be protected.
        let coworkers = vec![cw("madison", 10)];
        let phases = lifecycle_with(
            "madison",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // madison has an open PR with CI passed AND review feedback — should NOT break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),          // not busy
            &set(&["madison"]), // has open PR
            &set(&[]),          // not reviewing
            &set(&[]),          // no unblocked deps
            &set(&["madison"]), // CI PASSED
            &set(&[]),          // not usage_limited
            &set(&[]),          // no api_error
            &set(&[]),          // no pending_task_owners
            &set(&["madison"]), // HAS REVIEW FEEDBACK
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(
            decisions.is_empty(),
            "coworker with CI-passed PR and review feedback should be protected from \
             idle shutdown (prevents spawn→idle→break loop)"
        );
    }

    #[test]
    fn idle_shutdown_still_allows_break_for_ci_passed_pr_without_feedback() {
        // Regression guard: coworkers with CI-passed PRs but NO review feedback
        // should still be allowed to go on break (original behavior).
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),       // not busy
            &set(&["york"]), // has open PR
            &set(&[]),       // not reviewing
            &set(&[]),       // no unblocked deps
            &set(&["york"]), // CI PASSED
            &set(&[]),       // not usage_limited
            &set(&[]),       // no api_error
            &set(&[]),       // no pending_task_owners
            &set(&[]),       // NO review feedback
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert_eq!(
            decisions.len(),
            1,
            "coworker with CI-passed PR but no review feedback should still go on break"
        );
    }

    // -----------------------------------------------------------------------
    // decide_pr_issue_action tests
    // -----------------------------------------------------------------------

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
    // decide_pr_comment_action_with_handoff tests
    // -----------------------------------------------------------------------

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
        // because they already have a tmux window.
        let action = decide_review_complete_action(
            "york",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]), // york is NOT idle
            false,
            "review complete",
        );
        assert!(matches!(action, PrAction::NudgeOwner { .. }));
    }

    // -----------------------------------------------------------------------
    // decide_pr_issue_action_with_handoff tests
    // -----------------------------------------------------------------------

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
        // they already have a tmux window.
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
    // decide_orphan_recovery tests
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_recovery_finds_orphan() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
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
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_at_dev_limit() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, true, &empty, &empty, &empty, &empty);
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_skips_lead_owner() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "lead".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
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
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
        assert_eq!(result.unwrap().task_id, "1");
    }

    #[test]
    fn orphan_recovery_skips_invalid_coworker_name() {
        // Bug: task with invalid owner "fix" (not an avenue name) should be skipped,
        // not returned for recovery, since we can't spawn a coworker named "fix"
        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "fix".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
        // Should be None because "fix" is not a valid coworker name
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_handles_uppercase_owner() {
        // Uppercase owner names should still be recognized as valid coworkers
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "YORK".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let result = decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &empty);
        // Should return recovery because "YORK" maps to valid coworker "york"
        assert!(result.is_some());
        assert_eq!(result.unwrap().owner, "YORK");
    }

    #[test]
    fn orphan_recovery_skips_coworker_awaiting_review() {
        // Bug: coworker opened a PR with green CI and is awaiting review.
        // The idle shutdown correctly lets them go on break, but orphan
        // recovery kept respawning them because it didn't check PR state.
        let tasks = vec![(
            "789".to_string(),
            "Add usage bars".to_string(),
            "amsterdam".to_string(),
        )];
        let active = set(&[]); // amsterdam is not active (on break)
        let coworkers_with_open_prs = set(&["amsterdam"]);
        let review_feedback = set(&[]); // no review feedback yet
        let recently_stopped = set(&["amsterdam"]); // cleanly stopped (on break)

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — coworker is correctly waiting for review
        assert!(
            result.is_none(),
            "Should not recover coworker awaiting review on green PR"
        );
    }

    #[test]
    fn orphan_recovery_recovers_coworker_with_review_feedback() {
        // When review feedback arrives, the coworker should be recovered
        // so they can address the comments.
        let tasks = vec![(
            "789".to_string(),
            "Add usage bars".to_string(),
            "amsterdam".to_string(),
        )];
        let active = set(&[]); // amsterdam is not active
        let coworkers_with_open_prs = set(&["amsterdam"]);
        let review_feedback = set(&["amsterdam"]); // review feedback posted

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &HashSet::new(),
            &HashSet::new(),
        );
        // SHOULD recover — there's actionable review feedback
        assert!(result.is_some());
        assert_eq!(result.unwrap().task_id, "789");
    }

    #[test]
    fn orphan_recovery_skips_coworker_with_failed_ci_and_open_pr() {
        // When CI fails on the PR, the coworker should NOT be recovered
        // by orphan recovery — CI failures are handled separately by
        // handle_webhook_ci_failure() and the PR poll pathway. Recovering
        // via orphan recovery created a loop because the coworker would
        // be spawned, go idle (not knowing about CI failure), and shut down.
        let tasks = vec![(
            "789".to_string(),
            "Add usage bars".to_string(),
            "amsterdam".to_string(),
        )];
        let active = set(&[]); // amsterdam is not active
        let coworkers_with_open_prs = set(&["amsterdam"]);
        let review_feedback = set(&[]);
        let recently_stopped = set(&["amsterdam"]); // cleanly stopped after opening PR

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — coworker has an open PR. CI failures
        // are handled by the webhook/PR poll pathway, not orphan recovery.
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR (CI failures handled separately)"
        );
    }

    #[test]
    fn orphan_recovery_recovers_coworker_without_pr() {
        // Coworker without an open PR should still be recovered normally.
        let tasks = vec![(
            "789".to_string(),
            "Add usage bars".to_string(),
            "amsterdam".to_string(),
        )];
        let active = set(&[]); // amsterdam is not active
        let coworkers_with_open_prs = set(&[]); // no open PR
        let review_feedback = set(&[]);

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &HashSet::new(),
            &HashSet::new(),
        );
        // SHOULD recover — no PR means work isn't done yet
        assert!(result.is_some());
        assert_eq!(result.unwrap().task_id, "789");
    }

    #[test]
    fn orphan_recovery_skips_coworker_with_open_pr_before_ci_cached() {
        // Bug: coworker opens a PR, goes idle, shuts down. Orphan recovery fires
        // before the PR poll has cached CI status. coworkers_with_open_prs contains
        // the coworker (fallback to gh CLI), but ci_passed_pr_coworkers is empty
        // (only populated by PR poll). The skip check fails because it requires
        // BOTH has_open_pr AND ci_passed, creating a recovery loop.
        //
        // This is the root cause of the lexington recovery loop (task !810):
        // - lexington opened PR #682, went idle, shut down
        // - orphan check fires every 10s, PR poll every 30s
        // - In the window before PR poll caches CI status, recovery fires
        // - coworker spawns, goes idle, shuts down, recovery fires again
        let tasks = vec![(
            "810".to_string(),
            "Fix auth endpoint".to_string(),
            "lexington".to_string(),
        )];
        let active = set(&[]); // lexington is not active (shut down)
        let coworkers_with_open_prs = set(&["lexington"]); // PR detected via fallback
        let review_feedback = set(&[]);
        let recently_stopped = set(&["lexington"]); // cleanly stopped after opening PR

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — coworker has an open PR and no review feedback.
        // CI status is unknown (not cached yet), but the safe default should be
        // to skip recovery. CI failures are handled by the webhook/PR poll pathway.
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even when CI status is not yet cached"
        );
    }

    #[test]
    fn orphan_recovery_skips_multi_task_coworker_with_open_pr_before_ci() {
        // Bug: coworker has TWO in_progress tasks and an open PR, but the PR
        // poll hasn't cached CI status yet. ci_passed_pr_coworkers is empty.
        // The skip check fails for both tasks, and the first one triggers
        // recovery — creating a loop where the coworker is spawned for a task
        // whose work is already done (PR opened).
        let tasks = vec![
            (
                "810".to_string(),
                "Fix auth endpoint".to_string(),
                "lexington".to_string(),
            ),
            (
                "811".to_string(),
                "Address review feedback".to_string(),
                "lexington".to_string(),
            ),
        ];
        let active = set(&[]);
        let coworkers_with_open_prs = set(&["lexington"]);
        let review_feedback = set(&[]);
        let recently_stopped = set(&["lexington"]); // cleanly stopped after opening PR

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — coworker has an open PR. Even though CI status
        // is unknown, the safe default is to wait for the PR poll to determine
        // if recovery is actually needed. CI failures are handled separately.
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even when CI status is not yet cached"
        );
    }

    #[test]
    fn orphan_recovery_skips_recently_stopped_coworker() {
        // Bug: coworker completes work, goes idle, gets shut down. The task
        // is still in_progress because it hasn't been marked done yet. Orphan
        // recovery fires and respawns the coworker for a task it already finished.
        //
        // Fix: skip recovery for coworkers that recently stopped (within a grace
        // period), giving the system time to mark the task complete.
        let tasks = vec![(
            "832".to_string(),
            "Review feedback".to_string(),
            "york".to_string(),
        )];
        let active = set(&[]); // york is not active (just shut down)
        let empty = HashSet::new();
        let recently_stopped = set(&["york"]); // york stopped within grace period

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &empty,
            &empty,
            &recently_stopped,
            &empty,
        );
        assert!(
            result.is_none(),
            "Should not recover coworker that recently stopped (within grace period)"
        );
    }

    #[test]
    fn orphan_recovery_recovers_after_grace_period() {
        // After the grace period expires, the coworker should be recovered
        // if their task is still in_progress and they're not active.
        let tasks = vec![(
            "832".to_string(),
            "Review feedback".to_string(),
            "york".to_string(),
        )];
        let active = set(&[]); // york is not active
        let empty = HashSet::new();
        let recently_stopped = set(&[]); // york NOT in recently_stopped (grace period expired)

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &empty,
            &empty,
            &recently_stopped,
            &empty,
        );
        assert!(
            result.is_some(),
            "Should recover coworker after grace period expires"
        );
        assert_eq!(result.unwrap().task_id, "832");
    }

    /// Regression test for #874: RPC idle handler false orphan recovery.
    ///
    /// Bug: when a coworker reports idle via RPC, the handler shuts them down
    /// directly (bypassing the Effect system) and does NOT record the stop time
    /// in coworker_stop_times. On the next TaskDispatchTick (~10s later),
    /// check_and_recover_orphans computes recently_stopped from coworker_stop_times.
    /// Since the stop time was never recorded, recently_stopped is empty, and
    /// the coworker's in_progress task appears orphaned → false recovery.
    ///
    /// Fix: record stop time in coworker_stop_times in the RPC idle handler
    /// (and handle_coworker_break), matching what Effect::ShutdownCoworker does.
    ///
    /// This test verifies the decision function: when a coworker reports idle
    /// and the stop time IS recorded (recently_stopped contains them), orphan
    /// recovery should NOT trigger.
    #[test]
    fn orphan_recovery_skips_coworker_that_just_reported_idle() {
        // Scenario: madison reports idle via RPC. She still has in_progress
        // task !861 (task completion is async). The RPC handler shuts her down
        // and records her stop time. On the next TaskDispatchTick, orphan
        // recovery must skip her because she's in recently_stopped.
        let tasks = vec![(
            "861".to_string(),
            "Review PR #705".to_string(),
            "madison".to_string(),
        )];
        let active = set(&[]); // madison shut down (not in active_names)
        let empty = HashSet::new();
        let recently_stopped = set(&["madison"]); // stop time was recorded by RPC handler

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &empty,
            &empty,
            &recently_stopped,
            &empty,
        );
        assert!(
            result.is_none(),
            "Should NOT recover coworker that just reported idle (recently stopped)"
        );
    }

    /// Regression test for #874: verify false recovery WOULD occur without stop time.
    ///
    /// This is the buggy scenario: the RPC idle handler does NOT record the stop
    /// time, so recently_stopped is empty. Orphan recovery falsely triggers.
    #[test]
    fn orphan_recovery_false_positive_without_stop_time() {
        // Same scenario as above, but recently_stopped is empty (the bug)
        let tasks = vec![(
            "861".to_string(),
            "Review PR #705".to_string(),
            "madison".to_string(),
        )];
        let active = set(&[]); // madison shut down
        let empty = HashSet::new();
        // BUG: recently_stopped is empty because RPC handler didn't record stop time
        let recently_stopped = set(&[]);

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &empty,
            &empty,
            &recently_stopped,
            &empty,
        );
        assert!(
            result.is_some(),
            "Without stop time recording, orphan recovery falsely triggers (the bug)"
        );
        assert_eq!(result.unwrap().owner, "madison");
    }

    /// Regression test for #874: auth switch shuts down multiple coworkers.
    ///
    /// When handle_auth_switch shuts down all running coworkers, it must record
    /// stop times for each one. Otherwise, any coworker with an in_progress task
    /// gets falsely recovered on the next TaskDispatchTick.
    #[test]
    fn orphan_recovery_skips_coworkers_shut_down_by_auth_switch() {
        // Scenario: auth switch shuts down madison and park. Both have in_progress
        // tasks. The RPC handler records stop times for both.
        let tasks = vec![
            (
                "861".to_string(),
                "Review PR #705".to_string(),
                "madison".to_string(),
            ),
            (
                "862".to_string(),
                "Fix auth bug".to_string(),
                "park".to_string(),
            ),
        ];
        let active = set(&[]); // all coworkers shut down
        let empty = HashSet::new();
        let recently_stopped = set(&["madison", "park"]); // stop times recorded

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &empty,
            &empty,
            &recently_stopped,
            &empty,
        );
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

    /// Snapshot from bug #756: madison was in a break/respawn loop for 4+ hours
    /// with 4 duplicate tmux windows accumulating. The snapshot captures the state
    /// where madison has an open PR (#649) with CI passed AND review feedback,
    /// but was being repeatedly shut down.
    ///
    /// The review_feedback_pr_coworkers field didn't exist in the snapshot (captured
    /// before PR #650), so we reconstruct it from the channel messages which show
    /// review feedback was present.
    #[test]
    fn snapshot_20260205_madison_break_loop_protected_by_review_feedback() {
        let fixture = include_str!(
            "../tests/fixtures/snapshot/snapshot-madison-break-loop-pr-not-merging-20260205-130328.json"
        );
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();

        // Extract coworker list from snapshot
        let active = &snapshot["active_coworkers"];
        assert!(
            active
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "madison"),
            "madison should be in active_coworkers"
        );

        let coworkers = vec![cw("madison", 10)];

        let mut phases = lifecycle_with(
            "madison",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // From snapshot: madison has open PR with CI passed
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

        // Channel messages confirm review feedback exists — reconstruct the field
        // that was added in PR #650 to fix the original break loop (#753)
        let review_feedback = set(&["madison"]);

        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]), // not busy
            &coworkers_with_open_prs,
            &set(&[]), // not reviewing
            &set(&[]), // no unblocked deps
            &ci_passed,
            &set(&[]),        // not usage limited
            &set(&[]),        // no api errors
            &set(&[]),        // no pending tasks
            &review_feedback, // HAS review feedback
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, _transitions);

        // With the review_feedback protection (PR #650), madison should NOT be
        // shut down — she has an open PR with CI passed but also review feedback
        // to address. Without this protection, the break/respawn loop occurs.
        assert!(
            decisions.is_empty(),
            "madison should NOT be sent on break when she has review feedback on her PR. \
             This caused the break/respawn loop in bug #756 (4 duplicate tmux windows). \
             Decisions: {:?}",
            decisions
        );
    }

    // -----------------------------------------------------------------------
    // Usage limit detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn usage_limit_code_content_should_not_trigger_detection() {
        // This is the false positive case: code with "usage limits" in a comment
        // should NOT trigger usage limit detection
        let code_content = r#"
            // Health checks: idle shutdown, stuck detection, usage limits.
            fn check_health() {
                // Handle rate limit errors gracefully
                if self.rate_limit_exceeded {
                    return Err("rate limit hit");
                }
            }
        "#;

        assert!(
            !has_usage_limit_pattern(code_content),
            "code containing 'usage limits' in comments should NOT trigger detection"
        );
    }

    #[test]
    fn usage_limit_actual_screen_should_trigger_detection() {
        // This is the true positive case: actual Claude Code usage limit screen
        // shows "/upgrade" as an action option
        let actual_usage_limit_screen = r#"
            You've reached your usage limit for Claude Opus 4.5.

            Your limit will reset in 2 hours 30 minutes.

            Options:
            - /upgrade to increase your limit
            - /compact to reduce context
            - Wait for the limit to reset
        "#;

        assert!(
            has_usage_limit_pattern(actual_usage_limit_screen),
            "actual usage limit screen with '/upgrade' should trigger detection"
        );
    }

    // -----------------------------------------------------------------------
    // decide_stuck_coworker_restarts tests (ProcessHealth-based)
    // -----------------------------------------------------------------------

    #[test]
    fn stuck_detection_triggers_for_no_events() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "riverside".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "riverside".to_string(),
        )];

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0].name, "riverside");
    }

    #[test]
    fn stuck_detection_skips_recent_events() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "riverside".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::seconds(30)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "riverside".to_string(),
        )];

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "recent events should not trigger stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_usage_limited() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "york".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: true,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

        let mut usage_limited = HashSet::new();
        usage_limited.insert("york".to_string());

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &usage_limited,
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "usage-limited coworker should be skipped"
        );
    }

    #[test]
    fn stuck_detection_skips_api_error() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "madison".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: true,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "madison".to_string(),
        )];

        let mut api_error = HashSet::new();
        api_error.insert("madison".to_string());

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &api_error,
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(restarts.is_empty(), "API error coworker should be skipped");
    }

    #[test]
    fn stuck_detection_skips_running_subagent() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "park".to_string(),
            ProcessHealth {
                is_alive: true,
                // Last parent event was 10 minutes ago — normally stuck
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                // But has a running subagent, so parent stream is expected to be quiet
                has_running_subagent: true,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "park".to_string())];

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "coworker with running subagent should not be flagged as stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_dead_processes() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "broadway".to_string(),
            ProcessHealth {
                is_alive: false,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: Some(1),
            },
        );

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "broadway".to_string(),
        )];

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "dead processes are handled by check_and_respawn_dead_processes"
        );
    }

    #[test]
    fn stuck_detection_skips_attached_coworkers() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "park".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "park".to_string())];

        let mut attached = HashSet::new();
        attached.insert("park".to_string());

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &attached,
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "attached coworker should not be flagged as stuck"
        );
    }

    #[test]
    fn stuck_detection_skips_pending_tool_execution() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "broadway".to_string(),
            ProcessHealth {
                is_alive: true,
                // Last event was 10 minutes ago — normally stuck
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                // But has a pending tool (saw tool_use, waiting for tool_result)
                has_pending_tool: true,
                exit_code: None,
            },
        );

        let tasks = vec![(
            "42".to_string(),
            "Build project".to_string(),
            "broadway".to_string(),
        )];

        let restarts = decide_stuck_coworker_restarts(
            &health,
            &tasks,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(180),
        );

        assert!(
            restarts.is_empty(),
            "coworker with pending tool execution should not be flagged as stuck"
        );
    }

    #[test]
    fn orphan_recovery_skips_attached_coworkers() {
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "york".to_string())];
        let active = set(&["amsterdam"]);
        let empty = HashSet::new();
        let mut attached = HashSet::new();
        attached.insert("york".to_string());

        let result =
            decide_orphan_recovery(&tasks, &active, false, &empty, &empty, &empty, &attached);
        assert!(
            result.is_none(),
            "attached coworker should not be treated as orphan"
        );
    }

    #[test]
    fn orphan_recovery_skips_killed_coworker_with_open_pr() {
        // When a coworker is killed (e.g., by auth switch) while their PR is open
        // without review feedback, orphan recovery should NOT spawn them because:
        // 1. The PR is already open — the work is done
        // 2. If spawned, they'd see the PR exists and go idle again (loop)
        //
        // The daemon should instead auto-complete the task when it detects a PR
        // is open for an in_progress task. Orphan recovery is not the right pathway
        // for task completion — that's handled by PR management.
        let tasks = vec![(
            "952".to_string(),
            "Fix PR handling".to_string(),
            "broadway".to_string(),
        )];
        let active = set(&[]); // broadway is not active (killed)
        let coworkers_with_open_prs = set(&["broadway"]); // PR is open
        let review_feedback = set(&[]); // no review feedback yet
        let recently_stopped = set(&[]); // NOT in recently_stopped (killed, not cleanly stopped)

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — PR is open, work is done. Task should be auto-completed
        // by PR management pathway, not orphan recovery.
        assert!(
            result.is_none(),
            "Should not recover killed coworker if PR is open (work already done)"
        );
    }

    #[test]
    fn orphan_recovery_skips_recently_stopped_coworker_awaiting_review() {
        // When a coworker cleanly stops (within grace period) and has an open PR
        // without review feedback, they're correctly waiting for review — don't recover.
        let tasks = vec![(
            "952".to_string(),
            "Fix PR handling".to_string(),
            "broadway".to_string(),
        )];
        let active = set(&[]); // broadway is not active
        let coworkers_with_open_prs = set(&["broadway"]); // PR is open
        let review_feedback = set(&[]); // no review feedback yet
        let recently_stopped = set(&["broadway"]); // cleanly stopped within grace period

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — coworker is correctly waiting for review
        assert!(
            result.is_none(),
            "Should not recover coworker who recently stopped and is awaiting review"
        );
    }

    #[test]
    fn orphan_recovery_skips_coworker_after_grace_period_with_open_pr() {
        // Regression test for task !1011: When a coworker opens a PR and goes idle,
        // after the 40s grace period expires, they're no longer in recently_stopped.
        // Without this check, orphan recovery fires, spawns the coworker, who sees
        // the PR exists and goes idle again → infinite loop.
        //
        // Observed with amsterdam on task !1008:
        // 1. amsterdam opens PR #810, goes idle
        // 2. Grace period (40s) passes → no longer in recently_stopped
        // 3. Daemon recovers as "orphan" → amsterdam spawns
        // 4. amsterdam sees PR exists, goes idle, shuts down
        // 5. Repeat step 2 → loop
        let tasks = vec![(
            "1008".to_string(),
            "Add web UI channel switching".to_string(),
            "amsterdam".to_string(),
        )];
        let active = set(&[]); // amsterdam not active (shut down after grace period)
        let coworkers_with_open_prs = set(&["amsterdam"]); // PR #810 is open
        let review_feedback = set(&[]); // no review feedback yet
        let recently_stopped = set(&[]); // NOT in recently_stopped (grace period expired)

        let result = decide_orphan_recovery(
            &tasks,
            &active,
            false,
            &coworkers_with_open_prs,
            &review_feedback,
            &recently_stopped,
            &HashSet::new(),
        );
        // Should NOT recover — even though grace period expired, the coworker has an
        // open PR awaiting review. Recovering would create a loop because the coworker
        // would spawn, see the PR exists, and go idle again.
        assert!(
            result.is_none(),
            "Should not recover coworker with open PR even after grace period (creates loop)"
        );
    }

    // -----------------------------------------------------------------------
    // Usage limit recovery detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn usage_limit_recovery_detected_after_activity() {
        // Coworker hit usage limit but has since recovered and is working again
        let recovered_pane = r#"
You've reached your usage limit. /upgrade to increase.
Your limit will reset in 2 hours.

> User response resumed

⏺ I'll continue with the task.

Let me read the file first.

⏺ Read(file_path: "/src/main.rs")

Now I'll implement the fix.

⏺ Edit(file_path: "/src/main.rs")
"#;

        assert!(
            !has_usage_limit_pattern(recovered_pane),
            "coworker with significant activity after usage limit should NOT be detected as limited"
        );
    }

    #[test]
    fn usage_limit_still_stuck_at_limit() {
        // Coworker is still at the usage limit screen (no significant activity after)
        let stuck_at_limit = r#"
You've reached your usage limit for Claude Opus 4.5.

Your limit will reset in 2 hours.

Options:
- /upgrade to increase your limit
- /compact to reduce context
"#;

        assert!(
            has_usage_limit_pattern(stuck_at_limit),
            "coworker still at usage limit screen should be detected as limited"
        );
    }

    #[test]
    fn usage_limit_minimal_activity_still_limited() {
        // Just a few lines after the limit - not enough to consider recovered
        let minimal_after = r#"
- /upgrade to increase your limit

(waiting for limit to reset)
"#;

        assert!(
            has_usage_limit_pattern(minimal_after),
            "minimal activity after limit should still be considered limited"
        );
    }

    #[test]
    fn usage_limit_case_insensitive() {
        // Detection should be case-insensitive
        let uppercase = "Your limit reached. - /UPGRADE to increase your limit.";
        let mixed_case = "Your limit reached. - /Upgrade to increase your limit.";

        assert!(
            has_usage_limit_pattern(uppercase),
            "uppercase '/UPGRADE' should trigger detection"
        );
        assert!(
            has_usage_limit_pattern(mixed_case),
            "mixed case '/Upgrade' should trigger detection"
        );
    }

    #[test]
    fn usage_limit_code_with_upgrade_should_not_trigger() {
        // Code containing "/upgrade" in a string literal or comment should NOT trigger
        // because it lacks the contextual patterns "- /upgrade" or "/upgrade to"
        let code_with_upgrade = r#"
            // Test fixture for usage limit detection
            const PATTERN: &str = "/upgrade";

            fn test_usage_limit() {
                let pane = "some content with /upgrade in it";
                assert!(has_pattern(pane));
            }
        "#;

        assert!(
            !has_usage_limit_pattern(code_with_upgrade),
            "code containing '/upgrade' without context should NOT trigger detection"
        );
    }

    #[test]
    fn usage_limit_ui_chrome_should_not_count_as_activity() {
        // Pure UI chrome (horizontal rules, cursor prompts) after the limit should not
        // count as "significant activity" for recovery detection.
        // Note: Lines with actual text content (like "Task 1" or file paths) ARE counted
        // as significant since they represent real output, not just chrome.
        let limit_with_pure_chrome = r#"
You've reached your usage limit for Claude Opus 4.5.

- /upgrade to increase your limit

───────────────────────────
━━━━━━━━━━━━━━━━━━━━━━━━━━━
========================
❯
❯
"#;

        assert!(
            has_usage_limit_pattern(limit_with_pure_chrome),
            "pure UI chrome after usage limit should not count as recovery activity"
        );
    }

    #[test]
    fn usage_limit_real_activity_means_recovered() {
        // If there's actual meaningful content after the usage limit (tool calls,
        // text output, etc.), the coworker has recovered
        let recovered_with_real_output = r#"
You've reached your usage limit for Claude Opus 4.5.

- /upgrade to increase your limit

OK I'll continue working.
Let me read the file.
⏺ Read(file_path: "/src/main.rs")
Got it, here are the contents.
Now implementing the fix.
⏺ Edit(file_path: "/src/main.rs")
"#;

        assert!(
            !has_usage_limit_pattern(recovered_with_real_output),
            "real activity after usage limit should indicate recovery"
        );
    }

    #[test]
    fn ui_chrome_detects_task_list_items() {
        // Claude Code renders task list items with bullet chars + text
        assert!(is_ui_chrome("◼ Run 5 parallel code review agents"));
        assert!(is_ui_chrome("◻ Score and filter issues"));
        assert!(is_ui_chrome("✔ Check PR #702 eligibility"));
        assert!(is_ui_chrome("  ◼ Run 5 parallel code review agents")); // indented
    }

    #[test]
    fn ui_chrome_detects_cogitation_and_status() {
        assert!(is_ui_chrome("✻ Worked for 1m 49s"));
        assert!(is_ui_chrome(
            "✻ Running parallel code reviews… (2m 4s · ↓ 4.1k tokens)"
        ));
        assert!(is_ui_chrome(
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ctrl+t to hide tasks"
        ));
    }

    #[test]
    fn ui_chrome_detects_ctrl_key_hints() {
        assert!(is_ui_chrome(
            "6 tasks (3 done, 1 in progress, 2 open) · ctrl+t to hide tasks"
        ));
        assert!(is_ui_chrome("ctrl+b ctrl+b (twice) to run in background"));
    }

    #[test]
    fn ui_chrome_does_not_match_real_content() {
        assert!(!is_ui_chrome("Reading file src/main.rs"));
        assert!(!is_ui_chrome("OK I'll continue working."));
        assert!(!is_ui_chrome("Let me read the file."));
        assert!(!is_ui_chrome("Now implementing the fix."));
    }

    #[test]
    fn usage_limit_extra_usage_with_claude_code_ui() {
        // Real pane content from Claude Code v2.1.33+ hitting usage limit.
        // After the /extra-usage pattern, Claude Code renders its task list
        // and status bar — these should be recognized as UI chrome, not recovery.
        let pane = r#"
  ⎿  You've hit your limit · resets 11pm (America/Chicago)
     /extra-usage to finish what you're working on.

✻ Worked for 1m 49s

  6 tasks (3 done, 1 in progress, 2 open) · ctrl+t to hide tasks
  ◼ Run 5 parallel code review agents
  ◻ Score and filter issues
  ◻ Post review comment on PR
  ✔ Check PR #702 eligibility
  ✔ Find relevant CLAUDE.md files
  ✔ Get PR #702 summary

─────────────────────────────────────────────
❯
─────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ctrl+t to hide tasks
"#;

        assert!(
            has_usage_limit_pattern(pane),
            "usage limit with Claude Code UI chrome after /extra-usage should be detected"
        );
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
    // API error detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn api_error_detects_500_error() {
        let api_error_pane = r#"
I'll read the file now.
⏺ Read(file_path: "/src/main.rs")

API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"},"request_id":"req_123"}
"#;

        assert!(
            has_api_error_pattern(api_error_pane),
            "should detect API Error: 500 pattern"
        );
    }

    #[test]
    fn api_error_detects_overloaded_error() {
        let overloaded_pane = r#"
Working on the task.

API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_456"}
"#;

        assert!(
            has_api_error_pattern(overloaded_pane),
            "should detect overloaded_error pattern"
        );
    }

    #[test]
    fn api_error_detects_internal_server_error_message() {
        let internal_error_pane = "Something went wrong. Internal server error. Please try again.";

        assert!(
            has_api_error_pattern(internal_error_pane),
            "should detect 'Internal server error' message"
        );
    }

    #[test]
    fn api_error_detection_is_case_insensitive() {
        // Test various case variations to ensure detection works
        assert!(
            has_api_error_pattern("API ERROR: 500"),
            "should detect uppercase 'API ERROR'"
        );
        assert!(
            has_api_error_pattern("api error: 500"),
            "should detect lowercase 'api error'"
        );
        assert!(
            has_api_error_pattern("INTERNAL SERVER ERROR"),
            "should detect uppercase 'INTERNAL SERVER ERROR'"
        );
        assert!(
            has_api_error_pattern("internal server error"),
            "should detect lowercase 'internal server error'"
        );
    }

    #[test]
    fn api_error_code_content_should_not_trigger_detection() {
        // Code containing API error strings in comments should NOT trigger detection
        // if there's significant activity after
        let code_content = r#"
// Handle API errors gracefully
// API Error: 500 is a server error
fn handle_api_error(status: u16) {
    match status {
        500 => log!("Internal server error"),
        _ => log!("Unknown error"),
    }
}

// Now implement the actual handler
fn process_request() {
    let result = make_api_call();
    handle_response(result);
    validate_output();
    send_notification();
    cleanup_resources();
}
"#;

        assert!(
            !has_api_error_pattern(code_content),
            "code with API error strings followed by activity should NOT trigger detection"
        );
    }

    #[test]
    fn api_error_recovers_after_real_activity() {
        // If coworker continues working after API error, they've recovered
        let recovered_pane = r#"
API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"}}

Retrying the request...
⏺ Read(file_path: "/src/main.rs")
Got the file contents.
Now editing.
⏺ Edit(file_path: "/src/main.rs")
Done with the edit.
"#;

        assert!(
            !has_api_error_pattern(recovered_pane),
            "real activity after API error should indicate recovery"
        );
    }

    #[test]
    fn api_error_still_stuck_with_only_ui_chrome() {
        // If only UI chrome follows the error, coworker is still stuck
        let stuck_with_chrome = r#"
API Error: 502 {"type":"error","error":{"type":"api_error","message":"Bad gateway"}}

───────────────────────────
❯
"#;

        assert!(
            has_api_error_pattern(stuck_with_chrome),
            "UI chrome after API error should not count as recovery"
        );
    }

    // -----------------------------------------------------------------------
    // decide_stuck_reviewer_restarts tests
    // -----------------------------------------------------------------------

    #[test]
    fn stuck_reviewer_detected() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "riverside".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let mut reviewer_assignments = HashMap::new();
        reviewer_assignments.insert("riverside".to_string(), 42u64);

        let restarts = decide_stuck_reviewer_restarts(
            &health,
            &reviewer_assignments,
            &HashMap::new(), // no prior restarts
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(300),
            2,
        );

        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0].name, "riverside");
        assert_eq!(restarts[0].pr_number, 42);
        assert_eq!(restarts[0].restart_count, 0);
    }

    #[test]
    fn stuck_reviewer_skipped_usage_limited() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "york".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: true,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let mut reviewer_assignments = HashMap::new();
        reviewer_assignments.insert("york".to_string(), 42u64);

        let mut usage_limited = HashSet::new();
        usage_limited.insert("york".to_string());

        let restarts = decide_stuck_reviewer_restarts(
            &health,
            &reviewer_assignments,
            &HashMap::new(),
            &usage_limited,
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(300),
            2,
        );

        assert!(
            restarts.is_empty(),
            "usage-limited reviewer should be skipped"
        );
    }

    #[test]
    fn stuck_reviewer_skipped_subagent() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "park".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: true,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let mut reviewer_assignments = HashMap::new();
        reviewer_assignments.insert("park".to_string(), 42u64);

        let restarts = decide_stuck_reviewer_restarts(
            &health,
            &reviewer_assignments,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(300),
            2,
        );

        assert!(
            restarts.is_empty(),
            "reviewer with running subagent should be skipped"
        );
    }

    #[test]
    fn stuck_reviewer_max_restarts_stops_loop() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "broadway".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        let mut reviewer_assignments = HashMap::new();
        reviewer_assignments.insert("broadway".to_string(), 42u64);

        // PR 42 already has 2 restarts (= MAX_REVIEWER_RESTARTS)
        let mut restart_counts = HashMap::new();
        restart_counts.insert(42u64, 2u32);

        let restarts = decide_stuck_reviewer_restarts(
            &health,
            &reviewer_assignments,
            &restart_counts,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(300),
            2,
        );

        assert!(
            restarts.is_empty(),
            "reviewer at max restarts should not be flagged (loop broken)"
        );
    }

    #[test]
    fn stuck_reviewer_no_assignment_not_flagged() {
        use crate::daemon::snapshot::ProcessHealth;

        let now = Utc::now();
        let mut health = HashMap::new();
        health.insert(
            "madison".to_string(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now - chrono::Duration::minutes(10)),
                has_usage_limit: false,
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                exit_code: None,
            },
        );

        // madison has NO reviewer assignment
        let reviewer_assignments = HashMap::new();

        let restarts = decide_stuck_reviewer_restarts(
            &health,
            &reviewer_assignments,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            now,
            Duration::from_secs(300),
            2,
        );

        assert!(
            restarts.is_empty(),
            "coworker without PR assignment should not be flagged"
        );
    }
}
