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
}

// ---------------------------------------------------------------------------
// SessionHealth — the per-coworker state machine
// ---------------------------------------------------------------------------

/// The current health state of a coworker's session.
///
/// A coworker can only be in one phase at a time — the enum enforces
/// mutual exclusivity. Pane scraping is used only for health checks
/// (stuck detection, zombie detection, usage limits), not workflow state.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionHealth {
    /// Coworker has no tasks and is waiting for the idle timeout to expire.
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
/// - They have open unmerged PRs
/// - They are actively reviewing a PR
/// - They have unblocked dependent tasks
/// - Their pane content changed recently (within `pane_activity_grace`)
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_idle_shutdowns(
    coworkers: &[CoworkerSnapshot],
    busy_coworkers: &HashSet<String>,
    coworkers_with_open_prs: &HashSet<String>,
    active_reviewers: &HashSet<String>,
    coworkers_with_unblocked_deps: &HashSet<String>,
    _ci_passed_pr_coworkers: &HashSet<String>,
    records: &HashMap<String, CoworkerRecord>,
    now: Instant,
    now_utc: DateTime<Utc>,
    idle_break_duration: Duration,
    minimum_lifetime: Duration,
    pane_activity_grace: Duration,
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

        // Check pane activity: if the pane content changed recently, the coworker
        // is actively working and must not be sent on break.
        let pane_recently_active = records
            .get(coworker)
            .and_then(|r| r.pane_hash)
            .map(|(_, last_changed)| now.duration_since(last_changed) < pane_activity_grace)
            .unwrap_or(false);

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

        // Coworkers with open PRs, active tasks, review assignments,
        // unblocked deps, or recent pane activity are never sent on break.
        if is_busy || has_open_pr || is_reviewing || has_unblocked_deps || pane_recently_active {
            if matches!(
                get_health(records, coworker),
                Some(SessionHealth::Idle { .. })
            ) {
                transitions.push(HealthTransition::Clear {
                    name: coworker.clone(),
                });
            }
        } else if cw.isolated_tasks {
            // Isolated coworkers (reviewers) go on break immediately when idle
            to_shutdown.push(ShutdownDecision {
                name: coworker.clone(),
                is_isolated: true,
            });
        } else {
            match get_health(records, coworker) {
                Some(SessionHealth::Idle { since }) => {
                    if now.duration_since(since) >= idle_break_duration {
                        to_shutdown.push(ShutdownDecision {
                            name: coworker.clone(),
                            is_isolated: false,
                        });
                    }
                }
                None => {
                    transitions.push(HealthTransition::Set {
                        name: coworker.clone(),
                        phase: SessionHealth::Idle { since: now },
                    });
                }
            }
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

/// Pattern that indicates a coworker has hit a usage/rate limit.
///
/// When Claude Code hits a usage limit, it displays a message with "/upgrade"
/// as an action option. This is specific to the actual usage limit screen and
/// won't match code content that happens to mention "usage limit" or "rate limit"
/// in comments or variable names.
///
/// Previous patterns like "usage limit" caused false positives when coworkers
/// were editing code with those strings in comments.
const USAGE_LIMIT_PATTERN: &str = "/upgrade";

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
        if content.contains(USAGE_LIMIT_PATTERN) {
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

/// Detect coworkers whose pane content hasn't changed for `stuck_duration`.
///
/// Pure function: takes the current pane hash state and pane contents,
/// returns restart decisions and the updated hash state. The caller is
/// responsible for applying the hash updates to persistent state.
pub(crate) fn decide_stuck_coworker_restarts(
    pane_hashes: &HashMap<String, (u64, Instant)>,
    pane_contents: &HashMap<String, String>,
    in_progress_tasks: &[(String, String, String)],
    now: Instant,
    stuck_duration: Duration,
) -> StuckDetectionResult {
    use std::hash::{Hash, Hasher};

    let mut restarts = Vec::new();
    let mut updated_hashes = pane_hashes.clone();

    for (name, content) in pane_contents {
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
            // First check: must have actual compaction indicators in the pane
            if !has_compaction_indicator(content) {
                return false;
            }

            // Second check: find the compaction status line and parse elapsed time
            content.lines().any(|line| {
                // For "Sautéed for Xm Ys" format (completed compaction)
                if line.contains("Sautéed for") {
                    return parse_sauteed_duration(line)
                        .map(|d| d >= min_duration)
                        .unwrap_or(false);
                }

                // For active compaction with "esc to interrupt"
                if !line.contains("esc to interrupt") {
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

/// Check if the pane content contains actual compaction indicators.
///
/// Compaction verbs are: Whirlpooling, Baking, Simmering, Sautéed.
/// These are distinct from normal "thinking" states like "Fixing...",
/// "Scoring...", "Checking...", etc.
fn has_compaction_indicator(content: &str) -> bool {
    // Case-insensitive check for compaction verbs
    let content_lower = content.to_lowercase();
    content_lower.contains("whirlpooling")
        || content_lower.contains("baking")
        || content_lower.contains("simmering")
        || content_lower.contains("sautéed")
        || content_lower.contains("sauteed") // ASCII fallback
}

/// Parse duration from "Sautéed for Xm Ys" format.
fn parse_sauteed_duration(line: &str) -> Option<Duration> {
    let after_for = line.split("for").nth(1)?;

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
fn parse_compaction_duration(line: &str) -> Option<Duration> {
    // Look for the pattern "· Xm Ys ·" after "esc to interrupt"
    let after_esc = line.split("esc to interrupt").nth(1)?;

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

/// Check if pane content contains the usage limit pattern ("/upgrade").
///
/// Used directly in tests and indirectly via `decide_usage_limit_detection`.
#[allow(dead_code)]
pub(crate) fn has_usage_limit_pattern(pane_content: &str) -> bool {
    pane_content.contains(USAGE_LIMIT_PATTERN)
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
        assert!(decisions[0].is_isolated);
    }

    #[test]
    fn idle_shutdown_starts_tracking_newly_idle() {
        let coworkers = vec![cw("york", 10)];
        let mut phases: HashMap<String, CoworkerRecord> = HashMap::new();

        let (decisions, transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );
        apply_health_transitions(&mut phases, transitions);

        // No shutdown yet — just started tracking
        assert!(decisions.is_empty());
        assert!(get_health(&phases, "york").is_some());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr_even_ci_passed() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR AND CI is passing — should still be protected (never break with open PR)
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );

        assert!(
            decisions.is_empty(),
            "coworkers with open PRs should never be sent on break"
        );
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
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_recent_pane_activity() {
        let coworkers = vec![cw("york", 10)];
        // york has a pane_hash that changed recently (10 seconds ago)
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
                pane_hash: Some((12345, Instant::now() - Duration::from_secs(10))),
                zombie_respawn_count: 0,
            },
        );

        // york is idle and has no tasks/PRs, but pane changed 10s ago — should NOT break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );

        assert!(
            decisions.is_empty(),
            "coworkers with recent pane activity should not be sent on break"
        );
    }

    #[test]
    fn idle_shutdown_allows_break_with_stale_pane() {
        let coworkers = vec![cw("york", 10)];
        // york has a pane_hash that last changed 5 minutes ago (well beyond grace period)
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

        // york is idle, no tasks/PRs, pane unchanged for 5 minutes — should break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(120),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
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

    // -----------------------------------------------------------------------
    // Compaction whirlpool & queued prompt detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn compaction_detected_with_whirlpool_verb_long_duration() {
        let mut panes = HashMap::new();
        panes.insert(
            "york".to_string(),
            "  Whirlpooling your conversation…\n  (esc to interrupt · 18m 50s · ↓ 0 tokens)\n"
                .to_string(),
        );
        // 18m 50s > 5 min threshold — should trigger
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(stuck, vec!["york"]);
    }

    #[test]
    fn compaction_not_detected_with_short_duration() {
        let mut panes = HashMap::new();
        panes.insert(
            "amsterdam".to_string(),
            "  Baking your conversation…\n  (esc to interrupt · 3m 12s · ↓ 42 tokens)\n"
                .to_string(),
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
        panes.insert(
            "park".to_string(),
            "  Simmering your conversation…\n  (esc to interrupt · 5m 00s · ↓ 100 tokens)\n"
                .to_string(),
        );
        // 5m 00s = 5 min threshold — should trigger
        let stuck = detect_compaction_stuck(&panes, Duration::from_secs(300));
        assert_eq!(stuck, vec!["park"]);
    }

    #[test]
    fn compaction_not_detected_just_under_threshold() {
        let mut panes = HashMap::new();
        panes.insert(
            "park".to_string(),
            "  Simmering your conversation…\n  (esc to interrupt · 4m 59s · ↓ 100 tokens)\n"
                .to_string(),
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
        // Must include actual compaction verb (Whirlpooling) to be detected
        panes.insert(
            "york".to_string(),
            "  Whirlpooling your conversation…\n  (esc to interrupt · 10m 00s · ↓ 0 tokens)\n"
                .to_string(),
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
        panes.insert(
            "york".to_string(),
            "  Whirlpooling your conversation…\n  (esc to interrupt · 10m 00s · ↓ 0 tokens)\n"
                .to_string(),
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
        let test_cases = vec![
            (
                "whirlpool",
                "  Whirlpooling your conversation…\n  (esc to interrupt · 6m 00s · ↓ 0 tokens)\n",
            ),
            (
                "baking",
                "  Baking your conversation…\n  (esc to interrupt · 5m 30s · ↓ 100 tokens)\n",
            ),
            (
                "simmering",
                "  Simmering your conversation…\n  (esc to interrupt · 7m 00s · ↓ 50 tokens)\n",
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
}
