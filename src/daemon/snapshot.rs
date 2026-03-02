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
    /// Whether the coworker has a tool name conflict (e.g., duplicate MCP tool names).
    /// When true, the session may fail tool calls and needs a restart.
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

/// Default value for `lead_session_refresh_interval_secs` when deserializing from older snapshots.
fn default_lead_refresh_interval() -> u64 {
    crate::daemon::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS
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
#[derive(Debug, serde::Serialize, serde::Deserialize)]
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
    /// Session IDs of active coworkers (for session-first lookups).
    /// Populated alongside `active_names` during snapshot collection.
    #[serde(default)]
    pub active_session_ids: HashSet<String>,
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

    // ── Attached coworkers ───────────────────────────────────────────
    /// Coworkers currently in "attached" state, mapped to their attach timestamp.
    ///
    /// Entries are added (with current time) on attach, removed on detach.
    /// Must be excluded from stuck detection and orphan recovery.
    /// The timestamp enables auto-detach of stale entries when the interactive
    /// session ends without a proper `midtown session detach`.
    pub attached_coworkers: HashMap<String, chrono::DateTime<chrono::Utc>>,

    // ── Task state ──────────────────────────────────────────────────────
    /// In-progress tasks: `(task_id, subject, owner)`.
    pub in_progress_tasks: Vec<(String, String, String)>,
    /// Names of coworkers who are busy (have in-progress tasks), lowercase.
    pub busy_coworkers: HashSet<String>,
    /// Coworker → task assignment mapping (from daemon in-memory tracking).
    /// Maps coworker name (lowercase) → task_id. Used by task dispatch to prevent
    /// re-assigning the same task to the same coworker (nudge/spawn loop prevention).
    #[serde(default)]
    pub coworker_task_assignments: HashMap<String, String>,
    /// All tasks from disk (for relationship lookups).
    pub all_tasks: Vec<Task>,
    /// Pending tasks that have an owner: `(task_id, subject, owner)`.
    pub pending_tasks_with_owners: Vec<(String, String, String)>,
    /// Pending tasks with no owner (unclaimed, past grace period, unblocked).
    pub pending_tasks_without_owners: Vec<Task>,
    /// Task-to-channel assignment mapping for message routing.
    /// Maps task ID → channel name. Used by rules.rs to route coworker messages
    /// to the appropriate topic channel based on the task they're working on.
    pub task_channel: HashMap<String, String>,
    /// Task-to-model assignment mapping for coworker spawning.
    /// Maps task ID → model specification (e.g., "claude/opus", "claude/sonnet").
    /// Used by dispatch.rs to launch coworkers with the requested model when spawning
    /// for a task. Stored in DaemonPersistentState and loaded here for decision functions.
    #[serde(default)]
    pub task_model_map: HashMap<String, String>,
    /// Task-to-plan mapping for plan-driven execution.
    /// Maps task ID → absolute path to a plan file. Used by dispatch.rs to include
    /// plan content in the coworker's initial prompt when spawning for a task with a plan.
    #[serde(default)]
    pub task_plan_map: HashMap<String, String>,
    /// Task-to-execution-skill mapping for plan-driven execution.
    /// Maps task ID → skill name (e.g., "subagent-driven-development", "executing-plans").
    /// Used by dispatch.rs to include an explicit skill instruction in the coworker's
    /// initial prompt when spawning for a task with an execution skill.
    #[serde(default)]
    pub task_execution_skill_map: HashMap<String, String>,
    /// Channel lead session mapping for nudge routing.
    /// Maps channel name → session ID. Used by effects.rs to deliver
    /// `NudgeChannelLead` effects without locking persistent state.
    #[serde(default)]
    pub channel_lead_sessions: HashMap<String, String>,

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
    /// Open PR data (from last GitHub poll). Used by orphan PR reconciliation.
    /// Pre-collected during snapshot so decision logic doesn't need to lock pr_coworker_cache.
    #[serde(default)]
    pub open_prs_data: Vec<serde_json::Value>,
    /// Task IDs that have open PRs (derived from PR titles in `open_prs_data`).
    /// Maps task_id → pr_number. Complements `tasks_with_open_prs` (from pr_author_sessions)
    /// by catching cases where pr_author_sessions is stale after a daemon restart but the
    /// PR title contains `[Midtown !{task_id}]`. Used by:
    /// - Orphan recovery (`dispatch.rs`): prevent spawning duplicate coworkers.
    /// - PR→task auto-link repair (`pr.rs`): emit `SetTaskPr` as a polling fallback
    ///   when webhooks missed the PR open event (see `collect_pr_task_link_effects`).
    #[serde(default)]
    pub github_open_pr_task_ids: HashMap<String, u64>,
    /// Coworkers who have pending tasks assigned to them (task.owner set, status=pending).
    /// Provides defense-in-depth idle shutdown protection alongside `busy_coworkers`
    /// (in-memory assignment tracking). Both paths are checked to prevent the
    /// spawn→idle→break loop (see PR #650).
    pub pending_task_owners: HashSet<String>,
    /// Task IDs that have associated open PRs (from PrAuthorSession).
    /// Maps task_id → pr_number. Used by reconcile_tasks_in_review to detect
    /// tasks whose PR is open but whose owner is no longer active.
    pub tasks_with_open_prs: HashMap<String, u64>,
    /// PR numbers with associated task IDs (from PrAuthorSession).
    /// Maps pr_number → task_id. Used by abandoned PR detection to reset tasks
    /// when PRs are closed without merging.
    pub pr_task_associations: HashMap<u64, String>,

    // ── Reviewer state ──────────────────────────────────────────────────
    /// Currently active reviewers (from both in-memory tracker and persistent state).
    pub active_reviewers: HashSet<String>,
    /// Coworkers currently in `WorkflowPhase::Reviewing` (lowercase names).
    /// Defense-in-depth guard for idle shutdown: protects reviewers when their
    /// assignment timestamp has expired but their session is still working.
    #[serde(default)]
    pub reviewing_phase_coworkers: HashSet<String>,
    /// Reviewer → assigned PR number mapping (from github-state.json).
    pub reviewer_pr_assignments: HashMap<String, u64>,
    /// Placeholder comment IDs for PRs with an unupdated "Review in progress" comment.
    /// Maps PR number → GitHub comment database ID.
    /// Pre-collected during snapshot (cached to minimize API calls).
    #[serde(default)]
    pub reviewer_in_progress_comment_ids: HashMap<u64, u64>,
    /// PRs that have been verified as reviewed (Claude review comment exists).
    /// Pre-collected during snapshot so decision logic doesn't need API calls.
    pub reviewed_prs: HashSet<u64>,
    /// Count of open PRs that need review (not draft, no Claude review, no formal review).
    /// Used by task dispatch to prioritize reviews over new task pickup.
    pub prs_needing_review: usize,
    /// PR number → restart count for reviewer assignments.
    /// Used by stuck reviewer detection to implement backoff.
    pub reviewer_restart_counts: HashMap<u64, u32>,
    /// PR numbers for which a reviewer escalation warning has already been posted.
    /// Prevents the escalation warning from firing every tick after max restarts.
    pub reviewer_escalations_posted: HashSet<u64>,
    /// PR numbers for which the lead has already been nudged about an orphaned PR.
    /// Prevents `reconcile_orphaned_prs` from nudging on every polling tick.
    #[serde(default)]
    pub orphaned_pr_lead_nudges_sent: HashSet<u64>,
    /// GitHub API rate limit state (GraphQL and REST quotas).
    /// Used by adaptive throttling to reduce polling frequency when quotas run low.
    pub github_rate_limit: crate::github_rate_limit::GitHubRateLimit,
    /// Freshly fetched rate limit data (only populated during RateLimitCheckTick).
    /// This carries the new rate limit state from the API fetch to the decision phase.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub freshly_fetched_rate_limit: Option<crate::github_rate_limit::GitHubRateLimit>,

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
    /// Coworkers currently experiencing authentication errors (OAuth token expired).
    /// Unlike API errors and usage limits, auth errors require user intervention
    /// to re-authenticate and will not resolve with retries or time.
    #[serde(default)]
    pub auth_error_coworkers: HashSet<String>,
    /// Coworkers currently experiencing tool name conflicts (duplicate MCP tool names).
    /// These coworkers need a restart to resolve the conflict.
    #[serde(default)]
    pub tool_name_conflict_coworkers: HashSet<String>,
    /// Coworkers with active in-flight work (pending tool/subagent or pending API turn).
    /// These sessions are protected from idle shutdown — killing mid-turn can drop responses.
    #[serde(default)]
    pub coworkers_with_active_tools: HashSet<String>,

    // ── Channel state ──────────────────────────────────────────────────
    /// Channels that have already been archived (`.archived.jsonl` exists).
    /// Used by the UI + command handlers to keep archived channels hidden by default
    /// and to prevent duplicate channel-lead recovery for archived topics.
    #[serde(default)]
    pub archived_channels: HashSet<String>,
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
    /// Mapping from task_id → worktree_id for tasks that have registered worktrees.
    /// Used by dispatch to reuse existing worktrees when reassigning tasks to
    /// different coworkers (preserves build cache and partial work).
    pub task_worktree_map: HashMap<String, String>,
    /// Complete worktree registry for stale worktree cleanup.
    /// Contains all worktree assignments with completion timestamps.
    /// Extracted from persistent state during snapshot collection for pure decision functions.
    #[serde(default)]
    pub worktree_registry: crate::worktree_registry::WorktreeRegistry,
    /// Branch name → coworker name mapping from the worktree registry.
    /// Used by `coworker_from_branch()` to look up task-based branches (task-*, review-pr-*).
    pub worktree_branch_owners: HashMap<String, String>,
    /// PR number → branch name mapping from the worktree registry for merged PRs.
    /// Used by `collect_merged_pr_cleanup_effects()` to generate cleanup effects without I/O.
    pub merged_pr_branches: HashMap<u64, String>,

    // ── Lead session refresh ─────────────────────────────────────────────
    /// Interval for periodic lead session refresh in seconds (0 = disabled).
    /// From daemon config — available to pure decision functions.
    #[serde(default = "default_lead_refresh_interval")]
    pub lead_session_refresh_interval_secs: u64,

    // ── Limits & timing ─────────────────────────────────────────────────
    /// Whether the daemon is at the absolute coworker limit (max capacity).
    pub is_at_coworker_limit: bool,
    /// Whether the daemon is at the dev coworker limit (reserving review headroom).
    pub is_at_dev_limit: bool,
    /// Current wall-clock time.
    pub now_utc: DateTime<Utc>,
    /// Repository name.
    pub repo_name: String,
    /// Default channel name (e.g., "midtown"). Used by pure decision functions
    /// to construct `NudgeChannelLead` effects that route to the project lead.
    #[serde(default)]
    pub default_channel: String,
    /// Repository owner (from git remote URL). Used by pure decision functions
    /// to determine if a PR is authored by the lead (repo owner).
    #[serde(default)]
    pub repo_owner: Option<String>,

    // ── Dispatch cooldown state ──────────────────────────────────────────
    /// Whether the orphan spawn global cooldown is currently active.
    /// Pre-evaluated from `state.cooldowns` so decision functions stay pure.
    #[serde(default)]
    pub orphan_spawn_cooldown_active: bool,
    /// Whether the session dispatch global cooldown is currently active.
    /// Pre-evaluated from `state.cooldowns` so decision functions stay pure.
    #[serde(default)]
    pub session_dispatch_cooldown_active: bool,
    /// Names of coworkers currently on the spawn failure cooldown.
    /// Pre-evaluated from `state.cooldowns` over all known coworker names.
    #[serde(default)]
    pub spawn_failure_cooldown_names: HashSet<String>,
    /// Session IDs for which a recovery was recently attempted (and succeeded).
    /// Pre-evaluated from `state.cooldowns` (category `"session_recovered"`, keyed
    /// by session ID) so decision functions stay pure.
    ///
    /// After a recovery spawn, a per-session-id cooldown ("session_recovered") is set.
    /// This set contains all session_ids whose cooldown is still active, preventing
    /// re-recovery on the next tick even if the session dies quickly.
    ///
    /// Without this guard, the global SESSION_DISPATCH_COOLDOWN (2s) always expires
    /// before the next 5s tick, causing "Session dispatch: recovered" to be posted
    /// to the ops channel on every tick (task !1709 regression).
    #[serde(default)]
    pub recently_recovered_session_ids: HashSet<String>,

    // ── Session-centric fields (new model) ──────────────────────────────
    /// All session records, keyed by session_id.
    ///
    /// Populated from `DaemonPersistentState::sessions` during snapshot collection.
    /// Initially empty until sessions are recorded via `Effect::RecordSession`.
    #[serde(default)]
    pub sessions: HashMap<String, crate::daemon::state::SessionRecord>,

    /// Task ID → session ID mapping (reverse of SessionRecord.task_id).
    ///
    /// Enables O(1) lookup: "which session is working on task X?"
    #[serde(default)]
    pub session_task_map: HashMap<String, String>,

    /// Session ID → current name mapping (only running sessions have names).
    ///
    /// Enables O(1) lookup: "what name does session X currently hold?"
    #[serde(default)]
    pub session_name_map: HashMap<String, String>,

    /// Name → session ID reverse mapping (for @mention routing).
    ///
    /// Enables O(1) lookup: "which session currently holds name X?"
    #[serde(default)]
    pub name_session_map: HashMap<String, String>,

    /// Session IDs whose `working_dir` path no longer exists on disk.
    ///
    /// Pre-evaluated during snapshot collection (filesystem I/O) so that
    /// decision functions in `dispatch.rs` can check staleness without
    /// performing `Path::exists()` calls themselves.
    #[serde(default)]
    pub stale_working_dir_sessions: HashSet<String>,

    // ── Auth profile pool ─────────────────────────────────────────────────
    /// Maps coworker name (lowercase) → auth profile email.
    ///
    /// Populated from `DaemonState::session_profile_map` during snapshot collection.
    /// Used by `check_for_usage_limits()` to attribute usage limits to specific
    /// pool profiles and emit `MarkProfileLimited` effects.
    #[serde(default)]
    pub session_profile_map: HashMap<String, String>,

    /// Pool profile emails that are currently marked `is_usage_limited=true`.
    ///
    /// Populated from `DaemonPersistentState::profile_pool_state` during snapshot
    /// collection. Used by `maybe_nudge_usage_limit_expiry()` to clear ALL limited
    /// profiles on expiry, regardless of whether the associated coworker session
    /// is still running. Unlike `session_profile_map` (ephemeral, cleared on coworker
    /// stop), this is derived from persistent state and survives daemon restarts.
    #[serde(default)]
    pub limited_pool_profiles: HashSet<String>,
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
    /// Build a session-ID-keyed health map from name-keyed health data.
    ///
    /// During migration, health is still collected per-name. This method
    /// translates to session-ID keys using `name_session_map`.
    pub fn session_health_map(&self) -> HashMap<String, &ProcessHealth> {
        let mut map = HashMap::new();
        for (name, health) in &self.headless_process_health {
            if let Some(session_id) = self.name_session_map.get(name) {
                map.insert(session_id.clone(), health);
            }
        }
        map
    }

    /// Returns the set of active channel lead names (keys of `channel_lead_sessions`).
    pub fn channel_lead_names(&self) -> HashSet<String> {
        self.channel_lead_sessions.keys().cloned().collect()
    }

    /// Build a [`crate::rules::StuckExemptions`] view from this snapshot.
    ///
    /// Centralises the four-field construction used by every stuck-detection
    /// call site (`decide_stuck_coworker_restarts`, `decide_stuck_reviewer_restarts`).
    pub(crate) fn stuck_exemptions(&self) -> crate::rules::StuckExemptions<'_> {
        crate::rules::StuckExemptions {
            usage_limited: &self.usage_limited_coworkers,
            api_error: &self.api_error_coworkers,
            auth_error: &self.auth_error_coworkers,
            attached: &self.attached_coworkers,
        }
    }

    /// Populate debug context fields (channel messages and daemon logs).
    ///
    /// This is only called when capturing a snapshot for debugging, NOT during
    /// normal tick collection. This avoids file I/O overhead on every daemon tick.
    /// Async because it uses the non-blocking channel read variant.
    pub async fn with_debug_context(mut self, channel: &crate::channel::Channel) -> Self {
        // Read recent channel messages
        self.channel_messages = channel
            .read_last_n_messages_async(SNAPSHOT_CHANNEL_MESSAGE_COUNT)
            .await
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
/// sources (task storage, GitHub CLI). Called once per tick, before
/// any evaluation functions.
pub(crate) async fn collect_world_snapshot(state: &DaemonState) -> WorldSnapshot {
    // ── Coworker state ──────────────────────────────────────────────────
    let active_coworkers = state.coworkers.list();
    let running_coworkers = state.coworkers.list_running();
    let session_name = format!("{}{}", crate::process::SESSION_PREFIX, state.repo_name);

    let coworker_snapshots: Vec<CoworkerSnapshot> = active_coworkers
        .iter()
        .map(|cw| CoworkerSnapshot {
            name: cw.name.clone(),
            started_at: cw.started_at,
            session_id: cw.session_id.clone(),
        })
        .collect();

    // Include running coworkers and alive headless sessions
    let mut active_names: HashSet<String> = running_coworkers
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // Add alive headless coworkers (fixes #904: orphan recovery loop for headless-only setups)
    let headless_active_names = state.session_manager.list_names().await;
    for name in headless_active_names {
        // Only add if the session is alive (per SessionManager's internal tracking)
        if state.session_manager.is_alive(&name).await {
            active_names.insert(name.to_lowercase());
        }
    }

    // Collect active session IDs from all coworkers that have a known session_id.
    // First from CoworkerManager (which has session_id on the Coworker struct),
    // then from SessionManager for headless sessions that may have reported their
    // session_id via the init StreamEvent.
    let mut active_session_ids: HashSet<String> = active_coworkers
        .iter()
        .filter(|cw| active_names.contains(&cw.name.to_lowercase()))
        .filter_map(|cw| cw.session_id.clone())
        .collect();
    for name in &active_names {
        if let Some(sid) = state.session_manager.get_session_id(name).await {
            active_session_ids.insert(sid);
        }
    }

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

    // ── Attached coworkers ──────────────────────────────────────────────
    let attached_coworkers: HashMap<String, chrono::DateTime<chrono::Utc>> = {
        let attached = state.attached_coworkers.lock().unwrap();
        attached.clone()
    };

    // ── Task state ──────────────────────────────────────────────────────
    // IMPORTANT: Use _for_repo variants to avoid dependency on cwd.
    // The daemon may run from a directory where detect_repo_name() fails,
    // but state.repo_name is set correctly at startup. Using cwd-based
    // task reads causes the daemon to read from the wrong task directory
    // (or "default") and miss pending tasks, preventing dispatch (see #1288).
    let in_progress_tasks =
        crate::tasks::get_in_progress_tasks_with_subjects_for_repo(&state.repo_name);
    let busy_coworkers: HashSet<String> = state.get_all_busy_coworkers().into_iter().collect();

    // Coworker → task assignments (for nudge/spawn loop prevention in dispatch)
    let coworker_task_assignments: HashMap<String, String> = {
        let assignments = state.coworker_task_assignments.lock().unwrap();
        assignments
            .iter()
            .map(|(coworker, assignment)| (coworker.clone(), assignment.task_id.clone()))
            .collect()
    };

    let all_tasks = crate::tasks::read_tasks_for_repo(Some(&state.repo_name));
    let pending_tasks_with_owners =
        crate::tasks::get_pending_tasks_with_owners_for_repo(&state.repo_name);
    let pending_tasks_without_owners =
        crate::tasks::get_pending_tasks_without_owners_for_repo(&state.repo_name);

    // Task-to-channel, task-to-model, task-to-plan, task-to-execution-skill, and channel-lead mappings
    let (
        task_channel,
        task_model_map,
        task_plan_map,
        task_execution_skill_map,
        channel_lead_sessions,
    ) = {
        let ps = state.persistent_state.lock().await;
        (
            ps.task_channel.clone(),
            ps.task_model.clone(),
            ps.task_plan.clone(),
            ps.task_execution_skill.clone(),
            ps.channel_lead_sessions.clone(),
        )
    };

    // ── PR / GitHub state ───────────────────────────────────────────────
    let coworkers_with_open_prs: HashSet<String> = super::pr::get_coworkers_with_open_prs(state)
        .into_iter()
        .collect();
    let coworkers_with_merged_prs: HashSet<String> =
        super::pr::get_coworkers_with_merged_prs(state);
    // Merged PR numbers are populated as a side effect of the above call.
    let merged_pr_numbers = super::pr::get_merged_pr_numbers(state);
    let (ci_passed_pr_coworkers, review_feedback_pr_coworkers, prs_needing_review, open_prs_data) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        (
            cache.ci_passed_pr_owners.clone(),
            cache.review_feedback_pr_owners.clone(),
            cache.prs_needing_review,
            cache.open_prs_data.clone(),
        )
    };

    // Derive task→PR mapping from open_prs_data PR titles for orphan recovery.
    // This catches tasks with open PRs even when pr_author_sessions is stale after restart.
    let github_open_pr_task_ids: HashMap<String, u64> = open_prs_data
        .iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?;
            let task_id = crate::tasks::extract_task_id_from_pr_title(title)?;
            Some((task_id.to_string(), number))
        })
        .collect();

    // Pending task owners: coworkers who have claimed a task (owner set) but haven't
    // started it yet (status=pending). These should be protected from idle shutdown.
    let pending_task_owners: HashSet<String> = pending_tasks_with_owners
        .iter()
        .map(|(_, _, owner)| owner.to_lowercase())
        .collect();

    // ── PR author sessions (task → PR mapping) ────────────────────────
    let (tasks_with_open_prs, pr_task_associations) = {
        let ps = state.persistent_state.lock().await;
        (ps.github.task_to_pr_map(), ps.github.pr_to_task_map())
    };

    // ── Reviewer state ──────────────────────────────────────────────────
    let (active_reviewers, reviewer_pr_assignments, reviewer_restart_counts) = {
        let ps = state.persistent_state.lock().await;
        let reviewers = compute_active_reviewers_with_health(&ps.github, &headless_process_health);
        // Build reviewer → PR assignments from persistent state so that dead
        // reviewers (absent from active_coworkers) are still included.
        // This is required for decide_dead_reviewer_respawns to detect and
        // respawn reviewers whose processes have exited without posting a review.
        let assignments = build_reviewer_pr_assignments(&ps.github);
        // Collect PR → restart_count for stuck reviewer backoff
        let restart_counts: HashMap<u64, u32> = ps
            .github
            .pr_reviewers
            .iter()
            .filter(|(_, a)| a.restart_count > 0)
            .map(|(pr, a)| (*pr, a.restart_count))
            .collect();
        (reviewers, assignments, restart_counts)
    };

    // ── Reviewing-phase coworkers (defense-in-depth idle-shutdown guard) ─
    let reviewing_phase_coworkers: HashSet<String> = {
        let records = state.coworker_records.read().await;
        records
            .iter()
            .filter(|(_, rec)| {
                matches!(
                    rec.workflow_phase,
                    Some(crate::coworker_state::WorkflowPhase::Reviewing)
                )
            })
            .map(|(name, _)| name.to_lowercase())
            .collect()
    };

    // ── Reviewer escalation tracking ──────────────────────────────────
    let reviewer_escalations_posted: HashSet<u64> = {
        let posted = state.reviewer_escalations_posted.lock().unwrap();
        posted.clone()
    };

    // ── Orphaned PR lead nudge deduplication ──────────────────────────
    let orphaned_pr_lead_nudges_sent: HashSet<u64> = {
        let sent = state.orphaned_pr_lead_nudges_sent.lock().unwrap();
        sent.clone()
    };

    // Get all reviewed PRs from persistent state (not just assigned ones)
    // This ensures orphaned PRs (those without active reviewers/tasks) are included
    let reviewed_prs = {
        let ps = state.persistent_state.lock().await;
        ps.github.reviewed_prs.clone()
    };

    // Collect placeholder comment IDs for assigned PRs that haven't been reviewed yet.
    // Uses a cache with 120-second TTL for negative results to minimize API calls.
    // Positive results are kept until the reviewer completes (cache entry removed elsewhere).
    const PLACEHOLDER_CACHE_TTL_SECS: u64 = 120;
    let reviewer_in_progress_comment_ids: HashMap<u64, u64> = {
        let assigned_unreviewed_prs: Vec<u64> = reviewer_pr_assignments
            .values()
            .copied()
            .filter(|pr| !reviewed_prs.contains(pr))
            .collect();

        let mut result = HashMap::new();
        for pr_number in assigned_unreviewed_prs {
            // Check cache first
            let cached = {
                let cache = state.reviewer_placeholder_cache.lock().unwrap();
                cache.get(&pr_number).copied()
            };

            let comment_id = match cached {
                Some((id, checked_at))
                    if checked_at.elapsed().as_secs() < PLACEHOLDER_CACHE_TTL_SECS =>
                {
                    id // Use cached result within TTL
                }
                _ => {
                    // Cache miss or expired: fetch from GitHub
                    let id = crate::daemon::pr::pr_in_progress_placeholder_comment_id(pr_number);
                    {
                        let mut cache = state.reviewer_placeholder_cache.lock().unwrap();
                        cache.insert(pr_number, (id, std::time::Instant::now()));
                    }
                    id
                }
            };

            if let Some(id) = comment_id {
                result.insert(pr_number, id);
            }
        }
        result
    };

    // ── GitHub rate limit ────────────────────────────────────────────────
    let github_rate_limit = {
        let ps = state.persistent_state.lock().await;
        ps.github.rate_limit.clone()
    };

    // ── Dependency state ──────────────────────────────────────────────────
    let coworkers_with_unblocked_deps =
        crate::tasks::get_coworkers_with_unblocked_dependents_for_repo(&state.repo_name);

    // ── Usage limit state ────────────────────────────────────────────────
    let (usage_limit_nudge_scheduled, usage_limit_nudge_at) = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        (nudge_at.is_some(), *nudge_at)
    };
    let now_utc = Utc::now();

    // Derive usage limit and API error sets from headless process health.
    // These were previously detected from pane content; now read from structured flags.
    let usage_limited_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(_, health)| health.has_usage_limit)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // Auth errors take precedence over both usage limits and API errors, since they
    // require user intervention and won't resolve with time or retries.
    let auth_error_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(_, health)| health.has_auth_error)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let api_error_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(name, health)| {
            // Only flag API error if not already auth error or usage limit
            health.has_api_error
                && !auth_error_coworkers.contains(&name.to_lowercase())
                && !usage_limited_coworkers.contains(&name.to_lowercase())
        })
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let tool_name_conflict_coworkers: HashSet<String> = headless_process_health
        .iter()
        .filter(|(_, health)| health.has_tool_name_conflict)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    let max_pending_api_call_exemption = chrono::Duration::minutes(20);
    let coworkers_with_active_tools: HashSet<String> = headless_process_health
        .iter()
        .filter(|(name, health)| {
            let pending_api_turn_fresh = health.has_pending_api_call
                && health
                    .last_event_at
                    .or_else(|| coworker_start_times.get(&name.to_lowercase()).copied())
                    .is_some_and(|t| {
                        now_utc.signed_duration_since(t) < max_pending_api_call_exemption
                    });
            health.has_pending_tool || health.has_running_subagent || pending_api_turn_fresh
        })
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // ── Channel state ──────────────────────────────────────────────────
    let archived_channels: HashSet<String> = {
        let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);
        crate::channel::Channel::list_archived(&base_dir)
            .unwrap_or_default()
            .into_iter()
            .collect()
    };

    // These debug fields are NOT populated during tick collection (hot path).
    // They are only populated on-demand via `with_debug_context()` when
    // capturing a snapshot for debugging (e.g., `midtown e2e capture`).
    let channel_messages = Vec::new();
    let daemon_logs = Vec::new();

    // ── Worktree registry ────────────────────────────────────────────────
    #[allow(clippy::type_complexity)]
    let (
        tasks_with_worktrees,
        task_worktree_map,
        worktree_branch_owners,
        merged_pr_branches,
        worktree_registry,
    ): (
        HashSet<String>,
        HashMap<String, String>,
        HashMap<String, String>,
        HashMap<u64, String>,
        crate::worktree_registry::WorktreeRegistry,
    ) = {
        let ps = state.persistent_state.lock().await;
        let mut task_ids = HashSet::new();
        let mut wt_map = HashMap::new();
        let mut branch_owners = HashMap::new();
        let mut pr_branches = HashMap::new();

        for (_, assignment) in ps.worktree_registry.all_assignments().iter() {
            // Collect task IDs and task→worktree mapping
            if let Some(ref task_id) = assignment.task_id {
                task_ids.insert(task_id.clone());
                wt_map.insert(task_id.clone(), assignment.worktree_id.clone());
            }

            // Collect branch→coworker mapping for task-based branches
            if let Some(ref coworker) = assignment.current_coworker {
                branch_owners.insert(assignment.branch_name.clone(), coworker.clone());
            }

            // Build PR → branch mapping for merged PRs (used by cleanup effects)
            if let Some(pr_num) = assignment.pr_number {
                pr_branches.insert(pr_num, assignment.branch_name.clone());
            }
        }

        let worktree_registry = ps.worktree_registry.clone();

        (
            task_ids,
            wt_map,
            branch_owners,
            pr_branches,
            worktree_registry,
        )
    };

    // ── Lead session refresh interval ──────────────────────────────────
    let lead_session_refresh_interval_secs = {
        let cfg = crate::config::get_project_daemon_config(&state.repo_name);
        std::env::var("MIDTOWN_LEAD_SESSION_REFRESH_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(cfg.lead_session_refresh_interval_secs)
            .unwrap_or(crate::daemon::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS)
    };

    // ── Limits & timing ─────────────────────────────────────────────────
    let channel_lead_names: std::collections::HashSet<String> =
        channel_lead_sessions.keys().cloned().collect();
    let is_at_coworker_limit = state.is_at_coworker_limit(&channel_lead_names);
    let is_at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
    let repo_name = state.repo_name.clone();
    let default_channel = state.channel_router.default_channel_name().to_string();
    let repo_owner = state.repo_owner.clone();

    // ── Dispatch cooldown state ──────────────────────────────────────────
    // Pre-evaluate cooldown checks so decision functions (dispatch_via_sessions,
    // check_and_recover_orphans) stay pure — no locking during evaluation phase.
    let (
        orphan_spawn_cooldown_active,
        session_dispatch_cooldown_active,
        spawn_failure_cooldown_names,
    ) = {
        let cooldowns = state.cooldowns.lock().unwrap();
        let orphan_active = !cooldowns.check(
            "orphan_spawn",
            "global",
            crate::daemon::constants::ORPHAN_SPAWN_COOLDOWN,
        );
        let session_active = !cooldowns.check(
            "session_dispatch",
            "global",
            crate::daemon::constants::SESSION_DISPATCH_COOLDOWN,
        );
        // Collect all coworker names that are on the spawn failure cooldown.
        // We check against every known coworker name (active + sessions).
        let all_names: HashSet<String> = active_coworkers
            .iter()
            .map(|cw| cw.name.to_lowercase())
            .collect();
        let on_cooldown: HashSet<String> = all_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "spawn_failure",
                    name,
                    crate::daemon::constants::SPAWN_FAILURE_COOLDOWN,
                )
            })
            .cloned()
            .collect();
        (orphan_active, session_active, on_cooldown)
    };

    // ── Session-centric fields ───────────────────────────────────────────
    let (sessions, session_task_map, session_name_map, name_session_map) = {
        let persistent = state.persistent_state.lock().await;
        let sessions = persistent.sessions.clone();
        let mut session_task_map: HashMap<String, String> = HashMap::new();
        let mut session_name_map: HashMap<String, String> = HashMap::new();
        let mut name_session_map: HashMap<String, String> = HashMap::new();
        for (session_id, record) in &sessions {
            if let Some(task_id) = &record.task_id {
                session_task_map.insert(task_id.clone(), session_id.clone());
            }
            if let Some(name) = &record.current_name {
                session_name_map.insert(session_id.clone(), name.clone());
                name_session_map.insert(name.clone(), session_id.clone());
            }
        }
        (
            sessions,
            session_task_map,
            session_name_map,
            name_session_map,
        )
    };

    // ── Per-session recovery cooldown ────────────────────────────────────
    // Build the set of session_ids for which a recovery was recently attempted.
    // Uses the "session_recovered" cooldown category (per-session-id key) set in
    // on_success of SpawnCoworkerWithCallbacks by dispatch_via_sessions.
    // This prevents re-recovery spam when a session dies quickly after recovery.
    let recently_recovered_session_ids: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        sessions
            .keys()
            .filter(|sid| {
                !cooldowns.check(
                    "session_recovered",
                    sid,
                    crate::daemon::constants::SESSION_RECOVERED_COOLDOWN,
                )
            })
            .cloned()
            .collect()
    };

    // ── Pre-evaluate stale working directories ──────────────────────────
    // Check which sessions have a non-empty working_dir that no longer exists
    // on disk. This moves the filesystem I/O out of decision functions so
    // dispatch_via_sessions can remain pure.
    let stale_working_dir_sessions: HashSet<String> = sessions
        .iter()
        .filter(|(_, record)| {
            !record.working_dir.is_empty() && !std::path::Path::new(&record.working_dir).exists()
        })
        .map(|(session_id, _)| session_id.clone())
        .collect();

    // ── Auth profile pool ─────────────────────────────────────────────────
    // Snapshot the session→profile mapping so pure decision functions
    // (check_for_usage_limits) can emit MarkProfileLimited effects without
    // accessing DaemonState directly.
    let session_profile_map: HashMap<String, String> = {
        let map = state.session_profile_map.lock().unwrap();
        map.clone()
    };

    // Snapshot the set of pool profiles currently marked is_usage_limited so
    // maybe_nudge_usage_limit_expiry() can clear them directly from persistent
    // state on expiry — without depending on session_profile_map entries that
    // disappear when coworkers stop.
    let limited_pool_profiles: HashSet<String> = {
        let ps = state.persistent_state.lock().await;
        ps.profile_pool_state
            .iter()
            .filter(|(_, s)| s.is_usage_limited)
            .map(|(email, _)| email.clone())
            .collect()
    };

    let snapshot = WorldSnapshot {
        active_coworkers,
        running_coworkers,
        coworker_snapshots,
        active_names,
        active_session_ids,
        session_name,
        coworker_start_times,
        coworker_stop_times,
        headless_process_health,
        attached_coworkers,
        in_progress_tasks,
        busy_coworkers,
        coworker_task_assignments,
        all_tasks,
        pending_tasks_with_owners,
        pending_tasks_without_owners,
        task_channel,
        task_model_map,
        task_plan_map,
        task_execution_skill_map,
        channel_lead_sessions,
        coworkers_with_open_prs,
        coworkers_with_merged_prs,
        merged_pr_numbers,
        ci_passed_pr_coworkers,
        review_feedback_pr_coworkers,
        open_prs_data,
        github_open_pr_task_ids,
        pending_task_owners,
        tasks_with_open_prs,
        pr_task_associations,
        active_reviewers,
        reviewing_phase_coworkers,
        reviewer_pr_assignments,
        reviewer_in_progress_comment_ids,
        reviewed_prs,
        prs_needing_review,
        reviewer_restart_counts,
        reviewer_escalations_posted,
        orphaned_pr_lead_nudges_sent,
        github_rate_limit,
        freshly_fetched_rate_limit: None,
        coworkers_with_unblocked_deps,
        usage_limit_nudge_scheduled,
        usage_limit_nudge_at,
        usage_limited_coworkers,
        api_error_coworkers,
        auth_error_coworkers,
        tool_name_conflict_coworkers,
        coworkers_with_active_tools,
        archived_channels,
        channel_messages,
        daemon_logs,
        tasks_with_worktrees,
        task_worktree_map,
        worktree_registry,
        worktree_branch_owners,
        merged_pr_branches,
        lead_session_refresh_interval_secs,
        is_at_coworker_limit,
        is_at_dev_limit,
        now_utc,
        repo_name,
        default_channel,
        repo_owner,
        sessions,
        session_task_map,
        session_name_map,
        name_session_map,
        orphan_spawn_cooldown_active,
        session_dispatch_cooldown_active,
        spawn_failure_cooldown_names,
        recently_recovered_session_ids,
        stale_working_dir_sessions,
        session_profile_map,
        limited_pool_profiles,
    };

    // Log full snapshot at trace level for debugging and test case generation
    if tracing::enabled!(tracing::Level::TRACE)
        && let Ok(json) = serde_json::to_string_pretty(&snapshot)
    {
        tracing::trace!(snapshot = %json, "world snapshot collected");
    }

    snapshot
}

