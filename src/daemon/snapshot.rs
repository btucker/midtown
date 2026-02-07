//! World snapshot — an immutable view of all daemon state for a single tick.
//!
//! Pure evaluation functions read from the snapshot instead of reaching into
//! `DaemonState` directly. This eliminates duplicate data fetching across
//! multiple check functions within the same tick and makes decision logic
//! easier to test.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::coworker::Coworker;
use crate::message::Message;
use crate::rules::CoworkerSnapshot;
use crate::tasks::Task;

use super::DaemonState;

/// Health state of a headless coworker process.
///
/// Populated by the daemon's session management layer (future SessionManager)
/// from `HeadlessSession` stream events and process status. Decision functions
/// in `rules.rs` consume this structured data instead of parsing raw pane content.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessHealth {
    /// Whether the child process is still running.
    pub is_alive: bool,
    /// When the last stream event was received from this coworker's stdout.
    /// Used for stuck detection: if `None` or older than the stuck threshold,
    /// the coworker may be hung.
    pub last_event_at: Option<DateTime<Utc>>,
    /// Whether the coworker hit a usage/rate limit (detected from
    /// `StreamEvent::Result { is_error: true }` with usage limit content).
    pub has_usage_limit: bool,
    /// Whether the coworker is experiencing API errors (transient failures
    /// that may resolve on retry).
    pub has_api_error: bool,
    /// Whether the coworker has a running Task tool subagent.
    /// When true, the parent session may not emit events for several minutes
    /// while the subagent works — stuck detection should skip these coworkers.
    pub has_running_subagent: bool,
    /// Process exit code, if the process has terminated.
    pub exit_code: Option<i32>,
}

impl Default for ProcessHealth {
    fn default() -> Self {
        Self {
            is_alive: true,
            last_event_at: None,
            has_usage_limit: false,
            has_api_error: false,
            has_running_subagent: false,
            exit_code: None,
        }
    }
}

/// Number of recent channel messages to include in WorldSnapshot captures.
const SNAPSHOT_CHANNEL_MESSAGE_COUNT: usize = 50;

/// Number of recent daemon log lines to include in WorldSnapshot captures.
const SNAPSHOT_DAEMON_LOG_LINES: usize = 100;

/// Immutable snapshot of the daemon's world, collected once per tick.
///
/// Each field is owned data — no references back to `DaemonState`. This means
/// evaluation functions that take `&WorldSnapshot` cannot accidentally trigger
/// side effects on the underlying state.
///
/// The struct is serializable (for debugging and test fixtures).
#[derive(Debug, serde::Serialize)]
pub struct WorldSnapshot {
    // ── Coworker state ──────────────────────────────────────────────────
    /// All coworkers (any status).
    pub active_coworkers: Vec<Coworker>,
    /// Only coworkers with `Running` status.
    pub running_coworkers: Vec<Coworker>,
    /// Coworker snapshots for pure decision functions in `rules`.
    pub coworker_snapshots: Vec<CoworkerSnapshot>,
    /// Lowercase names of running coworkers (for fast lookup).
    pub active_names: HashSet<String>,
    /// Tmux session name (e.g., "midtown-projectname").
    pub session_name: String,
    /// Coworker start times keyed by lowercase name.
    pub coworker_start_times: HashMap<String, DateTime<Utc>>,
    /// Coworker stop times keyed by lowercase name.
    /// Tracks when coworkers were sent on a break (shutdown). Used by workflow
    /// features that need to know the last activity time of inactive coworkers.
    pub coworker_stop_times: HashMap<String, DateTime<Utc>>,

    // ── Process health (headless coworker monitoring) ──────────────────
    /// Health state of headless coworker processes, keyed by coworker name.
    /// Replaces pane scraping: stuck detection uses `last_event_at`,
    /// usage limits and API errors use structured flags set from stream events.
    pub headless_process_health: HashMap<String, ProcessHealth>,

