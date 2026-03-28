//! Snapshot utilities — PR/task index, process health, stale worktree detection.
//!
//! Retained from the former WorldSnapshot module. Decision functions now read
//! directly from `DaemonPersistentState` tick fields; this module provides
//! supporting types (`PrTaskIndex`, `ProcessHealth`, `CachedHealthSets`) and
//! async helpers (`collect_stale_channel_lead_worktrees`).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::DaemonState;

/// Deserialize helper for `HashMap<u64, V>` that tolerates string-encoded keys.
///
/// `#[serde(flatten)]` converts JSON through an intermediate `Content` type where
/// object keys are always strings. The standard `u64` key deserializer expects an
/// integer, causing "invalid type: string, expected u64" errors. This helper
/// accepts both string and integer key representations.
mod u64_key_map {
    use serde::{Deserialize, Deserializer};
    use std::collections::HashMap;

    pub fn deserialize<'de, D, V>(deserializer: D) -> Result<HashMap<u64, V>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let string_map: HashMap<String, V> = HashMap::deserialize(deserializer)?;
        string_map
            .into_iter()
            .map(|(k, v)| {
                k.parse::<u64>()
                    .map(|k| (k, v))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// Unified index of PR↔task mappings from two data sources.
///
/// Replaces three separate fields (`github_open_pr_task_ids`, `tasks_with_open_prs`,
/// `pr_task_associations`) with a single struct that builds both forward (task→PR)
/// and reverse (PR→task) maps once.
///
/// Two sources are kept separate internally because they have different reliability:
/// - **Session-derived** (`session_task_to_pr`): authoritative when fresh, but stale
///   after daemon restart until sessions reconnect.
/// - **GitHub-title-derived** (`github_task_to_pr`): survives restarts (repopulated
///   from GitHub API) but depends on `[Midtown !{id}]` in PR titles.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PrTaskIndex {
    /// task_id → pr_number from SessionRecord (primary source).
    #[serde(default, alias = "tasks_with_open_prs")]
    session_task_to_pr: HashMap<String, u64>,
    /// task_id → pr_number from GitHub PR titles (defense-in-depth backup).
    #[serde(default, alias = "github_open_pr_task_ids")]
    github_task_to_pr: HashMap<String, u64>,
    /// pr_number → task_id (inverse of `session_task_to_pr`).
    #[serde(
        default,
        alias = "pr_task_associations",
        deserialize_with = "u64_key_map::deserialize"
    )]
    pr_to_task: HashMap<u64, String>,
}

impl PrTaskIndex {
    /// Build a new index from session-derived and GitHub-title-derived data.
    ///
    /// `pr_to_task` is built directly from sessions (not by reversing `session_task_to_pr`)
    /// because `session_task_to_pr` is many-to-one (multiple sessions with different PRs
    /// can map to the same task, but only one PR survives in the task→PR HashMap). Building
    /// `pr_to_task` directly preserves all PR→task associations.
    pub fn new(
        session_task_to_pr: HashMap<String, u64>,
        github_task_to_pr: HashMap<String, u64>,
        pr_to_task: HashMap<u64, String>,
    ) -> Self {
        Self {
            session_task_to_pr,
            github_task_to_pr,
            pr_to_task,
        }
    }

    /// Convenience constructor that derives `pr_to_task` by reversing `session_task_to_pr`.
    ///
    /// Safe when each task maps to at most one PR (the common case in tests). In production,
    /// prefer `new()` with an independently-built `pr_to_task` from session records to avoid
    /// losing associations when multiple PRs map to the same task.
    pub fn from_task_maps(
        session_task_to_pr: HashMap<String, u64>,
        github_task_to_pr: HashMap<String, u64>,
    ) -> Self {
        let pr_to_task: HashMap<u64, String> = session_task_to_pr
            .iter()
            .map(|(task, &pr)| (pr, task.clone()))
            .collect();
        Self::new(session_task_to_pr, github_task_to_pr, pr_to_task)
    }

    /// Look up the PR number for a task from session data.
    pub fn session_pr_for_task(&self, task_id: &str) -> Option<u64> {
        self.session_task_to_pr.get(task_id).copied()
    }

    /// Look up the PR number for a task from GitHub title data.
    pub fn github_pr_for_task(&self, task_id: &str) -> Option<u64> {
        self.github_task_to_pr.get(task_id).copied()
    }

    /// Check if a task has an associated PR from either source.
    pub fn task_has_pr(&self, task_id: &str) -> bool {
        self.session_task_to_pr.contains_key(task_id)
            || self.github_task_to_pr.contains_key(task_id)
    }

    /// Look up the task ID for a PR number (session-derived, reverse map).
    pub fn task_for_pr(&self, pr_number: u64) -> Option<&str> {
        self.pr_to_task.get(&pr_number).map(|s| s.as_str())
    }

    /// Check if a PR number has an associated task (session-derived).
    pub fn pr_has_task(&self, pr_number: &u64) -> bool {
        self.pr_to_task.contains_key(pr_number)
    }

