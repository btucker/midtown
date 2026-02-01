//! World snapshot — an immutable view of all daemon state for a single tick.
//!
//! Pure evaluation functions read from the snapshot instead of reaching into
//! `DaemonState` directly. This eliminates duplicate data fetching across
//! multiple check functions within the same tick and makes decision logic
//! easier to test.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::coworker::Coworker;
use crate::rules::CoworkerSnapshot;
use crate::tasks::Task;

use super::DaemonState;

/// Immutable snapshot of the daemon's world, collected once per tick.
///
/// Each field is owned data — no references back to `DaemonState`. This means
/// evaluation functions that take `&WorldSnapshot` cannot accidentally trigger
/// side effects on the underlying state.
#[derive(Debug)]
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

    // ── Pane contents ───────────────────────────────────────────────────
    /// Captured tmux pane content per coworker (keyed by name).
    pub pane_contents: HashMap<String, String>,
    /// Running coworkers whose pane is entirely blank (no visible output).
    pub blank_pane_coworkers: HashSet<String>,

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
    /// Coworkers whose open PR has all CI checks passing (eligible for PR break).
    pub ci_passed_pr_coworkers: HashSet<String>,

    // ── Reviewer state ──────────────────────────────────────────────────
    /// Currently active reviewers (from both in-memory tracker and persistent state).
    pub active_reviewers: HashSet<String>,
    /// Reviewer → assigned PR number mapping (from github-state.json).
    pub reviewer_pr_assignments: HashMap<String, u64>,
    /// PRs that have been verified as reviewed (Claude review comment exists).
    /// Pre-collected during snapshot so decision logic doesn't need API calls.
    pub reviewed_prs: HashSet<u64>,

    // ── Dependency state ──────────────────────────────────────────────────
    /// Coworkers whose completed tasks have unblocked pending follow-ups.
    pub coworkers_with_unblocked_deps: HashSet<String>,

    // ── Usage limit state ────────────────────────────────────────────────
    /// Whether a usage-limit nudge is already scheduled.
    pub usage_limit_nudge_scheduled: bool,
    /// The scheduled usage-limit nudge time (if any).
    pub usage_limit_nudge_at: Option<tokio::time::Instant>,

    // ── Limits & timing ─────────────────────────────────────────────────
    /// Whether the daemon is at the dev coworker limit.
    pub is_at_dev_limit: bool,
    /// Current monotonic instant (for timeout comparisons).
    pub now: Instant,
    /// Current wall-clock time.
    pub now_utc: DateTime<Utc>,
    /// Repository name.
    pub repo_name: String,
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

    // ── Pane contents ───────────────────────────────────────────────────
    let mut pane_contents = HashMap::new();
    for cw in &active_coworkers {
        let target = format!("{}:{}", session_name, cw.name);
        if let Some(content) = crate::tmux::capture_pane(&target) {
            pane_contents.insert(cw.name.clone(), content);
        }
    }

    // Derive blank-pane set: running coworkers whose pane has no visible output
    let blank_pane_coworkers: HashSet<String> = running_coworkers
        .iter()
        .filter(|cw| {
            pane_contents
                .get(&cw.name)
                .map(|c| !crate::tmux::content_has_output(c))
                .unwrap_or(true) // no pane content captured → treat as blank
        })
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // ── Task state ──────────────────────────────────────────────────────
    let in_progress_tasks = crate::tasks::get_in_progress_tasks_with_subjects();
    let busy_coworkers: HashSet<String> =
        crate::tasks::get_busy_coworkers_for_repo(&state.repo_name)
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
    let all_tasks = crate::tasks::read_tasks();
    let pending_tasks_with_owners = crate::tasks::get_pending_tasks_with_owners();
    let pending_tasks_without_owners = crate::tasks::get_pending_tasks_without_owners();

    // ── PR / GitHub state ───────────────────────────────────────────────
    let coworkers_with_open_prs: HashSet<String> = super::get_coworkers_with_open_prs(state)
        .into_iter()
        .collect();
    let coworkers_with_merged_prs: HashSet<String> = super::get_coworkers_with_merged_prs(state);
    let ci_passed_pr_coworkers: HashSet<String> = {
        let cache = state.pr_coworker_cache.read().unwrap();
        cache.ci_passed_pr_owners.clone()
    };

    // ── Reviewer state ──────────────────────────────────────────────────
    let (active_reviewers, reviewer_pr_assignments) = {
        let github_state = state.github_state.lock().await;
        let reviewers = github_state.active_reviewers();
        // Collect reviewer → PR assignments for all active coworkers
        let assignments: HashMap<String, u64> = active_coworkers
            .iter()
            .filter_map(|cw| {
                github_state
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

    // ── Limits & timing ─────────────────────────────────────────────────
    let is_at_dev_limit = state.is_at_dev_limit();
    let now = Instant::now();
    let now_utc = Utc::now();
    let repo_name = state.repo_name.clone();

    WorldSnapshot {
        active_coworkers,
        running_coworkers,
        coworker_snapshots,
        active_names,
        session_name,
        coworker_start_times,
        pane_contents,
        blank_pane_coworkers,
        in_progress_tasks,
        busy_coworkers,
        all_tasks,
        pending_tasks_with_owners,
        pending_tasks_without_owners,
        coworkers_with_open_prs,
        coworkers_with_merged_prs,
        ci_passed_pr_coworkers,
        active_reviewers,
        reviewer_pr_assignments,
        reviewed_prs,
        coworkers_with_unblocked_deps,
        usage_limit_nudge_scheduled,
        usage_limit_nudge_at,
        is_at_dev_limit,
        now,
        now_utc,
        repo_name,
    }
}