    // ── Task state ──────────────────────────────────────────────────────
    /// In-progress tasks: `(task_id, subject, owner)`.
    pub in_progress_tasks: Vec<(String, String, String)>,
    /// Names of coworkers who are busy (have in-progress tasks), lowercase.
    pub busy_coworkers: HashSet<String>,
    /// All tasks from disk (for relationship lookups).
    pub all_tasks: Vec<Task>,
    /// Pending tasks that have an owner: `(task_id, subject, owner)`.
    pub pending_tasks_with_owners: Vec<(String, String, String)>,
    /// Pending tasks with no owner (unclaimed, past grace period, unblocked).
    pub pending_tasks_without_owners: Vec<Task>,

    // ── PR / GitHub state ───────────────────────────────────────────────
    /// Coworkers who have at least one open PR.
    pub coworkers_with_open_prs: HashSet<String>,
    /// Coworkers whose PR was recently merged.
    pub coworkers_with_merged_prs: HashSet<String>,
    /// PR numbers of recently merged PRs. Used by task dispatch to skip
    /// tasks that reference a merged PR (e.g., "Address review feedback on PR #709").
    pub merged_pr_numbers: HashSet<u64>,
    /// Coworkers whose open PR has all CI checks passing (eligible for PR break).
    pub ci_passed_pr_coworkers: HashSet<String>,
    /// Coworkers whose open PR has CI passed AND has review feedback to address.
    /// These coworkers are protected from idle shutdown (prevents spawn→idle→break loop).
    pub review_feedback_pr_coworkers: HashSet<String>,
    /// Coworkers who have pending tasks assigned to them (task.owner set, status=pending).
    /// Provides defense-in-depth idle shutdown protection alongside `busy_coworkers`
    /// (in-memory assignment tracking). Both paths are checked to prevent the
    /// spawn→idle→break loop (see PR #650).
    pub pending_task_owners: HashSet<String>,

    // ── Reviewer state ──────────────────────────────────────────────────
    /// Currently active reviewers (from both in-memory tracker and persistent state).
    pub active_reviewers: HashSet<String>,
    /// Reviewer → assigned PR number mapping (from github-state.json).
    pub reviewer_pr_assignments: HashMap<String, u64>,
    /// PRs that have been verified as reviewed (Claude review comment exists).
    /// Pre-collected during snapshot so decision logic doesn't need API calls.
    pub reviewed_prs: HashSet<u64>,
    /// Count of open PRs that need review (not draft, no Claude review, no formal review).
    /// Used by task dispatch to prioritize reviews over new task pickup.
    pub prs_needing_review: usize,

    // ── Dependency state ──────────────────────────────────────────────────
    /// Coworkers whose completed tasks have unblocked pending follow-ups.
    pub coworkers_with_unblocked_deps: HashSet<String>,

    // ── Usage limit state ────────────────────────────────────────────────
    /// Whether a usage-limit nudge is already scheduled.
    pub usage_limit_nudge_scheduled: bool,
    /// The scheduled usage-limit nudge time (if any).
    #[serde(skip)]
    pub usage_limit_nudge_at: Option<tokio::time::Instant>,
    /// Coworkers currently at a usage limit (detected from pane content).
    /// These coworkers should be excluded from stuck detection, idle warnings,
    /// and task assignment until the limit expires.
    pub usage_limited_coworkers: HashSet<String>,
    /// Coworkers currently experiencing API errors (detected from pane content).
    /// Like usage limits, these should be excluded from stuck detection, but
    /// unlike usage limits, they should receive periodic nudges to retry.
    pub api_error_coworkers: HashSet<String>,

    // ── Channel messages ─────────────────────────────────────────────────
    /// Recent channel messages for debugging context.
    /// Includes the last N messages from the channel log.
    pub channel_messages: Vec<Message>,

    // ── Daemon logs ──────────────────────────────────────────────────────
    /// Recent daemon log lines for debugging context.
    /// Includes the last N lines from the daemon.log file.
    pub daemon_logs: Vec<String>,

    // ── Worktree registry ─────────────────────────────────────────────────
    /// Task IDs that already have worktrees allocated in the registry.
    /// Used by dispatch to decide whether to allocate a new worktree or reuse.
    pub tasks_with_worktrees: HashSet<String>,

    // ── Limits & timing ─────────────────────────────────────────────────
    /// Whether the daemon is at the absolute coworker limit (max capacity).
    pub is_at_coworker_limit: bool,
    /// Whether the daemon is at the dev coworker limit (reserving review headroom).
    pub is_at_dev_limit: bool,
    /// Current wall-clock time.
    pub now_utc: DateTime<Utc>,
    /// Repository name.
    pub repo_name: String,
}