    /// Iterate over all (pr_number, task_id) pairs from session data.
    pub fn pr_task_pairs(&self) -> impl Iterator<Item = (u64, &str)> {
        self.pr_to_task
            .iter()
            .map(|(&pr, task)| (pr, task.as_str()))
    }

    /// Iterate over all (task_id, pr_number) pairs from GitHub title data.
    /// Used by `collect_pr_task_link_effects` for PR→task auto-link repair.
    pub fn github_task_pr_pairs(&self) -> impl Iterator<Item = (&str, u64)> {
        self.github_task_to_pr
            .iter()
            .map(|(task, &pr)| (task.as_str(), pr))
    }

    /// Expose the session-derived PR→task map for callers that need `&HashMap<u64, String>`.
    /// Used by `resolve_pr_owner_from_session` which is shared between the snapshot path
    /// and async webhook handlers (the latter pass their own HashMap).
    pub fn pr_to_task_map(&self) -> &HashMap<u64, String> {
        &self.pr_to_task
    }
}

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
    /// When the usage limit will reset (if known).
    pub usage_limit_reset_at: Option<DateTime<Utc>>,
    /// Whether the coworker is experiencing API errors (transient failures
    /// that may resolve on retry).
    pub has_api_error: bool,
    /// Whether the coworker has an authentication error (OAuth token expired).
    /// Unlike API errors and usage limits, auth errors require user intervention
    /// to re-authenticate and won't resolve with retries.
    #[serde(default)]
    pub has_auth_error: bool,
    /// Whether the coworker has a running Task tool subagent.
    /// When true, the parent session may not emit events for several minutes
    /// while the subagent works — stuck detection should skip these coworkers.
    pub has_running_subagent: bool,
    /// Whether the coworker has a pending tool execution (saw tool_use but no tool_result yet).
    /// When true, the session is waiting for a tool to complete (e.g., long-running Bash command)
    /// and shouldn't be considered stuck even if no events are emitted during execution.
    pub has_pending_tool: bool,
    /// Whether the coworker has an unrecoverable session error (e.g., duplicate MCP tool names,
    /// stale Codex session IDs, or context exhaustion). When true, the session needs a restart.
    #[serde(default)]
    pub has_tool_name_conflict: bool,
    /// Whether the coworker is waiting for the next API response after a tool result.
    ///
    /// Set when a `tool_result` arrives (clearing `has_pending_tool`), cleared when the
    /// next `assistant` event arrives. During this window the model may be doing extended
    /// thinking — no stream events are emitted — so stuck detection must not fire.
    #[serde(default)]
    pub has_pending_api_call: bool,
    /// Process exit code, if the process has terminated.
    pub exit_code: Option<i32>,
}

/// Cached health-derived sets, keyed by `headless_health_generation` in `DaemonState`.
///
/// All fields are pure derivations from `ProcessHealth` boolean flags — no dependency
/// on wall-clock time. `coworkers_with_active_tools` is intentionally excluded: its
/// `pending_api_turn_fresh` check depends on `now_utc`, so it must be recomputed
/// every tick.
#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct CachedHealthSets {
    pub usage_limited_coworkers: HashSet<String>,
    pub auth_error_coworkers: HashSet<String>,
    pub api_error_coworkers: HashSet<String>,
    pub tool_name_conflict_coworkers: HashSet<String>,
}

/// Derive the 4 cacheable health sets from raw process health data.
///
/// Priority ordering: auth errors take precedence over usage limits and API errors.
/// A coworker in `auth_error_coworkers` is excluded from `api_error_coworkers`.
#[allow(dead_code)]
pub(super) fn compute_health_sets(health: &HashMap<String, ProcessHealth>) -> CachedHealthSets {
    let usage_limited_coworkers: HashSet<String> = health
        .iter()
        .filter(|(_, h)| h.has_usage_limit)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let auth_error_coworkers: HashSet<String> = health
        .iter()
        .filter(|(_, h)| h.has_auth_error)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let api_error_coworkers: HashSet<String> = health
        .iter()
        .filter(|(name, h)| {
            h.has_api_error
                && !auth_error_coworkers.contains(&name.to_lowercase())
                && !usage_limited_coworkers.contains(&name.to_lowercase())
        })
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let tool_name_conflict_coworkers: HashSet<String> = health
        .iter()
        .filter(|(_, h)| h.has_tool_name_conflict)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    CachedHealthSets {
        usage_limited_coworkers,
        auth_error_coworkers,
        api_error_coworkers,
        tool_name_conflict_coworkers,
    }
}

impl Default for ProcessHealth {
    fn default() -> Self {
        Self {
            is_alive: true,
            last_event_at: None,
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: None,
        }
    }
}