/// Test helper: Creates a minimal WorldSnapshot for unit tests with all fields
/// set to empty/default values. Tests can override specific fields as needed.
#[cfg(test)]
pub(super) fn minimal_snapshot_for_test() -> WorldSnapshot {
    WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashMap::new(),
        coworker_task_assignments: HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewing_phase_coworkers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewer_in_progress_comment_ids: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        orphaned_pr_lead_nudges_sent: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        coworkers_with_active_tools: HashSet::new(),
        archived_channels: HashSet::new(),
        channel_messages: vec![],
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: HashSet::new(),
        recently_recovered_session_ids: HashSet::new(),
        stale_working_dir_sessions: HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    }
}

/// Extra seconds beyond the assignment timeout that an alive reviewer is still protected.
///
/// Both `SessionMonitorTick` and `PrPollTick` fire every 30 seconds. At the 600-second
/// expiry boundary, if the idle check fires just before the poll tick refreshes the
/// assignment timestamp, the reviewer loses protection. A 30-second grace window covers
/// this race without promoting truly stale assignments (e.g., from a session that ended
/// long before a new session with the same name was spawned).
const REVIEWER_ALIVE_GRACE_SECS: u64 = 30;

/// Compute the active reviewers set, augmented with alive-but-recently-expired reviewers.
///
/// `active_reviewers()` only returns reviewers whose assignment is within the
/// 600-second timeout window. However, there is a race condition between
/// `SessionMonitorTick` (idle shutdown, every 30s) and `PrPollTick` (which refreshes
/// assignment timestamps, also every 30s): when both fire at T=600s, if the idle
/// check fires first, a still-running reviewer loses their protection.
///
/// This function adds a secondary protection: if a coworker's process is alive
/// in `process_health` AND their assignment is within the extended window
/// (`PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + REVIEWER_ALIVE_GRACE_SECS`), they are
/// included in the result. The time bound prevents truly stale historical assignments
/// (from a previous session with the same name) from providing false protection.
pub(crate) fn compute_active_reviewers_with_health(
    github: &crate::github_state::GitHubState,
    process_health: &HashMap<String, ProcessHealth>,
) -> std::collections::HashSet<String> {
    let mut reviewers = github.active_reviewers();
    for (name, health) in process_health {
        if health.is_alive && github.reviewer_has_recent_assignment(name, REVIEWER_ALIVE_GRACE_SECS)
        {
            reviewers.insert(name.clone());
        }
    }
    reviewers
}