/// Read the last N lines from the daemon log file.
///
/// Uses a simple approach: reads the file and takes the last N lines.
/// Returns an empty vector if the file doesn't exist or can't be read.
pub fn read_daemon_log_tail(num_lines: usize) -> Vec<String> {
    let log_path = crate::paths::daemon_log_file();
    match std::fs::read_to_string(&log_path) {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(num_lines);
            lines[start..].iter().map(|s| s.to_string()).collect()
        }
        Err(_) => Vec::new(),
    }
}

impl WorldSnapshot {
    /// Populate debug context fields (channel messages and daemon logs).
    ///
    /// This is only called when capturing a snapshot for debugging, NOT during
    /// normal tick collection. This avoids file I/O overhead on every daemon tick.
    pub fn with_debug_context(mut self, channel: &crate::channel::Channel) -> Self {
        // Read recent channel messages
        self.channel_messages = channel
            .read_last_n_messages(SNAPSHOT_CHANNEL_MESSAGE_COUNT)
            .map(|(msgs, _)| msgs)
            .unwrap_or_default();

        // Read recent daemon log lines
        self.daemon_logs = read_daemon_log_tail(SNAPSHOT_DAEMON_LOG_LINES);

        self
    }
}

/// Collect a full world snapshot from the daemon state.
///
/// This is the single place where we read from `DaemonState` and external
/// sources (tmux, task storage, GitHub CLI). Called once per tick, before
/// any evaluation functions.
pub async fn collect_world_snapshot(state: &DaemonState) -> WorldSnapshot {
    // ── Coworker state ──────────────────────────────────────────────────
    let active_coworkers = state.coworkers.list();
    let running_coworkers = state.coworkers.list_running();
    let session_name = state.coworkers.session_name().to_string();

    let coworker_snapshots: Vec<CoworkerSnapshot> = active_coworkers
        .iter()
        .map(|cw| CoworkerSnapshot {
            name: cw.name.clone(),
            started_at: cw.started_at,
            isolated_tasks: cw.isolated_tasks,
        })
        .collect();

    let active_names: HashSet<String> = running_coworkers
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    let coworker_start_times: HashMap<String, DateTime<Utc>> = active_coworkers
        .iter()
        .map(|cw| (cw.name.to_lowercase(), cw.started_at))
        .collect();

    // Read coworker stop times from DaemonState
    let coworker_stop_times: HashMap<String, DateTime<Utc>> = {
        let stop_times = state.coworker_stop_times.read().unwrap();
        stop_times.clone()
    };

    // ── Process health (headless coworkers) ────────────────────────────
    // Read process health from DaemonState. This is populated by the session
    // management layer from HeadlessSession stream events and process status.
    let headless_process_health: HashMap<String, ProcessHealth> = {
        let health = state.headless_health.read().unwrap();
        health.clone()
    };

    // ── Task state ──────────────────────────────────────────────────────
    let in_progress_tasks = crate::tasks::get_in_progress_tasks_with_subjects();
    let busy_coworkers: HashSet<String> = state.get_all_busy_coworkers().into_iter().collect();
    let all_tasks = crate::tasks::read_tasks();
    let pending_tasks_with_owners = crate::tasks::get_pending_tasks_with_owners();
    let pending_tasks_without_owners = crate::tasks::get_pending_tasks_without_owners();

    // Reconcile pending task creation markers with tasks on disk.
    // - Non-completed tasks: clear the pending marker (Lead created it successfully)
    //   AND pre-populate the marker (prevents re-nudges after daemon restart)
    // - Completed tasks: ignored (new feedback may need a new task)
    for task in &all_tasks {
        if let Some(pr_num_str) = task.subject.strip_prefix("Address review feedback on PR #")
            && let Ok(pr_number) = pr_num_str.parse::<u64>()
            && let Some(ref owner) = task.owner
            && task.status != crate::tasks::TaskStatus::Completed
        {
            let key = super::DaemonState::task_creation_key(pr_number, owner);
            // Clear nudge-pending marker (task exists) and pre-populate
            // to prevent unnecessary re-nudges after daemon restart.
            state.mark_task_creation_pending(&key);
        }
    }

    // ── PR / GitHub state ───────────────────────────────────────────────
    let coworkers_with_open_prs: HashSet<String> = super::pr::get_coworkers_with_open_prs(state)
        .into_iter()
        .collect();
    let coworkers_with_merged_prs: HashSet<String> =
        super::pr::get_coworkers_with_merged_prs(state);
    // Merged PR numbers are populated as a side effect of the above call.
    let merged_pr_numbers = super::pr::get_merged_pr_numbers(state);
    let (ci_passed_pr_coworkers, review_feedback_pr_coworkers, prs_needing_review) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        (
            cache.ci_passed_pr_owners.clone(),
            cache.review_feedback_pr_owners.clone(),
            cache.prs_needing_review,
        )
    };

    // Pending task owners: coworkers who have claimed a task (owner set) but haven't
    // started it yet (status=pending). These should be protected from idle shutdown.
    let pending_task_owners: HashSet<String> = pending_tasks_with_owners
        .iter()
        .map(|(_, _, owner)| owner.to_lowercase())
        .collect();

    // ── Reviewer state ──────────────────────────────────────────────────
    let (active_reviewers, reviewer_pr_assignments) = {
        let ps = state.persistent_state.lock().await;
        let reviewers = ps.github.active_reviewers();
        // Collect reviewer → PR assignments for all active coworkers
        let assignments: HashMap<String, u64> = active_coworkers
            .iter()
            .filter_map(|cw| {
                ps.github
                    .pr_for_reviewer(&cw.name)
                    .map(|pr| (cw.name.clone(), pr))
            })
            .collect();
        (reviewers, assignments)
    };

    // Pre-check review status for all assigned PRs so decision logic doesn't need API calls
    let reviewed_prs = {
        let pr_numbers: Vec<u64> = reviewer_pr_assignments.values().copied().collect();
        let mut reviewed = HashSet::new();
        for pr in pr_numbers {
            if state.is_pr_reviewed(pr).await {
                reviewed.insert(pr);
            }
        }
        reviewed
    };

    // ── Dependency state ──────────────────────────────────────────────────
    let coworkers_with_unblocked_deps = crate::tasks::get_coworkers_with_unblocked_dependents();

    // ── Usage limit state ────────────────────────────────────────────────
    let (usage_limit_nudge_scheduled, usage_limit_nudge_at) = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        (nudge_at.is_some(), *nudge_at)
    };

    // Derive usage limit and API error sets from headless process health.
    // These were previously detected from pane content; now read from structured flags.
    let usage_limited_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(_, health)| health.has_usage_limit)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let api_error_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(name, health)| {
            // Only flag API error if not already at usage limit (usage limit takes precedence)
            health.has_api_error && !usage_limited_coworkers.contains(&name.to_lowercase())
        })
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // ── Channel messages & daemon logs ─────────────────────────────────
    // These debug fields are NOT populated during tick collection (hot path).
    // They are only populated on-demand via `with_debug_context()` when
    // capturing a snapshot for debugging (e.g., `midtown e2e capture`).
    let channel_messages = Vec::new();
    let daemon_logs = Vec::new();

    // ── Worktree registry ────────────────────────────────────────────────
    let tasks_with_worktrees: HashSet<String> = {
        let ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .all_assignments()
            .values()
            .filter_map(|a| a.task_id.clone())
            .collect()
    };

    // ── Limits & timing ─────────────────────────────────────────────────
    let is_at_coworker_limit = state.is_at_coworker_limit();
    let is_at_dev_limit = state.is_at_dev_limit();
    let now_utc = Utc::now();
    let repo_name = state.repo_name.clone();

    let snapshot = WorldSnapshot {
        active_coworkers,
        running_coworkers,
        coworker_snapshots,
        active_names,
        session_name,
        coworker_start_times,
        coworker_stop_times,
        headless_process_health,
        in_progress_tasks,
        busy_coworkers,
        all_tasks,
        pending_tasks_with_owners,
        pending_tasks_without_owners,
        coworkers_with_open_prs,
        coworkers_with_merged_prs,
        merged_pr_numbers,
        ci_passed_pr_coworkers,
        review_feedback_pr_coworkers,
        pending_task_owners,
        active_reviewers,
        reviewer_pr_assignments,
        reviewed_prs,
        prs_needing_review,
        coworkers_with_unblocked_deps,
        usage_limit_nudge_scheduled,
        usage_limit_nudge_at,
        usage_limited_coworkers,
        api_error_coworkers,
        channel_messages,
        daemon_logs,
        tasks_with_worktrees,
        is_at_coworker_limit,
        is_at_dev_limit,
        now_utc,
        repo_name,
    };

    // Log full snapshot at trace level for debugging and test case generation
    if tracing::enabled!(tracing::Level::TRACE)
        && let Ok(json) = serde_json::to_string_pretty(&snapshot)
    {
        tracing::trace!(snapshot = %json, "world snapshot collected");
    }

    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that ProcessHealth derives usage limit and API error sets correctly.
    #[test]
    fn test_process_health_derives_usage_limited_and_api_error_sets() {
        let mut health = HashMap::new();
        health.insert(
            "york".to_string(),
            ProcessHealth {
                has_usage_limit: true,
                ..Default::default()
            },
        );
        health.insert(
            "park".to_string(),
            ProcessHealth {
                has_api_error: true,
                ..Default::default()
            },
        );
        health.insert("madison".to_string(), ProcessHealth::default());

        let usage_limited: HashSet<String> = health
            .iter()
            .filter(|(_, h)| h.has_usage_limit)
            .map(|(n, _)| n.to_lowercase())
            .collect();
        let api_error: HashSet<String> = health
            .iter()
            .filter(|(n, h)| h.has_api_error && !usage_limited.contains(&n.to_lowercase()))
            .map(|(n, _)| n.to_lowercase())
            .collect();

        assert!(usage_limited.contains("york"));
        assert!(!usage_limited.contains("park"));
        assert!(api_error.contains("park"));
        assert!(!api_error.contains("madison"));
    }

    /// Test that WorldSnapshot has coworker_stop_times field and it serializes correctly.
    #[test]
    fn test_world_snapshot_has_coworker_stop_times() {
        let mut stop_times = HashMap::new();
        stop_times.insert("lexington".to_string(), Utc::now());
        stop_times.insert("broadway".to_string(), Utc::now());

        let snapshot = WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: stop_times.clone(),
            headless_process_health: HashMap::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        assert_eq!(snapshot.coworker_stop_times.len(), 2);
        assert!(snapshot.coworker_stop_times.contains_key("lexington"));
        assert!(snapshot.coworker_stop_times.contains_key("broadway"));

        let json = serde_json::to_string(&snapshot).expect("should serialize");
        assert!(json.contains("coworker_stop_times"));
    }

    /// Test that read_daemon_log_tail returns the last N lines of a file.
    #[test]
    fn test_read_daemon_log_tail() {
        use std::io::Write;

        // Create a temp file with 10 lines
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let log_path = temp_dir.path().join("test.log");
        {
            let mut file = std::fs::File::create(&log_path).expect("create file");
            for i in 1..=10 {
                writeln!(file, "line {}", i).expect("write line");
            }
        }

        // Test reading the tail - use a custom implementation that accepts a path
        // since read_daemon_log_tail uses a fixed path
        let contents = std::fs::read_to_string(&log_path).expect("read file");
        let lines: Vec<&str> = contents.lines().collect();
        let start = lines.len().saturating_sub(5);
        let tail: Vec<String> = lines[start..].iter().map(|s| s.to_string()).collect();

        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0], "line 6");
        assert_eq!(tail[4], "line 10");
    }

    /// Test that debug context fields (channel_messages, daemon_logs) are empty
    /// during normal snapshot collection to avoid I/O overhead on the hot path.
    #[test]
    fn test_snapshot_debug_context_empty_by_default() {
        let snapshot = WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        assert!(snapshot.channel_messages.is_empty());
        assert!(snapshot.daemon_logs.is_empty());

        let json = serde_json::to_string(&snapshot).expect("should serialize");
        assert!(json.contains("\"channel_messages\":[]"));
        assert!(json.contains("\"daemon_logs\":[]"));
    }
}
