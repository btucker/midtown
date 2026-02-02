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
    pub fn display_status(&self) -> Option<String> {
        self.workflow_phase.map(|phase| match self.task_id {
            Some(id) => format!("{}#{}", phase.abbreviation(), id),
            None => phase.abbreviation().to_string(),
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
    /// When true, the daemon should save the coworker's session ID before shutdown
    /// so it can be resumed later (e.g., PR break-and-resume when CI passes).
    pub save_session: bool,
}

// ---------------------------------------------------------------------------
// Lifecycle decision functions (pure — no async, no side effects)
// ---------------------------------------------------------------------------

/// Decide which coworkers should be shut down due to idleness.
///
/// Takes pre-collected state snapshots and immutable coworker records.
/// Returns shutdown decisions and health state transitions without performing
/// any side effects or mutations.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_idle_shutdowns(
    coworkers: &[CoworkerSnapshot],
    busy_coworkers: &HashSet<String>,
    coworkers_with_open_prs: &HashSet<String>,
    active_reviewers: &HashSet<String>,
    coworkers_with_unblocked_deps: &HashSet<String>,
    ci_passed_pr_coworkers: &HashSet<String>,
    records: &HashMap<String, CoworkerRecord>,
    now: Instant,
    now_utc: DateTime<Utc>,
    idle_break_duration: Duration,
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
                save_session: false,
            });
        } else {
            match get_health(records, coworker) {
                Some(SessionHealth::Idle { since }) => {
                    if now.duration_since(since) >= idle_break_duration {
                        to_shutdown.push(ShutdownDecision {
                            name: coworker.clone(),
                            is_isolated: false,
                            save_session: false,
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
        );
        apply_health_transitions(&mut phases, transitions);

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(!decisions[0].is_isolated);
        assert!(!decisions[0].save_session);
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
        );
        apply_health_transitions(&mut phases, transitions);

        // No shutdown yet — just started tracking
        assert!(decisions.is_empty());
        assert!(get_health(&phases, "york").is_some());
    }

    #[test]
    fn idle_shutdown_pr_break_with_ci_passed() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has an open PR AND CI is passing — should trigger PR break (save_session=true)
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
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(decisions[0].save_session);
    }

    #[test]
    fn idle_shutdown_pr_break_not_triggered_without_ci() {
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
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_pr_break_not_triggered_if_busy() {
        let coworkers = vec![cw("york", 10)];
        let phases = lifecycle_with(
            "york",
            SessionHealth::Idle {
                since: Instant::now() - Duration::from_secs(60),
            },
        );

        // york has open PR, CI passed, but is busy — should NOT trigger break
        let (decisions, _transitions) = decide_idle_shutdowns(
            &coworkers,
            &set(&["york"]),
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &phases,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
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
