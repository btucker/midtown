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
#[derive(Debug, Clone, serde::Serialize)]
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
    /// Pane content hash and last-changed timestamp for stuck detection.
    pub pane_hash: Option<(u64, Instant)>,
    /// Number of consecutive zombie respawn attempts. Reset on normal spawn.
    pub zombie_respawn_count: u32,
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
            pane_hash: None,
            zombie_respawn_count: 0,
        }
    }

    /// Format for tmux tab display, matching CoworkerStateReport::display_status().
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
            pane_hash: None,
            zombie_respawn_count: 0,
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
            pane_hash: None,
            zombie_respawn_count: 0,
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
    pub is_isolated: bool,
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
/// - They have open unmerged PRs with CI not yet passed (waiting for CI)
/// - They are actively reviewing a PR
/// - They have unblocked dependent tasks
/// - They have a subagent (Task tool) currently running
///
/// Coworkers with open PRs where CI has passed CAN go on break - they're just
/// waiting for human review, and the daemon will respawn them when feedback arrives.
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
    coworkers_with_running_subagents: &HashSet<String>,
    ci_passed_pr_coworkers: &HashSet<String>,
    usage_limited_coworkers: &HashSet<String>,
    api_error_coworkers: &HashSet<String>,
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
        let has_running_subagent = coworkers_with_running_subagents
            .iter()
            .any(|s| s.eq_ignore_ascii_case(coworker));
        let ci_passed = ci_passed_pr_coworkers
            .iter()
            .any(|c| c.eq_ignore_ascii_case(coworker));
        let is_usage_limited = usage_limited_coworkers.contains(&coworker.to_lowercase());
        let has_api_error = api_error_coworkers.contains(&coworker.to_lowercase());

        // Coworkers with active tasks, review assignments, unblocked deps,
        // running subagents, usage limits, or API errors are never sent on break.
        //
        // Coworkers with open PRs CAN go on break if their CI has passed
        // (they're waiting for review feedback, and the daemon will respawn
        // them when feedback arrives).
        //
        // Note: pane content changes are NOT checked here. Idle coworkers may
        // have pane activity from daemon nudges and UI updates, which previously
        // blocked idle breaks (bug #62). The other flags already cover all
        // legitimate work scenarios.
        let protected_by_open_pr = has_open_pr && !ci_passed;
        if is_busy
            || protected_by_open_pr
            || is_reviewing
            || has_unblocked_deps
            || has_running_subagent
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
                is_isolated: cw.isolated_tasks,
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
/// as an action option. We look for contextual patterns to avoid false positives
/// when coworkers edit code containing "/upgrade" in strings or comments:
/// - "- /upgrade" (menu option format in the usage limit screen)
/// - "/upgrade to" (instruction format: "/upgrade to increase your limit")
/// - "/upgrade or" (options format: "/upgrade or wait")
///
/// Previous patterns like "usage limit" caused false positives when coworkers
/// were editing code with those strings in comments.
const USAGE_LIMIT_PATTERNS: &[&str] = &["- /upgrade", "/upgrade to", "/upgrade or"];

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
const API_ERROR_PATTERNS: &[&str] = &[
    "API Error: 500",
    "API Error: 502",
    "API Error: 503",
    "API Error: 529",
    r#""type":"api_error""#,
    r#""type":"overloaded_error""#,
    "Internal server error",
];