/// Build the reviewer → PR number assignment map from persistent GitHub state.
///
/// This reads from `pr_reviewers` directly rather than filtering through
/// `active_coworkers`, so that dead reviewers (whose processes have exited)
/// are still included in the map. This is required for
/// `decide_dead_reviewer_respawns` to detect and respawn reviewers that died
/// before posting their review.
///
/// Intentionally does NOT apply the `PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS` filter.
/// A dead reviewer's assignment may expire before the respawn logic fires;
/// filtering it out here would make the reviewer invisible to
/// `decide_dead_reviewer_respawns` and cause the review to be permanently lost.
/// Use `active_reviewers()` when you need the timeout-filtered list for display
/// or logging.
///
/// When a reviewer has multiple entries in `pr_reviewers` (e.g., a stale assignment
/// from one PR and a fresh assignment for another), the most recently assigned entry
/// is kept to ensure the map is deterministic regardless of `HashMap` iteration order.
pub(crate) fn build_reviewer_pr_assignments(
    github: &crate::github_state::GitHubState,
) -> HashMap<String, u64> {
    let mut result: HashMap<String, (u64, chrono::DateTime<chrono::Utc>)> = HashMap::new();
    for (&pr_number, assignment) in &github.pr_reviewers {
        let is_newer = result
            .get(&assignment.reviewer)
            .is_none_or(|(_, existing_at)| assignment.assigned_at > *existing_at);
        if is_newer {
            result.insert(
                assignment.reviewer.clone(),
                (pr_number, assignment.assigned_at),
            );
        }
    }
    result
        .into_iter()
        .map(|(reviewer, (pr, _))| (reviewer, pr))
        .collect()
}

#[path = "snapshot_tests.rs"]
#[cfg(test)]
mod tests;