/// Compute the active reviewers set from task_session_spans.
///
/// Returns all reviewers with an open span (end_time = None, agent_type = "reviewer")
/// where either:
/// - The associated session record's `is_running` flag is true, OR
/// - The process health map shows the agent as alive.
///
/// This replaces the old `compute_active_reviewers_with_health` that read from
/// `GitHubState` assignment tracking + process health. The span-based approach uses
/// `SessionRecord.is_running` directly, eliminating the assignment timeout race
/// that required the grace window.
#[allow(dead_code)]
pub(crate) fn compute_active_reviewers_from_spans(
    ps: &crate::daemon::state::DaemonPersistentState,
    process_health: &HashMap<String, ProcessHealth>,
) -> HashSet<String> {
    let mut reviewers = HashSet::new();
    for span in ps.all_reviewer_sessions() {
        let is_running = ps
            .sessions
            .get(&span.session_id)
            .map(|s| s.is_running)
            .unwrap_or(false);
        let is_alive = process_health
            .get(&span.name)
            .map(|h| h.is_alive)
            .unwrap_or(false);
        if is_running || is_alive {
            reviewers.insert(span.name.clone());
        }
    }
    reviewers
}

/// Build the reviewer → PR number assignment map from reviewer sessions.
///
/// Maps agent_name → PR number via TaskStore (task_id → pr lookup).
/// Dead reviewers (whose process has exited but whose session record is
/// still open) are included so that `decide_dead_reviewer_respawns` can
/// detect and respawn them.
pub(crate) fn build_reviewer_pr_assignments_from_spans(
    ps: &crate::daemon::state::DaemonPersistentState,
    task_to_pr: &HashMap<String, u64>,
) -> HashMap<String, u64> {
    let mut assignments = HashMap::new();
    for span in ps.all_reviewer_sessions() {
        if let Some(pr) = span.task_id.as_ref().and_then(|tid| task_to_pr.get(tid)) {
            assignments.insert(span.name.clone(), *pr);
        }
    }
    assignments
}

/// How long to cache worktree freshness results before re-running git fetch.
const WORKTREE_FRESHNESS_CACHE_SECS: u64 = 25;

/// Collect channel lead worktrees that are behind `origin/<default_branch>`.
///
/// Runs a single `git fetch origin <branch> --quiet` in the project root, then
/// for each channel lead session, uses `git merge-base --is-ancestor` to check
/// whether HEAD is behind `origin/<branch>`.
///
/// Results are cached for [`WORKTREE_FRESHNESS_CACHE_SECS`] to avoid running
/// git fetch on every tick (snapshot collection runs for both SessionMonitorTick
/// at ~30s and TaskDispatchTick at ~5s).
///
/// Returns the set of channel names whose worktrees are behind.
pub(super) async fn collect_stale_channel_lead_worktrees(
    state: &DaemonState,
    channel_lead_sessions: &HashMap<String, String>,
    sessions: &HashMap<String, crate::daemon::state::SessionRecord>,
) -> HashSet<String> {
    if channel_lead_sessions.is_empty() {
        return HashSet::new();
    }

    // Check cache — return cached result if fresh enough.
    {
        let cache = state.worktree_freshness_cache.lock().unwrap();
        if let Some((timestamp, ref cached_result)) = *cache
            && timestamp.elapsed().as_secs() < WORKTREE_FRESHNESS_CACHE_SECS
        {
            return cached_result.clone();
        }
    }

    // Run a single git fetch in the project root to update origin/<branch> refs.
    let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
    if project_root.as_os_str().is_empty() || !project_root.exists() {
        return HashSet::new();
    }

    let default_branch = &state.default_branch;
    let origin_ref = format!("origin/{}", default_branch);

    let fetch_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("git")
            .args(["fetch", "origin", default_branch, "--quiet"])
            .current_dir(&project_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await;

    match fetch_result {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(_)) | Ok(Err(_)) => {
            tracing::debug!(
                "git fetch origin {} failed — skipping worktree freshness check",
                default_branch
            );
            return HashSet::new();
        }
        Err(_) => {
            tracing::debug!(
                "git fetch origin {} timed out — skipping worktree freshness check",
                default_branch
            );
            return HashSet::new();
        }
    }

    let mut stale = HashSet::new();

    for (channel_name, session_id) in channel_lead_sessions {
        let record = match sessions.get(session_id) {
            Some(r) => r,
            None => continue,
        };

        let working_dir = &record.working_dir;
        if working_dir.is_empty() {
            continue;
        }

        let wd = std::path::Path::new(working_dir);
        if !wd.exists() {
            continue;
        }

        // Use merge-base --is-ancestor to check if HEAD is behind origin/<branch>.
        // This correctly handles ahead/diverged worktrees — only truly behind
        // worktrees (where origin/<branch> is NOT an ancestor of HEAD) are flagged.
        let ancestor_output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new("git")
                .args(["merge-base", "--is-ancestor", &origin_ref, "HEAD"])
                .current_dir(wd)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status(),
        )
        .await;

        let is_up_to_date = match ancestor_output {
            Ok(Ok(status)) => status.success(),
            _ => continue, // timeout or error — skip this worktree
        };

        if !is_up_to_date {
            tracing::debug!(
                "Channel lead '{}' worktree is behind {} (merge-base check failed)",
                channel_name,
                origin_ref
            );
            stale.insert(channel_name.clone());
        }
    }

    // Update cache.
    {
        let mut cache = state.worktree_freshness_cache.lock().unwrap();
        *cache = Some((std::time::Instant::now(), stale.clone()));
    }

    stale
}

#[path = "snapshot_tests.rs"]
#[cfg(test)]
mod tests;