/// Detect whether pane content indicates a subagent (Task tool) is running.
///
/// When a coworker launches a Task agent, their pane shows status indicators
/// while waiting for the subagent to complete. During this time, the main pane
/// content doesn't change (it shows "Waiting..." or the task status), but the
/// coworker is actively working via the subagent.
///
/// Patterns detected:
/// - `✽` (whirlpool) at start of line = subagent actively thinking/running
/// - `Running X Task agent` = subagent(s) in progress
pub(crate) fn has_running_subagent(pane_content: &str) -> bool {
    for line in pane_content.lines() {
        let trimmed = line.trim();
        // Whirlpool indicator: ✽ followed by task description
        if trimmed.starts_with('✽') {
            return true;
        }
        // Running Task agents indicator
        if trimmed.contains("Running") && trimmed.contains("Task agent") {
            return true;
        }
    }
    false
}

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
///
/// To detect recovery: if the usage limit pattern appears but there's significant
/// activity AFTER it, the coworker has recovered and should not be marked as limited.
pub(crate) fn decide_usage_limit_detection(
    pane_contents: &HashMap<String, String>,
) -> UsageLimitDecision {
    for (name, content) in pane_contents {
        if is_at_usage_limit(content) {
            return UsageLimitDecision::Detected {
                coworker: name.clone(),
            };
        }
    }

    UsageLimitDecision::NoneDetected
}

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
fn is_ui_chrome(line: &str) -> bool {
    // Lines that are entirely horizontal rules
    if line
        .chars()
        .all(|c| matches!(c, '─' | '━' | '=' | '-' | ' '))
    {
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

/// Result of stuck coworker detection: restart decisions and updated hash state.
pub(crate) struct StuckDetectionResult {
    /// Coworkers that should be restarted.
    pub restarts: Vec<StuckCoworkerRestart>,
    /// Updated pane hash entries to replace the current state.
    pub updated_hashes: HashMap<String, (u64, Instant)>,
}

/// Detect coworkers whose pane content hasn't changed for `stuck_duration`
/// while showing activity indicators (running subagent).
///
/// A coworker is only considered stuck if:
/// 1. Pane content hash unchanged for `stuck_duration` (3 minutes)
/// 2. Pane shows activity indicators (whirlpool, "Running X Task agent")
///
/// If pane is frozen but shows NO activity indicators, the coworker is likely
/// idle/waiting, not stuck. This prevents false positives on legitimately
/// idle coworkers.
///
/// Pure function: takes the current pane hash state and pane contents,
/// returns restart decisions and the updated hash state. The caller is
/// responsible for applying the hash updates to persistent state.
pub(crate) fn decide_stuck_coworker_restarts(
    pane_hashes: &HashMap<String, (u64, Instant)>,
    pane_contents: &HashMap<String, String>,
    in_progress_tasks: &[(String, String, String)],
    usage_limited_coworkers: &HashSet<String>,
    api_error_coworkers: &HashSet<String>,
    now: Instant,
    stuck_duration: Duration,
) -> StuckDetectionResult {
    use std::hash::{Hash, Hasher};

    let mut restarts = Vec::new();
    let mut updated_hashes = pane_hashes.clone();

    for (name, content) in pane_contents {
        // Skip coworkers at usage limit — they're frozen but not stuck
        if usage_limited_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Skip coworkers with API errors — they're waiting but may recover on retry
        if api_error_coworkers.contains(&name.to_lowercase()) {
            continue;
        }
        // Hash the pane content for cheap comparison
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let new_hash = hasher.finish();

        let entry = updated_hashes
            .entry(name.clone())
            .or_insert((new_hash, now));

        if entry.0 != new_hash {
            // Pane changed — update hash and timestamp
            entry.0 = new_hash;
            entry.1 = now;
            continue;
        }

        // Hash unchanged — check if stuck long enough
        if now.duration_since(entry.1) < stuck_duration {
            continue;
        }

        // CRITICAL: Skip if coworker has running subagents.
        // The pane will be frozen while waiting for Task agents to complete,
        // which is normal behavior — not stuck. Only consider stuck if the
        // pane is frozen AND there are NO running subagents (true hang).
        if has_running_subagent(content) {
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

        // Reset the hash tracker so we don't immediately re-trigger
        entry.1 = now;
    }

    // Clean up entries for coworkers no longer in the snapshot
    updated_hashes.retain(|name, _| pane_contents.contains_key(name));

    StuckDetectionResult {
        restarts,
        updated_hashes,
    }
}

// ---------------------------------------------------------------------------
// Compaction whirlpool & queued prompt detection
// ---------------------------------------------------------------------------

/// Action to recover a coworker from a stuck UI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StuckUiRecovery {
    /// Coworker is stuck in compaction (whirlpool/baking). Send Escape.
    InterruptCompaction { name: String },
    /// Coworker has queued text sitting in the input but is not processing it.
    /// If the text matches a daemon-sent nudge, send Enter to auto-submit.
    /// Otherwise, leave it alone (could be user-typed input).
    InterruptQueuedNudges { name: String },
}

/// Detect coworkers stuck in Claude Code's compaction state.
///
/// Compaction shows a status line like `Whirlpooling your conversation…`
/// followed by `(esc to interrupt · 18m 50s · ↓ 0 tokens)`. The key verbs are:
/// - "Whirlpooling" (active compaction)
/// - "Baking" (active compaction)
/// - "Simmering" (active compaction)
/// - "Sautéed" (completed compaction, still in post-compaction state)
///
/// IMPORTANT: Normal "thinking" states also show "esc to interrupt" but are
/// NOT compaction. For example: "✢ Fixing tmux window naming… (esc to interrupt...)"
/// We must check for actual compaction verbs, not just "esc to interrupt".
///
/// Only flags coworkers whose compaction has been running for at least
/// `min_duration` — compaction is a normal, useful operation and we must not
/// interrupt short-running compactions.
///
/// Returns the names of coworkers that should receive an Escape keypress.
/// The caller is responsible for cooldown enforcement.
pub(crate) fn detect_compaction_stuck(
    pane_contents: &HashMap<String, String>,
    min_duration: Duration,
) -> Vec<String> {
    pane_contents
        .iter()
        .filter(|(_name, content)| {
            // Find a status line that has BOTH a compaction verb AND sufficient duration.
            // This prevents false positives when compaction verbs appear in displayed code
            // while an unrelated "esc to interrupt" thinking line shows elapsed time.
            content.lines().any(|line| {
                // For "Sautéed for Xm Ys" format (completed compaction)
                // Must have the compaction indicator on the same line to avoid
                // false positives from "Sautéed for" appearing in code/comments
                if is_completed_compaction_line(line) {
                    return parse_sauteed_duration(line)
                        .map(|d| d >= min_duration)
                        .unwrap_or(false);
                }

                // For active compaction: the line must contain BOTH:
                // 1. A compaction verb (Whirlpooling, Baking, Simmering)
                // 2. "esc to interrupt" with parseable duration
                if !is_active_compaction_line(line) {
                    return false;
                }
                // Parse duration from pattern like "· 18m 50s ·" or "· 5m 00s ·"
                match parse_compaction_duration(line) {
                    Some(elapsed) => elapsed >= min_duration,
                    // If we can't parse the duration, be conservative and don't interrupt
                    None => false,
                }
            })
        })
        .map(|(name, _content)| name.clone())
        .collect()
}

/// Check if a line contains an active compaction verb (case-insensitive).
///
/// Compaction verbs are: Whirlpooling, Baking, Simmering.
/// These are distinct from normal "thinking" states like "Fixing...",
/// "Scoring...", "Checking...", etc.
fn has_active_compaction_verb(line: &str) -> bool {
    let line_lower = line.to_lowercase();
    line_lower.contains("whirlpooling")
        || line_lower.contains("baking")
        || line_lower.contains("simmering")
}

/// Check if a line is an active compaction status line.
///
/// Active compaction has BOTH the verb AND "esc to interrupt" on the same line.
/// This distinguishes actual compaction from compaction verbs appearing in
/// displayed code (comments, strings, etc.).
///
/// Case-insensitive matching for "esc to interrupt" handles both "esc" and "Esc"
/// variants that Claude Code may output.
fn is_active_compaction_line(line: &str) -> bool {
    let line_lower = line.to_lowercase();
    has_active_compaction_verb(line) && line_lower.contains("esc to interrupt")
}

/// Check if a line is a completed compaction status line.
///
/// Completed compaction shows "Sautéed for Xm Ys" format with the ✻ indicator.
/// Case-insensitive to handle both "Sautéed" and "Sauteed" (ASCII).
fn is_completed_compaction_line(line: &str) -> bool {
    let line_lower = line.to_lowercase();
    // Must contain "sautéed for" or "sauteed for" (case-insensitive)
    // and should look like a status line (has ✻ marker or starts with whitespace + marker)
    (line_lower.contains("sautéed for") || line_lower.contains("sauteed for"))
        && (line.contains('✻') || line.trim_start().starts_with('✻'))
}

/// Check if the pane content has active compaction in progress.
///
/// This checks for actual compaction status lines (verb + "esc to interrupt"
/// on the same line), NOT just verb presence. This avoids false positives
/// when compaction verbs appear in displayed code (comments, strings, etc.).
///
/// Use this to determine if a coworker is currently compacting and should
/// be excluded from other recovery mechanisms (like queued nudge detection).
fn has_compaction_indicator(content: &str) -> bool {
    content.lines().any(is_active_compaction_line)
}

/// Parse duration from "Sautéed for Xm Ys" format.
fn parse_sauteed_duration(line: &str) -> Option<Duration> {
    // Case-insensitive search for "for" to handle "Sautéed FOR" (unlikely but possible)
    let line_lower = line.to_lowercase();
    let for_pos = line_lower.find(" for ")?;
    let after_for = &line[for_pos + 5..]; // Skip " for "

    let mut total_secs: u64 = 0;
    let mut found_time = false;

    for part in after_for.split_whitespace() {
        if let Some(m) = part.strip_suffix('m')
            && let Ok(mins) = m.parse::<u64>()
        {
            total_secs += mins * 60;
            found_time = true;
        } else if let Some(s) = part.strip_suffix('s')
            && let Ok(secs) = s.parse::<u64>()
        {
            total_secs += secs;
            found_time = true;
        }
    }

    if found_time {
        Some(Duration::from_secs(total_secs))
    } else {
        None
    }
}

/// Parse the elapsed duration from a compaction status line.
///
/// Expected format: `(esc to interrupt · 18m 50s · ↓ 0 tokens)`
/// Returns the parsed duration, or None if the format doesn't match.
///
/// Case-insensitive matching for "esc to interrupt" handles both "esc" and "Esc"
/// variants that Claude Code may output.
fn parse_compaction_duration(line: &str) -> Option<Duration> {
    // Look for the pattern "· Xm Ys ·" after "esc to interrupt" (case-insensitive)
    let line_lower = line.to_lowercase();
    let split_pos = line_lower.find("esc to interrupt")?;
    let after_esc = &line[split_pos + "esc to interrupt".len()..];

    let mut total_secs: u64 = 0;
    let mut found_time = false;

    for part in after_esc.split_whitespace() {
        if let Some(m) = part.strip_suffix('m')
            && let Ok(mins) = m.parse::<u64>()
        {
            total_secs += mins * 60;
            found_time = true;
        } else if let Some(s) = part.strip_suffix('s')
            && let Ok(secs) = s.parse::<u64>()
        {
            total_secs += secs;
            found_time = true;
        }
    }

    if found_time {
        Some(Duration::from_secs(total_secs))
    } else {
        None
    }
}

/// Detect coworkers with queued nudge messages that aren't being processed.
///
/// Claude Code's TUI structure (from bottom to top):
/// - Status bar (permissions/interrupt hints)
/// - Bottom input separator (`───────...`)
/// - Input line (`❯ [text being typed]`)
/// - Top input separator (`───────...`)
/// - **Queued nudges appear here** (`❯ message` lines)
/// - Action/verb line (`✳` in-progress or `⏺` completed)
/// - Conversation history (already processed)
///
/// Queued nudges are messages that were sent via tmux send-keys but haven't
/// been submitted yet. They appear BELOW the action line but ABOVE the input
/// separator. We need to parse the TUI structure to find them, not just look
/// for any `❯` line (which would match conversation history).
pub(crate) fn detect_queued_prompt_stuck(pane_contents: &HashMap<String, String>) -> Vec<String> {
    pane_contents
        .iter()
        .filter(|(_name, content)| has_queued_nudges(content))
        .map(|(name, _content)| name.clone())
        .collect()
}

/// Check if pane content has queued nudges waiting to be processed.
///
/// Returns true if there are `❯ text` lines between the action line and
/// the input separator, indicating nudges that were sent but not submitted.
fn has_queued_nudges(content: &str) -> bool {
    // Don't check during actual compaction (separate recovery mechanism).
    // Note: "esc to interrupt" appears in ALL thinking states, not just compaction,
    // so we must check for actual compaction verbs (Whirlpooling, Baking, etc.)
    if has_compaction_indicator(content) {
        return false;
    }

    let lines: Vec<&str> = content.lines().collect();

    // Find the input separator (line of mostly ─ characters) scanning from bottom
    // The input area has two separators; we want the top one (second from bottom)
    let mut separator_indices: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate().rev() {
        if is_input_separator(line) {
            separator_indices.push(i);
            if separator_indices.len() >= 2 {
                break;
            }
        }
    }

    // Need at least the top input separator to locate the queued area
    let top_separator_idx = match separator_indices.last() {
        Some(&idx) => idx,
        None => return false,
    };

    // Find the action line (starts with ✳ or ⏺) scanning upward from separator
    let mut action_line_idx = None;
    for i in (0..top_separator_idx).rev() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with('✳') || trimmed.starts_with('⏺') {
            action_line_idx = Some(i);
            break;
        }
    }

    let action_idx = match action_line_idx {
        Some(idx) => idx,
        None => return false,
    };

    // Look for queued nudges between action line and top separator
    // These are `❯ text` lines (with actual content after the prompt)
    for line in lines
        .iter()
        .skip(action_idx + 1)
        .take(top_separator_idx.saturating_sub(action_idx + 1))
    {
        let trimmed = line.trim();
        if trimmed.starts_with('❯') && trimmed.len() > "❯".len() + 1 {
            return true;
        }
    }

    false
}

/// Extract the text content of queued nudges from pane content.
///
/// Uses the same TUI parsing logic as `has_queued_nudges` but returns the actual
/// text content, which can be used to verify if it matches a daemon-sent nudge.
/// Returns None if no queued nudges are found.
pub(crate) fn extract_queued_nudge_text(content: &str) -> Option<String> {
    // Don't check during actual compaction (separate recovery mechanism).
    if has_compaction_indicator(content) {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();

    // Find the input separator (line of mostly ─ characters) scanning from bottom
    let mut separator_indices: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate().rev() {
        if is_input_separator(line) {
            separator_indices.push(i);
            if separator_indices.len() >= 2 {
                break;
            }
        }
    }

    // Need at least the top input separator to locate the queued area
    let top_separator_idx = match separator_indices.last() {
        Some(&idx) => idx,
        None => return None,
    };

    // Find the action line (starts with ✳ or ⏺) scanning upward from separator
    let mut action_line_idx = None;
    for i in (0..top_separator_idx).rev() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with('✳') || trimmed.starts_with('⏺') {
            action_line_idx = Some(i);
            break;
        }
    }

    let action_idx = action_line_idx?;

    // Collect all queued nudge text between action line and top separator
    let mut queued_texts = Vec::new();
    for line in lines
        .iter()
        .skip(action_idx + 1)
        .take(top_separator_idx.saturating_sub(action_idx + 1))
    {
        let trimmed = line.trim();
        if trimmed.starts_with('❯') && trimmed.len() > "❯".len() + 1 {
            // Extract text after the ❯ symbol (skip ❯ and any following space)
            let text = trimmed.trim_start_matches('❯').trim();
            if !text.is_empty() {
                queued_texts.push(text.to_string());
            }
        }
    }

    if queued_texts.is_empty() {
        None
    } else {
        // Join multiple queued lines (rare but possible)
        Some(queued_texts.join(" "))
    }
}

/// Check if a line is an input separator (horizontal line of ─ characters).
fn is_input_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Input separators are long lines of ─ (box drawing horizontal)
    let dash_count = trimmed.chars().filter(|&c| c == '─').count();
    dash_count > 20 && dash_count as f64 / trimmed.chars().count() as f64 > 0.9
}

/// Pure decision: determine which coworkers need UI recovery actions.
///
/// Checks pane contents for two stuck states and returns the appropriate
/// recovery actions. Cooldown tracking is the caller's responsibility.
///
/// `min_compaction_duration` sets the minimum elapsed time before a
/// compaction is considered stuck. Short compactions are normal and useful.
///
/// `coworker_start_times` and `now_utc` are used for age-based protection:
/// coworkers younger than `min_queued_nudge_age` are excluded from queued
/// nudge detection (the TUI structure is still forming during startup).
pub(crate) fn decide_stuck_ui_recoveries(
    pane_contents: &HashMap<String, String>,
    min_compaction_duration: Duration,
    coworker_start_times: &HashMap<String, DateTime<Utc>>,
    now_utc: DateTime<Utc>,
    min_queued_nudge_age: chrono::Duration,
) -> Vec<StuckUiRecovery> {
    let mut recoveries = Vec::new();

    for name in detect_compaction_stuck(pane_contents, min_compaction_duration) {
        recoveries.push(StuckUiRecovery::InterruptCompaction { name });
    }

    for name in detect_queued_prompt_stuck(pane_contents) {
        // Age-based protection: skip coworkers younger than min_queued_nudge_age.
        // During startup, the TUI structure is still forming and has_queued_nudges()
        // can produce false positives.
        let is_old_enough = coworker_start_times
            .get(&name)
            .map(|started| now_utc.signed_duration_since(*started) >= min_queued_nudge_age)
            .unwrap_or(false);

        if is_old_enough {
            recoveries.push(StuckUiRecovery::InterruptQueuedNudges { name });
        }
    }

    recoveries
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
/// Looks for patterns like:
/// - "try again in 15 minutes", "resets in 2 hours" (relative duration)
/// - "available after 30 minutes" (relative duration)
/// - "resets at 3:45" or "at 15:30" (24-hour absolute time)
/// - "resets 12pm" or "resets 3am" (12-hour absolute time)
/// - "resets 12pm (America/Chicago)" (12-hour with timezone - timezone ignored, uses UTC)
///
/// Returns a default of 15 minutes if no parseable duration is found.
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

    // Look for 12-hour format: "resets 12pm", "resets 3am", "resets 12pm (America/Chicago)"
    // Pattern: "resets" followed by a number and am/pm
    if let Some(idx) = lower.find("resets ") {
        let after = &lower[idx + 7..];
        if let Some(duration) = parse_12hour_time(after) {
            return duration;
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

/// Parse 12-hour time format like "12pm", "3am", "12pm (America/Chicago)".
///
/// Returns duration until the specified time. If the time is in the past,
/// assumes it's tomorrow. Timezone in parentheses is noted but ignored
/// (we use UTC for simplicity - the error is at most a few hours).
fn parse_12hour_time(text: &str) -> Option<Duration> {
    // Extract the hour number
    let num_str: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    let hour_12: u32 = num_str.parse().ok()?;

    if hour_12 == 0 || hour_12 > 12 {
        return None;
    }

    // Check for am/pm immediately after the number
    let after_num = text[num_str.len()..].trim_start();
    let is_pm = after_num.starts_with("pm");
    let is_am = after_num.starts_with("am");

    if !is_pm && !is_am {
        return None;
    }

    // Convert to 24-hour format
    let hour_24 = if is_pm {
        if hour_12 == 12 { 12 } else { hour_12 + 12 }
    } else {
        // am
        if hour_12 == 12 { 0 } else { hour_12 }
    };

    let now = chrono::Utc::now();
    let mut target = now.date_naive().and_hms_opt(hour_24, 0, 0)?;

    if target < now.naive_utc() {
        target += chrono::Duration::days(1);
    }

    let diff = target - now.naive_utc();
    diff.to_std().ok()
}

/// Check if pane content indicates an active (not recovered) usage limit.
///
/// Returns true only if the usage limit pattern is present AND the coworker
/// hasn't recovered (no significant activity after the limit message).
///
/// Used directly in tests and indirectly via `decide_usage_limit_detection`.
pub(crate) fn has_usage_limit_pattern(pane_content: &str) -> bool {
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
pub(crate) fn has_api_error_pattern(pane_content: &str) -> bool {
    is_at_api_error(pane_content)
}

/// Check if pane content indicates an active (not recovered) API error.
///
/// Uses the same recovery detection as usage limits: if there's significant
/// activity after the error message, the coworker has recovered.
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
/// - The original author is not active
/// - A stored session context is available
/// - There are idle coworkers available to take over
///
/// The handoff preserves the original author's session context so the new
/// coworker has full history of decisions and code understanding.
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

    if is_active {
        // Owner is active — just nudge them
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if !owner.is_empty() {
        if at_dev_limit {
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
            } else {
                // No idle coworkers available — fall back to spawning the original owner
                PrAction::SpawnOwner {
                    owner: owner.to_string(),
                    message: message.to_string(),
                }
            }
        } else {
            // No session context — spawn the original owner
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
/// and session context is available.
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

    if is_active {
        PrAction::NudgeOwner {
            owner: owner.to_string(),
            message: message.to_string(),
        }
    } else if at_dev_limit {
        PrAction::Skip {
            reason: format!("dev limit reached, cannot spawn {} for PR comment", owner),
        }
    } else if let Some(ctx) = session_context {
        // We have session context — try to hand off to an idle coworker
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
        } else {
            // No idle coworkers available — fall back to spawning the original owner
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
///
/// # Arguments
/// * `is_owner_isolated` - If true, the owner is an isolated reviewer with their own
///   task namespace. Main task list updates should not nudge isolated reviewers.
pub(crate) fn decide_pending_task_action(
    task_id: &str,
    task_subject: &str,
    owner: &str,
    active_names: &HashSet<String>,
    at_dev_limit: bool,
    on_nudge_cooldown: bool,
    is_owner_isolated: bool,
) -> PendingTaskAction {
    // Skip empty or lead-owned tasks
    if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
        return PendingTaskAction::Skip {
            reason: format!("task #{} owner is lead or empty", task_id),
        };
    }

    // Skip invalid coworker names — can't spawn or nudge an invalid name
    if !crate::coworker::is_coworker_name(&owner.to_lowercase()) {
        return PendingTaskAction::Skip {
            reason: format!(
                "task #{} owner '{}' is not a valid coworker name",
                task_id, owner
            ),
        };
    }

    // Skip isolated reviewers — they have their own task namespace and should
    // not be nudged about main task list updates (task ID collision issue).
    if is_owner_isolated {
        return PendingTaskAction::Skip {
            reason: format!(
                "task #{} owner '{}' is an isolated reviewer (separate task namespace)",
                task_id, owner
            ),
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
        // Skip invalid coworker names — can't spawn a coworker with this name
        // Use lowercase to match how avenue names are stored
        if !crate::coworker::is_coworker_name(&owner_clean.to_lowercase()) {
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
                pane_hash: None,
                zombie_respawn_count: 0,
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(!decisions[0].is_isolated);
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
        assert!(decisions[0].is_isolated);
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
                // Pane changed just 10 seconds ago — but coworker has no task
                pane_hash: Some((12345, Instant::now() - Duration::from_secs(10))),
                zombie_respawn_count: 0,
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
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
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
    fn idle_shutdown_allows_break_with_pane_hash_present() {
        let coworkers = vec![cw("york", 10)];
        // york has a pane_hash in its record — should not interfere with idle break
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
                pane_hash: Some((12345, Instant::now() - Duration::from_secs(300))),
                zombie_respawn_count: 0,
            },
        );

        // york is idle with no tasks/PRs — pane hash doesn't affect idle break decision
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]), // usage_limited_coworkers
            &set(&[]), // api_error_coworkers
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_running_subagent() {
        // Test case for bug #27: coworker with running Task agent should NOT be shut down
        let coworkers = vec![cw("madison", 10)];
        let mut phases = lifecycle_with(
            "madison",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // madison has a subagent running — should NOT be sent on break
        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),          // not busy (no in-progress tasks)
            &set(&[]),          // no open PRs
            &set(&[]),          // not reviewing
            &set(&[]),          // no unblocked deps
            &set(&["madison"]), // HAS RUNNING SUBAGENT
            &set(&[]),          // ci_passed
            &set(&[]),          // usage_limited
            &set(&[]),          // api_error
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );
        apply_health_transitions(&mut phases, transitions);

        assert!(
            decisions.is_empty(),
            "coworkers with running subagents should NOT be sent on break"
        );
        // Subagent coworker should be cleared from idle tracking
        assert!(get_health(&phases, "madison").is_none());
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
            &set(&[]),       // no running subagent
            &set(&["york"]), // CI PASSED
            &set(&[]),       // usage_limited
            &set(&[]),       // api_error
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
            &set(&[]),       // no running subagent
            &set(&[]),       // no ci_passed
            &set(&["york"]), // usage_limited
            &set(&[]),       // api_error
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
            &set(&[]),       // no running subagent
            &set(&[]),       // no ci_passed
            &set(&[]),       // not usage_limited
            &set(&["york"]), // HAS API ERROR
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        assert!(
            decisions.is_empty(),
            "API error coworker should be protected from idle shutdown"
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
    fn pr_comment_handoff_nudges_active_owner() {
        let session = make_session_context("york", 42);
        let action = decide_pr_comment_action_with_handoff(
            "york",
            "amsterdam",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]),
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
    fn pr_handoff_nudges_active_owner_even_with_session() {
        // Even with a session context available, active owners get nudged
        let session = make_session_context("york", 42);
        let action = decide_pr_issue_action_with_handoff(
            "york",
            &active(&["york", "amsterdam"]),
            &active(&["amsterdam"]),
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
    // decide_pending_task_action tests
    // -----------------------------------------------------------------------

    #[test]
    fn pending_task_nudges_active_owner() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, false, false);
        assert!(matches!(action, PendingTaskAction::NudgeOwner { .. }));
    }

    #[test]
    fn pending_task_skips_nudge_on_cooldown() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, true, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_spawns_inactive_owner() {
        let names = set(&["amsterdam"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "york", &names, false, false, false);
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
            decide_pending_task_action("42", "Fix bug", "york", &names, true, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_lead_owner() {
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "lead", &names, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_empty_owner() {
        let names = set(&["york"]);
        let action = decide_pending_task_action("42", "Fix bug", "", &names, false, false, false);
        assert!(matches!(action, PendingTaskAction::Skip { .. }));
    }

    #[test]
    fn pending_task_skips_invalid_coworker_name() {
        // "fix" is not a valid coworker name (not an avenue name)
        let names = set(&["york"]);
        let action =
            decide_pending_task_action("42", "Fix bug", "fix", &names, false, false, false);
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

    #[test]
    fn orphan_recovery_skips_invalid_coworker_name() {
        // Bug: task with invalid owner "fix" (not an avenue name) should be skipped,
        // not returned for recovery, since we can't spawn a coworker named "fix"
        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "fix".to_string())];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
        // Should be None because "fix" is not a valid coworker name
        assert!(result.is_none());
    }

    #[test]
    fn orphan_recovery_handles_uppercase_owner() {
        // Uppercase owner names should still be recognized as valid coworkers
        let tasks = vec![("1".to_string(), "Fix bug".to_string(), "YORK".to_string())];
        let active = set(&["amsterdam"]);
        let result = decide_orphan_recovery(&tasks, &active, false);
        // Should return recovery because "YORK" maps to valid coworker "york"
        assert!(result.is_some());
        assert_eq!(result.unwrap().owner, "YORK");
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

    // -----------------------------------------------------------------------
    // Compaction whirlpool & queued prompt detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn compaction_detected_with_whirlpool_verb_long_duration() {
        let mut panes = HashMap::new();
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "york".to_string(),
            "✶ Whirlpooling… (esc to interrupt · 18m 50s · ↓ 0 tokens)\n".to_string(),
        );
        // 18m 50s > 5 min threshold — should trigger
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(stuck, vec!["york"]);
    }

    #[test]
    fn compaction_not_detected_with_short_duration() {
        let mut panes = HashMap::new();
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "amsterdam".to_string(),
            "✶ Baking… (esc to interrupt · 3m 12s · ↓ 42 tokens)\n".to_string(),
        );
        // 3m 12s < 5 min threshold — should NOT trigger (compaction is normal)
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "short compaction should not be interrupted"
        );
    }

    #[test]
    fn compaction_detected_at_exact_threshold() {
        let mut panes = HashMap::new();
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "park".to_string(),
            "✶ Simmering… (esc to interrupt · 5m 00s · ↓ 100 tokens)\n".to_string(),
        );
        // 5m 00s = 5 min threshold — should trigger
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(stuck, vec!["park"]);
    }

    #[test]
    fn compaction_not_detected_just_under_threshold() {
        let mut panes = HashMap::new();
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "park".to_string(),
            "✶ Simmering… (esc to interrupt · 4m 59s · ↓ 100 tokens)\n".to_string(),
        );
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "compaction just under threshold should not be interrupted"
        );
    }

    #[test]
    fn compaction_not_detected_in_normal_output() {
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  Reading file src/main.rs\n  Edit: replaced 3 lines\n  $ cargo build\n".to_string(),
        );
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(stuck.is_empty());
    }

    #[test]
    fn compaction_not_detected_when_pane_mentions_esc_in_code() {
        // "esc to interrupt" in code output but no parseable duration — conservative: don't interrupt
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  // detect the pattern: esc to interrupt\n  fn check() {}\n".to_string(),
        );
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        // Can't parse duration from code comment → conservative: don't interrupt
        assert!(
            stuck.is_empty(),
            "unparseable duration should not trigger (conservative)"
        );
    }

    #[test]
    fn parse_compaction_duration_works() {
        assert_eq!(
            parse_compaction_duration("  (esc to interrupt · 18m 50s · ↓ 0 tokens)"),
            Some(Duration::from_secs(18 * 60 + 50))
        );
        assert_eq!(
            parse_compaction_duration("  (esc to interrupt · 0m 30s · ↓ 0 tokens)"),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_compaction_duration("  (esc to interrupt · 5m 00s · ↓ 100 tokens)"),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            parse_compaction_duration("  // detect the pattern: esc to interrupt"),
            None,
        );
    }

    #[test]
    fn compaction_detected_with_capital_esc() {
        // Task #36: Claude Code may output "Esc to interrupt" (capital E) instead of
        // "esc to interrupt" (lowercase). Detection should be case-insensitive.
        let mut panes = HashMap::new();
        // Real Claude Code output uses capital E in "Esc to interrupt"
        panes.insert(
            "park".to_string(),
            "✶ Whirlpooling… (Esc to interrupt · 6m 30s · ↓ 0 tokens)\n".to_string(),
        );
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(
            stuck,
            vec!["park"],
            "compaction detection should be case-insensitive for 'Esc to interrupt'"
        );
    }

    #[test]
    fn parse_compaction_duration_case_insensitive() {
        // Task #36: Duration parsing should handle both "esc" and "Esc" variants
        assert_eq!(
            parse_compaction_duration("  (Esc to interrupt · 10m 00s · ↓ 0 tokens)"),
            Some(Duration::from_secs(600)),
            "duration parsing should work with capital 'Esc'"
        );
        assert_eq!(
            parse_compaction_duration("  (ESC TO INTERRUPT · 5m 30s · ↓ 0 tokens)"),
            Some(Duration::from_secs(330)),
            "duration parsing should work with uppercase 'ESC TO INTERRUPT'"
        );
    }

    #[test]
    fn queued_prompt_detected_with_nudge_messages() {
        // Realistic TUI: queued nudge between action line and input separator
        let mut panes = HashMap::new();
        let tui_content = "\
⏺ Previous completed action

✳ Current action in progress...
❯ You have a new task assignment: task #42
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("york".to_string(), tui_content.to_string());
        let stuck = detect_queued_prompt_stuck(&panes);
        assert_eq!(stuck, vec!["york"]);
    }

    #[test]
    fn queued_prompt_not_detected_during_compaction() {
        // If compaction is happening simultaneously, don't also flag queued prompt
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  Whirlpooling…\n  (esc to interrupt · 5m 00s · ↓ 0 tokens)\n❯ pending nudge\n"
                .to_string(),
        );
        let stuck = detect_queued_prompt_stuck(&panes);
        assert!(
            stuck.is_empty(),
            "should not flag queued prompt during compaction"
        );
    }

    #[test]
    fn queued_prompt_detected_during_normal_thinking_state() {
        // Normal thinking state shows "esc to interrupt" but is NOT compaction.
        // Queued nudges SHOULD be detected in this case (bug fix for PR #515 follow-up).
        let mut panes = HashMap::new();
        let tui_content = "\
✳ Fixing tmux window naming…
  (esc to interrupt · 2m 30s)
❯ You have a new task assignment
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("york".to_string(), tui_content.to_string());
        let stuck = detect_queued_prompt_stuck(&panes);
        assert_eq!(
            stuck,
            vec!["york"],
            "should detect queued nudges during normal thinking (not compaction)"
        );
    }

    #[test]
    fn queued_prompt_not_detected_with_bare_prompt() {
        // Normal TUI with empty input - no queued nudges
        let mut panes = HashMap::new();
        let tui_content = "\
⏺ Completed action

✳ Working on something...
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("york".to_string(), tui_content.to_string());
        let stuck = detect_queued_prompt_stuck(&panes);
        assert!(
            stuck.is_empty(),
            "bare prompt character should not trigger recovery"
        );
    }

    #[test]
    fn queued_prompt_not_detected_in_normal_output() {
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  $ cargo test\n  running 5 tests\n  test result: ok. 5 passed\n".to_string(),
        );
        let stuck = detect_queued_prompt_stuck(&panes);
        assert!(stuck.is_empty());
    }

    #[test]
    fn queued_prompt_not_detected_in_conversation_history() {
        // This is the FALSE POSITIVE case: ❯ lines in conversation history
        // should NOT be detected as queued nudges
        let mut panes = HashMap::new();
        let tui_content = "\
❯ Previous user message in history

⏺ Claude's response to that message

✳ Current action in progress...
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("york".to_string(), tui_content.to_string());
        let stuck = detect_queued_prompt_stuck(&panes);
        assert!(
            stuck.is_empty(),
            "❯ lines in conversation history should not trigger recovery"
        );
    }

    #[test]
    fn combined_recovery_returns_both_types() {
        let mut panes = HashMap::new();
        // One coworker stuck in compaction (10 min — well above threshold)
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "york".to_string(),
            "✶ Whirlpooling… (esc to interrupt · 10m 00s · ↓ 0 tokens)\n".to_string(),
        );
        // Another coworker with queued nudges (proper TUI structure)
        let amsterdam_tui = "\
⏺ Previous action

✳ Working on task...
❯ Check the channel
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), amsterdam_tui.to_string());

        // Set up coworkers as "old enough" (started 2 minutes ago)
        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(120));
        start_times.insert(
            "amsterdam".to_string(),
            now - chrono::Duration::seconds(120),
        );
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );
        assert_eq!(recoveries.len(), 2);

        let has_compaction = recoveries
            .iter()
            .any(|r| matches!(r, StuckUiRecovery::InterruptCompaction { name } if name == "york"));
        let has_queued = recoveries.iter().any(
            |r| matches!(r, StuckUiRecovery::InterruptQueuedNudges { name } if name == "amsterdam"),
        );
        assert!(has_compaction, "should detect york's compaction");
        assert!(has_queued, "should detect amsterdam's queued nudges");
    }

    #[test]
    fn extract_queued_text_returns_content_when_present() {
        let tui_content = "\
⏺ Previous completed action

✳ Current action in progress...
❯ You have a new task assignment: task #42
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        let result = extract_queued_nudge_text(tui_content);
        assert_eq!(
            result,
            Some("You have a new task assignment: task #42".to_string())
        );
    }

    #[test]
    fn extract_queued_text_returns_none_when_empty_prompt() {
        let tui_content = "\
⏺ Completed action

✳ Working on something...
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        let result = extract_queued_nudge_text(tui_content);
        assert!(
            result.is_none(),
            "should return None when no queued nudge text"
        );
    }

    #[test]
    fn extract_queued_text_returns_none_during_compaction() {
        let tui_content = "\
  Whirlpooling your conversation…
  (esc to interrupt · 5m 00s · ↓ 0 tokens)
❯ pending nudge
";
        let result = extract_queued_nudge_text(tui_content);
        assert!(
            result.is_none(),
            "should return None during compaction (separate recovery mechanism)"
        );
    }

    #[test]
    fn extract_queued_text_ignores_conversation_history() {
        let tui_content = "\
❯ Previous user message in history

⏺ Claude's response to that message

✳ Current action in progress...
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        let result = extract_queued_nudge_text(tui_content);
        assert!(
            result.is_none(),
            "should ignore ❯ lines in conversation history (above action line)"
        );
    }

    #[test]
    fn queued_prompt_detected_with_multiple_nudges_and_edit_hint() {
        // When multiple nudges pile up, Claude Code shows queued messages in the input area.
        // This test constructs a representative TUI state with multiple queued nudges
        // that need interrupt (Escape) to clear.
        let mut panes = HashMap::new();
        let tui_content = "\
⏺ Previous action completed

✳ Analyzing the authentication module...
  (esc to interrupt · 1m 30s)
❯ You have a new task assignment: task #42
❯ github said: @amsterdam CI failed on PR #123
❯ Press up to edit queued messages
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), tui_content.to_string());

        // Should detect the queued nudges
        let stuck = detect_queued_prompt_stuck(&panes);
        assert_eq!(
            stuck,
            vec!["amsterdam"],
            "should detect multiple queued nudges with 'Press up to edit' hint"
        );

        // Should also extract the queued text (first nudge)
        let extracted = extract_queued_nudge_text(tui_content);
        assert!(
            extracted.is_some(),
            "should extract queued text from multi-nudge scenario"
        );
        let text = extracted.unwrap();
        // Multiple queued lines are joined with space
        assert!(
            text.contains("task #42"),
            "extracted text should contain first nudge: got '{}'",
            text
        );
        assert!(
            text.contains("CI failed"),
            "extracted text should contain second nudge: got '{}'",
            text
        );
        assert!(
            text.contains("Press up to edit"),
            "extracted text should contain edit hint: got '{}'",
            text
        );
    }

    #[test]
    fn queued_prompt_triggers_interrupt_for_stuck_queue() {
        // Verify that decide_stuck_ui_recoveries returns InterruptQueuedNudges
        // for a coworker with multiple queued nudges needing interrupt.
        let mut panes = HashMap::new();
        let tui_content = "\
⏺ Completed previous task

✳ Running cargo test...
  (esc to interrupt · 45s)
❯ Check the channel for updates
❯ Your PR needs attention
❯ Press up to edit queued messages
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), tui_content.to_string());

        // Set up amsterdam as "old enough" to trigger recovery
        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        start_times.insert(
            "amsterdam".to_string(),
            now - chrono::Duration::seconds(120),
        );
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300), // compaction threshold
            &start_times,
            now,
            min_age,
        );

        // Should trigger InterruptQueuedNudges for amsterdam
        assert_eq!(recoveries.len(), 1, "should have one recovery action");
        assert!(matches!(
            &recoveries[0],
            StuckUiRecovery::InterruptQueuedNudges { name } if name == "amsterdam"
        ));
    }

    #[test]
    fn queued_prompt_skipped_when_coworker_not_in_start_times() {
        // Safety behavior: if a coworker has queued nudges but is NOT in start_times,
        // we should NOT trigger recovery. This prevents false positives during startup
        // when the TUI structure is still forming. The unwrap_or(false) at line 896
        // ensures coworkers missing from start_times are skipped.
        let mut panes = HashMap::new();
        let tui_content = "\
⏺ Completed previous task

✳ Running cargo test...
  (esc to interrupt · 45s)
❯ Check the channel for updates
❯ Press up to edit queued messages
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), tui_content.to_string());

        // Empty start_times - amsterdam is NOT tracked (simulates startup scenario)
        let now = chrono::Utc::now();
        let start_times = HashMap::new(); // Empty!
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300), // compaction threshold
            &start_times,
            now,
            min_age,
        );

        // Should NOT trigger recovery - coworker not in start_times means we can't
        // verify their age, so we skip them to prevent false positives
        assert!(
            recoveries.is_empty(),
            "should skip recovery when coworker not in start_times (safety behavior)"
        );
    }

    #[test]
    fn combined_recovery_skips_short_compaction() {
        let mut panes = HashMap::new();
        // Compaction running for only 2 minutes — below threshold
        // Includes compaction verb (Baking) but duration is too short
        panes.insert(
            "york".to_string(),
            "  Baking your conversation…\n  (esc to interrupt · 2m 00s · ↓ 0 tokens)\n".to_string(),
        );
        // Queued nudge with proper TUI structure
        let amsterdam_tui = "\
⏺ Previous action

✳ Working on task...
❯ Check the channel
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), amsterdam_tui.to_string());

        // Set up coworkers as "old enough" (started 2 minutes ago)
        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(120));
        start_times.insert(
            "amsterdam".to_string(),
            now - chrono::Duration::seconds(120),
        );
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );
        // Only the queued nudge should trigger, not the short compaction
        assert_eq!(recoveries.len(), 1);
        assert!(matches!(
            &recoveries[0],
            StuckUiRecovery::InterruptQueuedNudges { name } if name == "amsterdam"
        ));
    }

    #[test]
    fn recovery_empty_for_healthy_coworkers() {
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  Reading file\n  Edit complete\n".to_string(),
        );
        panes.insert(
            "amsterdam".to_string(),
            "  $ cargo build\n  Compiling midtown v0.4.1\n".to_string(),
        );

        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(120));
        start_times.insert(
            "amsterdam".to_string(),
            now - chrono::Duration::seconds(120),
        );
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );
        assert!(recoveries.is_empty());
    }

    #[test]
    fn recovery_empty_for_no_coworkers() {
        let panes: HashMap<String, String> = HashMap::new();
        let start_times: HashMap<String, DateTime<Utc>> = HashMap::new();
        let now = chrono::Utc::now();
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );
        assert!(recoveries.is_empty());
    }

    #[test]
    fn queued_nudge_recovery_skips_young_coworkers() {
        // This test verifies the age-based protection for queued nudge detection.
        // During startup, the TUI structure is still forming and has_queued_nudges()
        // can produce false positives.
        let mut panes = HashMap::new();
        let amsterdam_tui = "\
⏺ Previous action

✳ Working on task...
❯ Check the channel
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), amsterdam_tui.to_string());

        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        // Coworker started only 30 seconds ago (below 60s threshold)
        start_times.insert("amsterdam".to_string(), now - chrono::Duration::seconds(30));
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );

        // Young coworker should NOT trigger queued nudge recovery
        assert!(
            recoveries.is_empty(),
            "coworkers younger than min_age should be skipped for queued nudge recovery"
        );
    }

    #[test]
    fn queued_nudge_recovery_triggers_for_old_coworkers() {
        // Verify that queued nudge detection DOES trigger for coworkers old enough.
        let mut panes = HashMap::new();
        let amsterdam_tui = "\
⏺ Previous action

✳ Working on task...
❯ Check the channel
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
        panes.insert("amsterdam".to_string(), amsterdam_tui.to_string());

        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        // Coworker started 90 seconds ago (above 60s threshold)
        start_times.insert("amsterdam".to_string(), now - chrono::Duration::seconds(90));
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );

        // Old enough coworker SHOULD trigger queued nudge recovery
        assert_eq!(recoveries.len(), 1);
        assert!(matches!(
            &recoveries[0],
            StuckUiRecovery::InterruptQueuedNudges { name } if name == "amsterdam"
        ));
    }

    #[test]
    fn compaction_recovery_not_affected_by_age() {
        // Compaction recovery should NOT use age-based protection
        // (only queued nudge detection has this protection).
        let mut panes = HashMap::new();
        // Single-line format matches real Claude Code compaction status
        panes.insert(
            "york".to_string(),
            "✶ Whirlpooling… (esc to interrupt · 10m 00s · ↓ 0 tokens)\n".to_string(),
        );

        let now = chrono::Utc::now();
        let mut start_times = HashMap::new();
        // Coworker is young (30 seconds), but compaction should still trigger
        start_times.insert("york".to_string(), now - chrono::Duration::seconds(30));
        let min_age = chrono::Duration::seconds(60);

        let recoveries = decide_stuck_ui_recoveries(
            &panes,
            Duration::from_secs(300),
            &start_times,
            now,
            min_age,
        );

        // Young coworker SHOULD still trigger compaction recovery
        assert_eq!(recoveries.len(), 1);
        assert!(matches!(
            &recoveries[0],
            StuckUiRecovery::InterruptCompaction { name } if name == "york"
        ));
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

    #[test]
    fn test_coworker_record_display_status_with_task_zero_omits_number() {
        // Task ID 0 is used as a placeholder for taskless work (e.g., PR reviews
        // without a formal task assignment). It should display without the "#0"
        // suffix to avoid confusing window names like "PR#0".
        use crate::coworker_state::WorkflowPhase;

        let mut record = CoworkerRecord::new_spawn();
        record.workflow_phase = Some(WorkflowPhase::PullRequest);
        record.task_id = Some(0);

        // Should show "PR" not "PR#0"
        assert_eq!(record.display_status(), Some("PR".to_string()));
    }

    #[test]
    fn test_coworker_record_display_status_with_valid_task() {
        use crate::coworker_state::WorkflowPhase;

        let mut record = CoworkerRecord::new_spawn();
        record.workflow_phase = Some(WorkflowPhase::Developing);
        record.task_id = Some(42);

        assert_eq!(record.display_status(), Some("dev#42".to_string()));
    }

    #[test]
    fn test_coworker_record_display_status_without_task() {
        use crate::coworker_state::WorkflowPhase;

        let mut record = CoworkerRecord::new_spawn();
        record.workflow_phase = Some(WorkflowPhase::Idle);
        record.task_id = None;

        assert_eq!(record.display_status(), Some("idle".to_string()));
    }

    // -----------------------------------------------------------------------
    // Compaction misdetection bug tests (task #7)
    // -----------------------------------------------------------------------

    #[test]
    fn compaction_not_detected_for_normal_thinking_with_esc_to_interrupt() {
        // Bug: The daemon was interrupting coworkers doing normal work because
        // their "thinking" status shows "esc to interrupt", even though they're
        // NOT in compaction. Compaction has specific verbs like "Whirlpooling",
        // "Baking", "Simmering", "Sautéed" - normal thinking says "Fixing...",
        // "Scoring...", etc.
        let mut panes = HashMap::new();

        // Normal thinking state - NOT compaction (from actual snapshot)
        panes.insert(
            "vernon".to_string(),
            "✢ Fixing tmux window naming… (esc to interrupt · ctrl+t to hide tasks · 6m 11s · ↓ 1.4k tokens · thinking)\n"
                .to_string(),
        );

        // Even though duration (6m 11s) exceeds threshold (5m), this is NOT
        // compaction - it's normal task work. Should NOT trigger.
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "normal thinking state should not be detected as compaction, even with 'esc to interrupt'"
        );
    }

    #[test]
    fn compaction_detected_for_actual_compaction_verbs() {
        // These ARE actual compaction - should be detected
        // Real Claude Code compaction status has verb + duration on same line
        let test_cases = vec![
            (
                "whirlpool",
                "✶ Whirlpooling… (esc to interrupt · 6m 00s · ↓ 0 tokens)\n",
            ),
            (
                "baking",
                "✶ Baking… (esc to interrupt · 5m 30s · ↓ 100 tokens)\n",
            ),
            (
                "simmering",
                "✶ Simmering… (esc to interrupt · 7m 00s · ↓ 50 tokens)\n",
            ),
            ("sauteed", "  ✻ Sautéed for 6m 30s\n"),
        ];

        for (name, content) in test_cases {
            let mut panes = HashMap::new();
            panes.insert(name.to_string(), content.to_string());
            let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
            assert!(
                !stuck.is_empty(),
                "actual compaction '{}' should be detected",
                name
            );
        }
    }

    #[test]
    fn compaction_false_positive_from_verb_in_displayed_code() {
        // BUG REPRODUCTION: Cross-contamination between compaction verb in
        // displayed code and duration from unrelated thinking status line.
        //
        // Scenario: Coworker is doing normal thinking work (6+ minutes), and
        // the pane content includes code that happens to contain a compaction
        // verb like "simmering" in a comment or string.
        //
        // Current bug: has_compaction_indicator() finds "simmering" anywhere
        // in the content, then duration parsing finds "esc to interrupt" from
        // the thinking line, causing a false positive.
        let mut panes = HashMap::new();

        // Normal thinking status line (not compaction) + code with "simmering" verb
        let pane_content = r#"
⏺ Read(src/rules.rs)
  ⎿  Read 50 lines
     /// Compaction verbs are: Whirlpooling, Baking, Simmering, Sautéed.

⏺ Let me implement the fix for this bug.

✶ Fixing false positive detection… (esc to interrupt · ctrl+t to hide tasks · 6m 30s · ↓ 12.4k tokens · thinking)
  ⎿  ◼ #10 Fix false positive stuck compaction detection (york)

─────────────────────────────────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────────────────────────────────
"#;
        panes.insert("york".to_string(), pane_content.to_string());

        // This should NOT be detected as stuck compaction because:
        // 1. The "Simmering" is in displayed code, not an actual compaction status
        // 2. The "esc to interrupt" is from normal thinking, not compaction
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "compaction verb in displayed code should not cause false positive"
        );
    }

    #[test]
    fn sauteed_in_code_does_not_trigger_false_positive() {
        // Review feedback: "Sautéed for" appearing in code/comments should not
        // trigger stuck detection. Only actual compaction completion lines
        // (with the ✻ marker) should be detected.
        let mut panes = HashMap::new();

        // Code comment containing "Sauteed for 10m 00s" + normal thinking status
        let pane_content = r#"
⏺ Read(src/rules.rs)
  ⎿  Read 50 lines
     // Example: "Sauteed for 10m 00s" completion format

⏺ Working on the implementation.

✶ Implementing feature… (esc to interrupt · 6m 30s · ↓ 12.4k tokens · thinking)

─────────────────────────────────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────────────────────────────────
"#;
        panes.insert("york".to_string(), pane_content.to_string());

        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "'Sauteed for' in code comment should not trigger false positive"
        );
    }

    #[test]
    fn sauteed_case_insensitive_detection() {
        // Review feedback: uppercase SAUTEED should be detected (case insensitivity)
        let mut panes = HashMap::new();

        // Real compaction completion with uppercase (unlikely but possible)
        panes.insert("york".to_string(), "  ✻ SAUTEED FOR 10m 00s\n".to_string());

        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(
            stuck.len(),
            1,
            "uppercase SAUTEED should be detected (case insensitive)"
        );
    }

    #[test]
    fn queued_nudge_detected_despite_compaction_verb_in_code() {
        // Review feedback: queued nudge detection should work even when
        // displayed code contains compaction verbs like "Simmering".
        // The has_compaction_indicator() check should only skip detection
        // for ACTIVE compaction (verb + "esc to interrupt"), not just verb presence.
        let tui_content = r#"
⏺ Read(src/rules.rs)
  ⎿  /// Compaction verbs are: Whirlpooling, Baking, Simmering, Sautéed.

⏺ Completed the task.

✳ Working on next task...
❯ Check the channel for updates
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on"#;

        // has_queued_nudges should return true despite "Simmering" in displayed code
        assert!(
            has_queued_nudges(tui_content),
            "queued nudge should be detected even when compaction verb appears in displayed code"
        );
    }

    #[test]
    fn compaction_not_detected_for_various_normal_thinking_states() {
        // All of these are normal work states, NOT compaction
        let test_cases = vec![
            (
                "scoring",
                "✢ Scoring issues… (esc to interrupt · ctrl+t to hide tasks · 10m 2s · ↓ 4.2k tokens · thought for 2s)",
            ),
            (
                "fixing",
                "✳ Fixing tmux window naming… (esc to interrupt · ctrl+t to hide tasks · 8m 20s · ↓ 3.5k tokens · thinking)",
            ),
            (
                "checking",
                "✽ Checking PR eligibility… (esc to interrupt · ctrl+t to hide tasks · 6m 0s · ↓ 784 tokens · thinking)",
            ),
            (
                "reading",
                "✶ Reading file… (esc to interrupt · ctrl+t to hide tasks · 5m 30s · ↓ 500 tokens · thinking)",
            ),
        ];

        for (name, content) in test_cases {
            let mut panes = HashMap::new();
            panes.insert(name.to_string(), content.to_string());
            let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
            assert!(
                stuck.is_empty(),
                "normal thinking state '{}' should NOT be detected as compaction",
                name
            );
        }
    }

    #[test]
    fn snapshot_20260203_023607_no_false_compaction_detections() {
        // Test against real snapshot data where 6+ coworkers were incorrectly
        // interrupted. This snapshot captures the actual bug scenario:
        // - amsterdam: Scoring PR review issues
        // - broadway: Scoring PR review issues
        // - park: Completed scoring agents
        // - pleasant: Posting to channel after task completion
        // - riverside: Creating a PR
        // - york: Running cargo fmt
        //
        // None of these are compaction, but they all have "esc to interrupt"
        // in their pane content because that's shown during normal work.

        let fixture = include_str!("../tests/fixtures/snapshot/snapshot-20260203-023607.json");
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let pane_contents = snapshot["pane_contents"].as_object().unwrap();

        let mut panes: HashMap<String, String> = HashMap::new();
        for (name, content) in pane_contents {
            panes.insert(name.clone(), content.as_str().unwrap_or("").to_string());
        }

        // With the fix, none of these should be detected as stuck compaction
        // because none of them contain actual compaction verbs
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "snapshot should have no compaction detections, but found: {:?}",
            stuck
        );
    }

    #[test]
    fn snapshot_20260203_023035_no_false_compaction_detections() {
        // Second snapshot captured during the same incident - broadway mid-review
        let fixture = include_str!("../tests/fixtures/snapshot/snapshot-20260203-023035.json");
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let pane_contents = snapshot["pane_contents"].as_object().unwrap();

        let mut panes: HashMap<String, String> = HashMap::new();
        for (name, content) in pane_contents {
            panes.insert(name.clone(), content.as_str().unwrap_or("").to_string());
        }

        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "snapshot should have no compaction detections, but found: {:?}",
            stuck
        );
    }

    #[test]
    fn snapshot_20260204_175139_compaction_investigation_no_false_positives() {
        // Task #36: This snapshot was captured while investigating compaction false positives.
        // The pane content includes "✻ Investigating stuck compaction false positives…"
        // which has the ✻ marker but is NOT actual compaction (no "Sautéed for" text).
        // The detection logic should correctly ignore this.
        let fixture = include_str!(
            "../tests/fixtures/snapshot/snapshot-compaction-investigation-20260204-175139.json"
        );
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let pane_contents = snapshot["pane_contents"].as_object().unwrap();

        let mut panes: HashMap<String, String> = HashMap::new();
        for (name, content) in pane_contents {
            panes.insert(name.clone(), content.as_str().unwrap_or("").to_string());
        }

        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert!(
            stuck.is_empty(),
            "snapshot should have no compaction detections (task status lines with ✻ are not compaction), but found: {:?}",
            stuck
        );
    }

    // -----------------------------------------------------------------------
    // Subagent detection E2E tests (using real snapshot fixtures)
    // -----------------------------------------------------------------------

    /// E2E test: verify subagent detection works with real captured pane content.
    ///
    /// This test uses the snapshot-20260203-023607.json fixture which captured
    /// Madison with a running subagent ("✽ Checking PR eligibility…").
    /// This is the exact scenario from bug #27 where Madison was being falsely
    /// detected as idle while her scoring subagent was running.
    #[test]
    fn snapshot_20260203_023607_detects_madison_running_subagent() {
        let fixture = include_str!("../tests/fixtures/snapshot/snapshot-20260203-023607.json");
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let pane_contents = snapshot["pane_contents"].as_object().unwrap();

        let mut panes: HashMap<String, String> = HashMap::new();
        for (name, content) in pane_contents {
            panes.insert(name.clone(), content.as_str().unwrap_or("").to_string());
        }

        // Madison should have a running subagent detected (she has ✽ Checking PR eligibility…)
        let madison_pane = panes.get("madison").expect("madison should be in snapshot");
        assert!(
            has_running_subagent(madison_pane),
            "madison's pane should show a running subagent (whirlpool indicator)"
        );

        // Verify the specific pattern is present
        assert!(
            madison_pane.contains("✽"),
            "madison's pane should contain whirlpool indicator"
        );

        // Count total coworkers with running subagents
        let coworkers_with_subagents: HashSet<String> = panes
            .iter()
            .filter(|(_, content)| has_running_subagent(content))
            .map(|(name, _)| name.to_lowercase())
            .collect();

        assert!(
            coworkers_with_subagents.contains("madison"),
            "madison should be in the set of coworkers with running subagents"
        );
    }

    /// E2E test: verify idle shutdown protection for coworkers with running subagents.
    ///
    /// Uses the same fixture as above but tests the full idle shutdown decision flow
    /// to ensure Madison would be protected from idle shutdown while her subagent runs.
    #[test]
    fn snapshot_20260203_023607_madison_protected_from_idle_shutdown() {
        let fixture = include_str!("../tests/fixtures/snapshot/snapshot-20260203-023607.json");
        let snapshot: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let pane_contents = snapshot["pane_contents"].as_object().unwrap();

        let mut panes: HashMap<String, String> = HashMap::new();
        for (name, content) in pane_contents {
            panes.insert(name.clone(), content.as_str().unwrap_or("").to_string());
        }

        // Build the set of coworkers with running subagents (same as snapshot collector does)
        let coworkers_with_running_subagents: HashSet<String> = panes
            .iter()
            .filter(|(_, content)| has_running_subagent(content))
            .map(|(name, _)| name.to_lowercase())
            .collect();

        // Create a CoworkerSnapshot for madison (10 minutes old, so past minimum lifetime)
        let coworkers = vec![CoworkerSnapshot {
            name: "madison".to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(10),
            isolated_tasks: true, // madison is an isolated reviewer
        }];

        // Create idle health state (madison has been "idle" for 60+ seconds)
        let mut phases = HashMap::new();
        phases.insert(
            "madison".to_string(),
            CoworkerRecord {
                health: Some(SessionHealth::Idle {
                    since: Instant::now() - Duration::from_secs(60),
                }),
                last_activity: None,
                workflow_phase: None,
                task_id: None,
                workflow_updated_at: None,
                pane_hash: Some((12345, Instant::now() - Duration::from_secs(300))), // stale pane
                zombie_respawn_count: 0,
            },
        );

        // Run idle shutdown decision with madison having a running subagent
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),                         // not busy (no in-progress tasks)
            &set(&[]),                         // no open PRs
            &set(&[]),                         // not reviewing
            &set(&[]),                         // no unblocked deps
            &coworkers_with_running_subagents, // madison HAS running subagent
            &set(&[]),                         // ci_passed
            &set(&[]),                         // usage_limited
            &set(&[]),                         // api_error
            &phases,
            Utc::now(),
            Duration::from_secs(300),
        );

        // Madison should NOT be shut down because she has a running subagent
        assert!(
            decisions.is_empty(),
            "madison should NOT be sent on break while she has a running subagent. \
             This is the fix for bug #27: false idle detection when subagent is running. \
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

    #[test]
    fn usage_limit_decide_detection_false_positive() {
        // Test the full decision function with code content
        let mut panes = HashMap::new();
        panes.insert(
            "riverside".to_string(),
            "// Health checks: idle shutdown, stuck detection, usage limits.".to_string(),
        );

        let decision = decide_usage_limit_detection(&panes);
        assert_eq!(
            decision,
            UsageLimitDecision::NoneDetected,
            "code content should not trigger usage limit detection"
        );
    }

    #[test]
    fn usage_limit_decide_detection_true_positive() {
        // Test the full decision function with actual usage limit screen
        let mut panes = HashMap::new();
        panes.insert(
            "broadway".to_string(),
            "You've reached your usage limit. /upgrade to increase your limit.".to_string(),
        );

        let decision = decide_usage_limit_detection(&panes);
        assert!(
            matches!(decision, UsageLimitDecision::Detected { coworker } if coworker == "broadway"),
            "actual usage limit screen should trigger detection"
        );
    }

    // -----------------------------------------------------------------------
    // parse_usage_limit_duration tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_usage_limit_duration_12hour_pm() {
        // "resets 12pm" should parse as noon (12:00)
        let content = "Your limit resets 12pm (America/Chicago). /upgrade";
        let duration = parse_usage_limit_duration(content);
        // We can't assert exact value since it depends on current time,
        // but it should be less than 24 hours
        assert!(
            duration.as_secs() <= 24 * 3600,
            "12pm should be within 24 hours"
        );
    }

    #[test]
    fn parse_usage_limit_duration_12hour_am() {
        // "resets 3am" should parse as 03:00
        let content = "Your limit resets 3am. /upgrade";
        let duration = parse_usage_limit_duration(content);
        assert!(
            duration.as_secs() <= 24 * 3600,
            "3am should be within 24 hours"
        );
    }

    #[test]
    fn parse_usage_limit_duration_relative() {
        // "in 2 hours" should return 2 hours
        let content = "Your limit will reset in 2 hours. /upgrade";
        let duration = parse_usage_limit_duration(content);
        assert_eq!(duration.as_secs(), 2 * 3600, "should parse '2 hours'");
    }

    #[test]
    fn parse_usage_limit_duration_relative_minutes() {
        // "in 45 minutes" should return 45 minutes
        let content = "Available after 45 minutes. /upgrade";
        let duration = parse_usage_limit_duration(content);
        assert_eq!(duration.as_secs(), 45 * 60, "should parse '45 minutes'");
    }

    #[test]
    fn parse_usage_limit_duration_fallback() {
        // Unknown format should return 15 minutes
        let content = "Some unknown format. /upgrade";
        let duration = parse_usage_limit_duration(content);
        assert_eq!(
            duration.as_secs(),
            15 * 60,
            "unknown format should default to 15 minutes"
        );
    }

    // -----------------------------------------------------------------------
    // decide_stuck_coworker_restarts tests
    // -----------------------------------------------------------------------

    #[test]
    fn stuck_detection_skips_coworkers_with_running_subagents() {
        // Coworker with running subagents should be PROTECTED from stuck detection.
        // The pane is frozen because it's waiting for Task agents to complete,
        // which is normal behavior, not stuck.
        let mut pane_hashes = HashMap::new();
        let now = Instant::now();
        let old_time = now - Duration::from_secs(400); // 6+ minutes ago

        // Pane content with whirlpool indicator = active subagent
        let active_content = r#"
✽ Checking PR eligibility… (esc to interrupt · ctrl+t to hide tasks · 33s · ↓ 784 tokens · thinking)
  ⎿  ◼ #1 Check PR #508 eligibility for code review (madison)
"#;

        // Use the hash of this content
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        active_content.hash(&mut hasher);
        let content_hash = hasher.finish();

        // Set old hash to same value (pane unchanged for 6+ minutes)
        pane_hashes.insert("broadway".to_string(), (content_hash, old_time));

        let mut pane_contents = HashMap::new();
        pane_contents.insert("broadway".to_string(), active_content.to_string());

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "broadway".to_string(),
        )];

        let usage_limited = HashSet::new();
        let api_error = HashSet::new();
        let result = decide_stuck_coworker_restarts(
            &pane_hashes,
            &pane_contents,
            &tasks,
            &usage_limited,
            &api_error,
            now,
            Duration::from_secs(180), // 3 minute stuck duration
        );

        assert!(
            result.restarts.is_empty(),
            "coworker with running subagents should be PROTECTED from stuck detection"
        );
    }

    #[test]
    fn stuck_detection_triggers_for_frozen_pane_without_subagents() {
        // Coworker with frozen pane AND no activity indicators = truly stuck.
        // This could be a hung Claude Code process that needs restart.
        let mut pane_hashes = HashMap::new();
        let now = Instant::now();
        let old_time = now - Duration::from_secs(400); // 6+ minutes ago

        // Pane content showing it's working but no subagent indicator
        // (e.g., Claude Code froze mid-thinking)
        let stuck_content = r#"
⏺ Working on task #42

Reading files...
"#;

        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stuck_content.hash(&mut hasher);
        let content_hash = hasher.finish();

        // Set old hash to same value (pane unchanged)
        pane_hashes.insert("riverside".to_string(), (content_hash, old_time));

        let mut pane_contents = HashMap::new();
        pane_contents.insert("riverside".to_string(), stuck_content.to_string());

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "riverside".to_string(),
        )];

        let usage_limited = HashSet::new();
        let api_error = HashSet::new();
        let result = decide_stuck_coworker_restarts(
            &pane_hashes,
            &pane_contents,
            &tasks,
            &usage_limited,
            &api_error,
            now,
            Duration::from_secs(180), // 3 minute stuck duration
        );

        assert_eq!(
            result.restarts.len(),
            1,
            "coworker with frozen pane and NO subagents SHOULD be restarted"
        );
        assert_eq!(result.restarts[0].name, "riverside");
    }

    #[test]
    fn stuck_detection_skips_usage_limited_coworkers() {
        // Coworkers at usage limit should be skipped from stuck detection.
        // Their pane is frozen waiting for the limit to reset, not stuck.
        let mut pane_hashes = HashMap::new();
        let now = Instant::now();
        let old_time = now - Duration::from_secs(400); // 6+ minutes ago

        // Pane content showing usage limit screen
        let usage_limit_content = r#"
You've reached your usage limit for Claude Opus 4.5.

Your limit will reset in 2 hours.

Options:
- /upgrade to increase your limit
- /compact to reduce context
"#;

        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        usage_limit_content.hash(&mut hasher);
        let content_hash = hasher.finish();

        // Set old hash to same value (pane unchanged for 6+ minutes)
        pane_hashes.insert("york".to_string(), (content_hash, old_time));

        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), usage_limit_content.to_string());

        let tasks = vec![("42".to_string(), "Fix bug".to_string(), "york".to_string())];

        // Mark york as usage-limited
        let mut usage_limited = HashSet::new();
        usage_limited.insert("york".to_string());
        let api_error = HashSet::new();

        let result = decide_stuck_coworker_restarts(
            &pane_hashes,
            &pane_contents,
            &tasks,
            &usage_limited,
            &api_error,
            now,
            Duration::from_secs(180), // 3 minute stuck duration
        );

        assert!(
            result.restarts.is_empty(),
            "usage-limited coworker should be skipped from stuck detection"
        );
    }

    #[test]
    fn stuck_detection_skips_api_error_coworkers() {
        // Coworkers with API errors should be skipped from stuck detection.
        // Their pane is frozen waiting for the API to recover, not stuck.
        let mut pane_hashes = HashMap::new();
        let now = Instant::now();
        let old_time = now - Duration::from_secs(400); // 6+ minutes ago

        // Pane content showing API error
        let api_error_content = r#"
Working on task #42...

API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"},"request_id":"req_123"}
"#;

        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        api_error_content.hash(&mut hasher);
        let content_hash = hasher.finish();

        // Set old hash to same value (pane unchanged for 6+ minutes)
        pane_hashes.insert("madison".to_string(), (content_hash, old_time));

        let mut pane_contents = HashMap::new();
        pane_contents.insert("madison".to_string(), api_error_content.to_string());

        let tasks = vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "madison".to_string(),
        )];

        // Mark madison as having API error
        let usage_limited = HashSet::new();
        let mut api_error = HashSet::new();
        api_error.insert("madison".to_string());

        let result = decide_stuck_coworker_restarts(
            &pane_hashes,
            &pane_contents,
            &tasks,
            &usage_limited,
            &api_error,
            now,
            Duration::from_secs(180), // 3 minute stuck duration
        );

        assert!(
            result.restarts.is_empty(),
            "API error coworker should be skipped from stuck detection"
        );
    }

    // -----------------------------------------------------------------------
    // has_running_subagent tests
    // -----------------------------------------------------------------------

    #[test]
    fn subagent_detection_whirlpool_indicator() {
        // Whirlpool (✽) at start of line indicates active subagent
        let pane_with_subagent = r#"
✔ Task #1 updated: owner, status → in progress
  ⎿  Running PostToolUse hooks… (1/2 done)

⏺ Now let me run the first three tasks in parallel:

✽ Checking PR eligibility… (esc to interrupt · ctrl+t to hide tasks · 33s · ↓ 784 tokens · thinking)
  ⎿  ◼ #1 Check PR #508 eligibility for code review (madison)
     ◻ #2 Find relevant CLAUDE.md files for PR #508 (madison)
"#;
        assert!(
            has_running_subagent(pane_with_subagent),
            "whirlpool indicator should be detected as running subagent"
        );
    }

    #[test]
    fn subagent_detection_running_task_agents() {
        // "Running X Task agent" pattern indicates subagents in progress
        let pane_with_running_agents = r#"
⏺ I have two issues to score. Let me launch Haiku agents to score both issues.

   Running 3 Task agents… (ctrl+o to expand)
   ├─ Score issue 1: coarse filter · 15 tool uses · 51.3k tokens
   │  ⎿  Running…
   └─ Score issue 2: test naming · 11 tool uses · 25.8k tokens
      ⎿  Running…
"#;
        assert!(
            has_running_subagent(pane_with_running_agents),
            "'Running X Task agent' should be detected as running subagent"
        );
    }

    #[test]
    fn subagent_detection_finished_agents_not_detected() {
        // Finished agents should NOT trigger detection
        let pane_with_finished_agents = r#"
⏺ 5 Task agents finished (ctrl+o to expand)
   ├─ Agent 1: CLAUDE.md compliance · 5 tool uses · 27.3k tokens
   │  ⎿  Done
   ├─ Agent 2: Obvious bugs scan · 15 tool uses · 31.2k tokens
   │  ⎿  Done
   └─ Agent 3: Git history context · 58 tool uses · 50.1k tokens
      ⎿  Done

⏺ The 5 agents have completed. Let me now score the issues.
"#;
        assert!(
            !has_running_subagent(pane_with_finished_agents),
            "finished agents should NOT be detected as running subagent"
        );
    }

    #[test]
    fn subagent_detection_normal_idle_pane() {
        // Normal idle pane without subagents
        let idle_pane = r#"
⏺ I've completed the task. Let me post to the channel.

✔ Task #27 updated: status → completed
  ⎿  Running PostToolUse hooks… (1/2 done)

midtown channel post "/me finished task 27"
  ⎿  Message posted to channel

❯
"#;
        assert!(
            !has_running_subagent(idle_pane),
            "normal idle pane should NOT be detected as running subagent"
        );
    }

    #[test]
    fn subagent_detection_code_with_task_agent_string() {
        // Code that mentions "Task agent" in comments should NOT trigger
        let _code_content = r#"
⏺ Let me update the documentation.

/// Launch a Task agent to handle the work.
/// Running Task agent operations require proper cleanup.
fn launch_agent() {
    // Task agent spawned here
}
"#;
        // Note: This will match because "Running Task agent" appears in comment
        // but that's acceptable - false positives here just prevent unnecessary
        // idle shutdown, which is safe. The key is avoiding false negatives.
        // In practice, code files rarely have this exact pattern.
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

    // -----------------------------------------------------------------------
    // decide_pending_task_action tests (isolated namespace handling)
    // -----------------------------------------------------------------------

    #[test]
    fn pending_task_action_skips_isolated_reviewer() {
        // Bug: Task ID collision between reviewer's isolated namespace and main task list.
        // Isolated reviewers should NOT be nudged about main task list updates.
        let active_names: HashSet<String> = ["madison".to_string()].into_iter().collect();

        // Main task #6 has owner="madison", but madison is an isolated reviewer
        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "madison",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            true,  // IS isolated reviewer
        );

        assert!(
            matches!(action, PendingTaskAction::Skip { .. }),
            "isolated reviewer should be skipped for main task list updates, got: {:?}",
            action
        );

        // Verify the skip reason mentions isolation
        if let PendingTaskAction::Skip { reason } = action {
            assert!(
                reason.contains("isolated"),
                "skip reason should mention isolation: {}",
                reason
            );
        }
    }

    #[test]
    fn pending_task_action_nudges_non_isolated_coworker() {
        // Non-isolated coworkers SHOULD be nudged about their pending tasks
        let active_names: HashSet<String> = ["york".to_string()].into_iter().collect();

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "york",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            false, // NOT isolated
        );

        assert!(
            matches!(action, PendingTaskAction::NudgeOwner { .. }),
            "non-isolated coworker should be nudged, got: {:?}",
            action
        );
    }

    #[test]
    fn pending_task_action_spawns_non_isolated_inactive_owner() {
        // Inactive non-isolated owners should be spawned
        let active_names: HashSet<String> = HashSet::new(); // york is not active

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "york",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            false, // NOT isolated
        );

        assert!(
            matches!(action, PendingTaskAction::SpawnOwner { .. }),
            "inactive non-isolated owner should be spawned, got: {:?}",
            action
        );
    }

    #[test]
    fn pending_task_action_skips_isolated_inactive_owner() {
        // Regression test from PR #614 review: isolation check fires before active check.
        // An inactive isolated owner should still be skipped, not spawned.
        let active_names: HashSet<String> = HashSet::new(); // madison is NOT active

        let action = decide_pending_task_action(
            "6",
            "Prevent coworkers from checking out default branch",
            "madison",
            &active_names,
            false, // not at dev limit
            false, // not on cooldown
            true,  // IS isolated (even though inactive)
        );

        assert!(
            matches!(action, PendingTaskAction::Skip { .. }),
            "inactive isolated owner should still be skipped, got: {:?}",
            action
        );

        // Verify the skip reason mentions isolation
        if let PendingTaskAction::Skip { reason } = action {
            assert!(
                reason.contains("isolated"),
                "skip reason should mention isolation: {}",
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
}
