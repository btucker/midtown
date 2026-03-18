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
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
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

/// Default value for `default_branch` when deserializing from older snapshots.
fn default_branch_name() -> String {
    "main".to_string()
}

/// Default value for `lead_session_refresh_interval_secs` when deserializing from older snapshots.
fn default_lead_refresh_interval() -> u64 {
    crate::daemon::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS
}

fn default_max_in_progress_tasks() -> usize {
    crate::daemon::constants::DEFAULT_MAX_IN_PROGRESS_TASKS
}

/// Number of recent channel messages to include in WorldSnapshot captures.
const SNAPSHOT_CHANNEL_MESSAGE_COUNT: usize = 50;

/// Number of recent daemon log lines to include in WorldSnapshot captures.
const SNAPSHOT_DAEMON_LOG_LINES: usize = 100;

// ─── Nested state structs ──────────────────────────────────────────────
//
// WorldSnapshot groups its 65+ fields into domain-specific nested structs.
// Each nested struct uses `#[serde(flatten)]` so JSON serialization stays
// flat (backwards-compatible with existing test fixtures).

/// Coworker identity, lifecycle, and presence state.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotCoworkerState {
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
    /// Session name (e.g., "midtown-projectname").
    pub session_name: String,
    /// Coworker start times keyed by lowercase name.
    pub coworker_start_times: HashMap<String, DateTime<Utc>>,
    /// Coworker stop times keyed by lowercase name.
    /// Tracks when coworkers were sent on a break (shutdown). Used by workflow
    /// features that need to know the last activity time of inactive coworkers.
    pub coworker_stop_times: HashMap<String, DateTime<Utc>>,
    /// Coworkers currently in "attached" state, mapped to their attach timestamp.
    ///
    /// Entries are added (with current time) on attach, removed on detach.
    /// Must be excluded from stuck detection and orphan recovery.
    /// The timestamp enables auto-detach of stale entries when the interactive
    /// session ends without a proper `midtown agent detach`.
    pub attached_coworkers: HashMap<String, chrono::DateTime<chrono::Utc>>,
}

/// PR and GitHub state — open PRs, merge tracking, CI status, rate limits.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotPrState {
    /// PR numbers of recently merged PRs. Used by task dispatch to skip
    /// tasks that reference a merged PR (e.g., "Address review feedback on PR #709").
    pub merged_pr_numbers: HashSet<u64>,
    /// Open PR data (from last GitHub poll). Used by orphan PR reconciliation.
    /// Pre-collected during snapshot so decision logic doesn't need to lock pr_poll_data.
    #[serde(default)]
    pub open_prs_data: Vec<serde_json::Value>,
    /// Unified PR↔task index from both SessionRecord and GitHub PR title sources.
    /// Replaces the former `github_open_pr_task_ids`, `tasks_with_open_prs`, and
    /// `pr_task_associations` fields.
    #[serde(flatten)]
    pub pr_task_index: PrTaskIndex,
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
}

/// Reviewer tracking — assignments, escalations, review status.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotReviewerState {
    /// Currently active reviewers (from both in-memory tracker and persistent state).
    pub active_reviewers: HashSet<String>,
    /// Reviewer → assigned PR number mapping (from task_session_spans).
    pub reviewer_pr_assignments: HashMap<String, u64>,
    /// Placeholder comment IDs for PRs with an unupdated "Review in progress" comment.
    /// Maps PR number → GitHub comment database ID.
    /// Pre-collected during snapshot (cached to minimize API calls).
    #[serde(default, deserialize_with = "u64_key_map::deserialize")]
    pub reviewer_in_progress_comment_ids: HashMap<u64, u64>,
    /// PRs that have been verified as reviewed (Claude review comment exists).
    /// Pre-collected during snapshot so decision logic doesn't need API calls.
    pub reviewed_prs: HashSet<u64>,
    /// Count of open PRs that need review (not draft, no Claude review, no formal review).
    /// Used by task dispatch to prioritize reviews over new task pickup.
    pub prs_needing_review: usize,
    /// PR number → restart count for reviewer assignments.
    /// Used by stuck reviewer detection to implement backoff.
    #[serde(deserialize_with = "u64_key_map::deserialize")]
    pub reviewer_restart_counts: HashMap<u64, u32>,
    /// PR numbers for which a reviewer escalation warning has already been posted.
    /// Prevents the escalation warning from firing every tick after max restarts.
    pub reviewer_escalations_posted: HashSet<u64>,
}

/// Health monitoring — process health, usage limits, error states.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotHealthState {
    /// Health state of headless coworker processes, keyed by coworker name.
    /// Replaces pane scraping: stuck detection uses `last_event_at`,
    /// usage limits and API errors use structured flags set from stream events.
    pub headless_process_health: HashMap<String, ProcessHealth>,
    /// Whether a usage-limit nudge is already scheduled.
    pub usage_limit_nudge_scheduled: bool,
    /// The scheduled usage-limit nudge time (if any).
    #[serde(skip)]
    pub usage_limit_nudge_at: Option<tokio::time::Instant>,
    /// Coworkers currently at a usage limit (derived from `headless_process_health`).
    /// These coworkers should be excluded from stuck detection, idle warnings,
    /// and task assignment until the limit expires.
    pub usage_limited_coworkers: HashSet<String>,
    /// Coworkers currently experiencing API errors (derived from `headless_process_health`).
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
}

/// Immutable snapshot of the daemon's world, collected once per tick.
///
/// Each field is owned data — no references back to `DaemonState`. This means
/// evaluation functions that take `&WorldSnapshot` cannot accidentally trigger
/// side effects on the underlying state.
///
/// Fields are organized into domain-specific nested structs:
/// - [`SnapshotCoworkerState`]: coworker identity, lifecycle, presence
/// - [`SnapshotPrState`]: PR/GitHub data, merge tracking, rate limits
/// - [`SnapshotReviewerState`]: reviewer assignments, escalations, review status
/// - [`SnapshotHealthState`]: process health, usage limits, error states
///
/// Nested structs use `#[serde(flatten)]` so the JSON representation stays flat
/// (backwards-compatible with existing test fixtures and serializable for debugging).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct WorldSnapshot {
    // ── Coworker state ──────────────────────────────────────────────────
    #[serde(flatten)]
    pub coworkers: SnapshotCoworkerState,

    // ── PR / GitHub state ───────────────────────────────────────────────
    #[serde(flatten)]
    pub pr: SnapshotPrState,

    // ── Reviewer state ──────────────────────────────────────────────────
    #[serde(flatten)]
    pub reviewer: SnapshotReviewerState,

    // ── Health / monitoring state ────────────────────────────────────────
    #[serde(flatten)]
    pub health: SnapshotHealthState,

    // ── Task state ──────────────────────────────────────────────────────
    /// In-progress tasks: `(task_id, subject, owner)`.
    pub in_progress_tasks: Vec<(String, String, String)>,
    /// Names of coworkers who are busy (have in-progress tasks), lowercase.
    pub busy_coworkers: HashSet<String>,
    /// Coworker → task assignment mapping (from daemon in-memory tracking).
    /// Maps coworker name (lowercase) → task_id. Used by task dispatch to prevent
    /// re-assigning the same task to the same coworker (nudge/spawn loop prevention).
    #[serde(default, alias = "coworker_task_assignments")]
    pub name_task_assignments: HashMap<String, String>,
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
    /// Task-to-thread-ID mapping for workflow events.
    /// Maps task ID → thread ID. Used by dispatch.rs to include thread context
    /// in TaskAssigned/TaskCompleted workflow events without I/O.
    #[serde(default)]
    pub task_thread_id_map: HashMap<String, String>,
    /// Task-to-message-ID mapping for workflow events.
    /// Maps task ID → announcement message ID. Used by dispatch.rs to include
    /// message context in TaskAssigned/TaskCompleted workflow events without I/O.
    #[serde(default)]
    pub task_message_id_map: HashMap<String, String>,
    /// Task-to-parent mapping for UI grouping of related tasks.
    /// Maps child task ID → parent task ID. Used for displaying task hierarchies.
    #[serde(default)]
    pub task_parent_map: HashMap<String, String>,
    /// Inverted blocking graph: task_id → list of task_ids it unblocks.
    /// Built from `Task.blocked_by` during snapshot collection.
    /// Used by dispatch priority to identify tasks that unblock other work.
    #[serde(default)]
    pub blocks_map: HashMap<String, Vec<String>>,
    /// Task-to-agent-type mapping for specialized task dispatch.
    /// Maps task ID → agent type name (e.g., "midtown-code-reviewer").
    /// Used by dispatch.rs to spawn tasks with the correct agent definition.
    #[serde(default)]
    pub task_agent_type_map: HashMap<String, String>,
    /// Channel lead session mapping for nudge routing.
    /// Maps channel name → session ID. Used by effects.rs to deliver
    /// `NudgeChannelLead` effects without locking persistent state.
    #[serde(default)]
    pub channel_lead_sessions: HashMap<String, String>,
    /// Pre-computed set of channel lead names (keys of `channel_lead_sessions`).
    /// Avoids re-allocating a HashSet on every call to `channel_lead_names()`.
    #[serde(default)]
    pub channel_lead_names: HashSet<String>,

    // ── Dependency state ──────────────────────────────────────────────────
    /// Coworkers whose completed tasks have unblocked pending follow-ups.
    pub coworkers_with_unblocked_deps: HashSet<String>,
    /// Coworkers who have pending tasks assigned to them (task.owner set, status=pending).
    /// Provides defense-in-depth idle shutdown protection alongside `busy_coworkers`
    /// (in-memory assignment tracking). Both paths are checked to prevent the
    /// spawn→idle→break loop (see PR #650).
    pub pending_task_owners: HashSet<String>,

    // ── Channel state ──────────────────────────────────────────────────
    /// Channels operating in lead-driven mode.
    /// When a channel is in this set, the daemon relays workflow events as
    /// human-readable @mentions to the channel lead instead of executing its
    /// built-in state machine (auto-dispatch, reviewer spawning, PR nudges).
    #[serde(default)]
    pub lead_driven_channels: HashSet<String>,
    /// Channels that have already been archived (`.archived.jsonl` exists).
    /// Used by the UI + command handlers to keep archived channels hidden by default
    /// and to prevent duplicate channel-lead recovery for archived topics.
    #[serde(default)]
    pub archived_channels: HashSet<String>,
    /// Stale channel notes: maps channel name → list of stale note names.
    /// Empty by default; populated in `run_tick()` only for `NoteReviewTick` (hourly).
    #[serde(default)]
    pub stale_channel_notes: HashMap<String, Vec<String>>,
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
    /// PR number → branch name mapping from the worktree registry for merged PRs.
    /// Used by `collect_merged_pr_cleanup_effects()` to generate cleanup effects without I/O.
    #[serde(deserialize_with = "u64_key_map::deserialize")]
    pub merged_pr_branches: HashMap<u64, String>,

    // ── Lead session refresh ─────────────────────────────────────────────
    /// Interval for periodic lead session refresh in seconds (0 = disabled).
    /// From daemon config — available to pure decision functions.
    #[serde(default = "default_lead_refresh_interval")]
    pub lead_session_refresh_interval_secs: u64,

    // ── Limits & timing ─────────────────────────────────────────────────
    /// Whether the daemon is at the in-progress task limit.
    #[serde(default)]
    pub is_at_task_limit: bool,
    /// Maximum in-progress tasks (from config). Available to pure decision functions
    /// for per-spawn limit checks in the dispatch loop.
    #[serde(default = "default_max_in_progress_tasks")]
    pub max_in_progress_tasks: usize,
    /// Current wall-clock time.
    pub now_utc: DateTime<Utc>,
    /// Filesystem directory key (e.g., "midtown.nosync"). Used for path construction,
    /// config lookups, and task storage operations.
    #[serde(alias = "repo_name")]
    pub dir_key: String,
    /// Logical project name (e.g., "midtown"). Used for lead identity checks,
    /// channel routing, session naming, and display.
    #[serde(default)]
    pub project_name: String,
    /// Default channel name (e.g., "midtown"). Used by pure decision functions
    /// to construct `NudgeChannelLead` effects that route to the project lead.
    #[serde(default)]
    pub default_channel: String,
    /// Repository owner (from git remote URL). Used by pure decision functions
    /// to determine if a PR is authored by the lead (repo owner).
    #[serde(default)]
    pub repo_owner: Option<String>,
    /// Default branch name (e.g., "main" or "master"). Used by pure decision
    /// functions to construct nudge messages with the correct branch reference.
    #[serde(default = "default_branch_name")]
    pub default_branch: String,

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
    /// Channels that have been recently nudged about stale notes.
    /// Pre-evaluated from `state.cooldowns` so `check_for_stale_notes` stays pure.
    #[serde(default)]
    pub note_staleness_cooldown_channels: HashSet<String>,

    /// Coworkers recently nudged to rebase after a PR merge.
    /// Pre-evaluated from `state.cooldowns` (category `"merge_rebase_nudge"`)
    /// so `collect_merge_rebase_nudge_effects` stays pure.
    #[serde(default)]
    pub merge_rebase_nudge_cooldown_names: HashSet<String>,

    /// Merged PR numbers that have already triggered rebase nudges.
    /// Pre-evaluated from `state.cooldowns` (category `"merge_rebase_pr_processed"`)
    /// so `collect_merge_rebase_nudge_effects` only nudges for newly merged PRs.
    #[serde(default)]
    pub rebase_nudge_processed_prs: HashSet<u64>,

    /// Coworkers recently warned about post-rebase regressions.
    /// Pre-evaluated from `state.cooldowns` (category `"rebase_regression"`)
    /// so `check_for_rebase_regressions` can skip already-warned coworkers.
    #[serde(default)]
    pub rebase_regression_cooldown_names: HashSet<String>,

    /// Channel lead worktrees that are behind `origin/main`.
    /// Populated during snapshot collection using `git merge-base --is-ancestor`
    /// to check if each channel lead's worktree is behind `origin/<default_branch>`.
    /// Results are cached to avoid running git fetch on every tick.
    #[serde(default)]
    pub stale_channel_lead_worktrees: HashSet<String>,

    /// Channels recently nudged about stale worktrees.
    /// Pre-evaluated from `state.cooldowns` (category `"lead_worktree_freshness"`)
    /// so `check_channel_lead_worktree_freshness` stays pure.
    #[serde(default)]
    pub lead_worktree_freshness_cooldown_channels: HashSet<String>,

    /// Task IDs that have a pending `AssignAndSpawn` or `SpawnCoworkerWithCallbacks`
    /// effect from a previous tick that hasn't completed yet.
    /// Pre-evaluated from `DaemonState::in_flight_task_spawns` so decision functions stay pure.
    #[serde(default)]
    pub in_flight_task_spawns: HashSet<String>,

    /// Task IDs (formatted as `"pending-{task_id}"`) that are on the nudge cooldown.
    /// Pre-evaluated from `state.cooldowns` (category `"task_nudge"`) so decision functions stay pure.
    #[serde(default)]
    pub task_nudge_cooldown_ids: HashSet<String>,

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

    // ── Fork / topic session state ─────────────────────────────────────
    /// Thread-bound fork sessions: maps `thread_parent_id → session_id`.
    ///
    /// Populated from `DaemonState::topic_sessions` during snapshot collection.
    /// Used by `decide_dead_fork_respawns()` to detect fork sessions whose
    /// processes have died and need respawning.
    #[serde(default)]
    pub topic_sessions: HashMap<String, String>,

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

    // ── Dispatch pre-filters ──────────────────────────────────────────────
    /// Tasks that should not be spawned/recovered due to PR status or completion.
    ///
    /// Pre-computed from `all_tasks` during snapshot collection. Checks (in order):
    /// 1. Task is already completed → always protected
    /// 2. Task has a merged PR (via `tasks_with_open_prs` or `task.pr`) → always
    ///    protected regardless of session state (prevents recovery-loops)
    /// 3. Task owner has no active session → not protected by open PRs (allows
    ///    dispatch of pending tasks or tasks whose owner went away)
    /// 4. Task has an open PR via `tasks_with_open_prs` → protected
    /// 5. Task has an open PR detected from GitHub PR titles (`github_open_pr_task_ids`)
    ///
    /// Dispatch checks `pr_protected_tasks.contains(&task_id)` instead of calling
    /// `is_task_pr_protected` at each site.
    #[serde(default)]
    pub pr_protected_tasks: HashSet<String>,
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
        for (name, health) in &self.health.headless_process_health {
            if let Some(session_id) = self.name_session_map.get(name) {
                map.insert(session_id.clone(), health);
            }
        }
        map
    }

    /// Returns the pre-computed set of active channel lead names.
    pub fn channel_lead_names(&self) -> &HashSet<String> {
        &self.channel_lead_names
    }

    /// Backward-compatibility fixup for snapshots serialized before the
    /// `repo_name` → `dir_key` + `project_name` split. Old JSON has a single
    /// `repo_name` field which serde maps to `dir_key` via `#[serde(alias)]`.
    /// `project_name` defaults to `""` — this method copies `dir_key` into it.
    /// Only used in test fixtures — gated to `#[cfg(test)]`.
    #[cfg(test)]
    pub fn fixup_legacy_fields(&mut self) {
        if self.project_name.is_empty() && !self.dir_key.is_empty() {
            self.project_name = self.dir_key.clone();
        }
    }

    /// Look up the topic channel for a PR via its associated task.
    ///
    /// Chains `pr_task_index` (PR# → task_id) and `task_channel`
    /// (task_id → channel name). Analogous to `PrContext::get_channel()`
    /// on the async side — use this in synchronous decision functions that
    /// operate on the snapshot.
    pub fn channel_for_pr(&self, pr_number: u64) -> Option<String> {
        let task_id = self.pr.pr_task_index.task_for_pr(pr_number)?;
        self.task_channel.get(task_id).cloned()
    }

    /// Look up the topic channel for a PR, falling back to the project name.
    pub fn channel_for_pr_or_default(&self, pr_number: u64) -> String {
        self.channel_for_pr(pr_number)
            .unwrap_or_else(|| self.project_name.clone())
    }

    /// Look up the session record for a task, if one exists.
    ///
    /// Chains `session_task_map` (task_id → session_id) and `sessions`
    /// (session_id → record). Returns `None` if no session is associated
    /// with this task or if the session_id is stale.
    ///
    /// This replaces the repeated `snap.session_task_map.get(id)? → snap.sessions.get(sid)?`
    /// chain that appeared in 5+ locations across dispatch.rs and pr.rs.
    pub fn find_session_for_task(
        &self,
        task_id: &str,
    ) -> Option<&crate::daemon::state::SessionRecord> {
        let session_id = self.session_task_map.get(task_id)?;
        self.sessions.get(session_id)
    }

    /// Check whether a worktree is bound to a different ACTIVE coworker.
    ///
    /// Returns `Some(bound_coworker_name)` if the worktree's registered coworker
    /// is active and differs from `intended_coworker`. Returns `None` if safe to
    /// proceed (no collision).
    pub fn worktree_collision(&self, worktree_id: &str, intended_coworker: &str) -> Option<String> {
        let assignment = self.worktree_registry.get(worktree_id)?;
        let bound_coworker = assignment.current_coworker.as_deref()?;

        if bound_coworker.eq_ignore_ascii_case(intended_coworker) {
            return None;
        }

        let bound_lower = bound_coworker.to_lowercase();
        if self.coworkers.active_names.contains(&bound_lower) {
            return Some(bound_coworker.to_string());
        }

        None
    }

    /// Get coworker names that have sessions with open PRs.
    ///
    /// Derived from `SessionRecord.pr_number` cross-referenced with `open_prs_data`.
    /// Replaces the legacy `PrCoworkerCache.open_pr_owners` which derived ownership
    /// from branch names.
    pub fn sessions_with_open_prs(&self) -> HashSet<String> {
        let open_pr_numbers: HashSet<u64> = self
            .pr
            .open_prs_data
            .iter()
            .filter_map(|pr| pr["number"].as_u64())
            .collect();

        self.sessions
            .values()
            .filter(|s| s.pr_number.is_some_and(|pr| open_pr_numbers.contains(&pr)))
            .filter_map(|s| s.current_name.clone().or_else(|| s.preferred_name.clone()))
            .collect()
    }

    /// Get coworker names that have sessions with recently merged PRs.
    ///
    /// Derived from `SessionRecord.pr_number` cross-referenced with `merged_pr_numbers`.
    /// Replaces the legacy `PrCoworkerCache.merged_pr_owners` which derived ownership
    /// from branch names.
    pub fn sessions_with_merged_prs(&self) -> HashSet<String> {
        self.sessions
            .values()
            .filter(|s| {
                s.pr_number
                    .is_some_and(|pr| self.pr.merged_pr_numbers.contains(&pr))
            })
            .filter_map(|s| s.current_name.clone().or_else(|| s.preferred_name.clone()))
            .collect()
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
    let session_name = format!("midtown-{}", state.project_name);

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
    // but state.paths.dir_key() is set correctly at startup. Using cwd-based
    // task reads causes the daemon to read from the wrong task directory
    // (or "default") and miss pending tasks, preventing dispatch (see #1288).
    //
    // Read the task list ONCE and derive all views from it. Multiple
    // independent reads create TOCTOU races where concurrent writers
    // (Lead/coworker processes) can modify a task file between reads,
    // causing the same task to appear in both in_progress_tasks AND
    // pending_tasks_without_owners — bypassing the dispatch exclusion set
    // and leading to duplicate task assignment.
    let all_tasks = crate::tasks::read_tasks_for_repo(Some(state.paths.dir_key()));
    let in_progress_tasks: Vec<(String, String, String)> = all_tasks
        .iter()
        .filter(|t| t.status == crate::tasks::TaskStatus::InProgress)
        .map(|t| {
            (
                t.id.clone(),
                t.subject.clone(),
                t.owner.clone().unwrap_or_default(),
            )
        })
        .collect();
    let pending_tasks_with_owners: Vec<(String, String, String)> = all_tasks
        .iter()
        .filter(|t| t.status == crate::tasks::TaskStatus::Pending && t.owner.is_some())
        .map(|t| {
            (
                t.id.clone(),
                t.subject.clone(),
                t.owner.clone().unwrap_or_default(),
            )
        })
        .collect();
    let pending_tasks_without_owners =
        crate::tasks::filter_pending_tasks_without_owners(&all_tasks, 45);
    let mut blocks_map: HashMap<String, Vec<String>> = HashMap::new();
    for task in all_tasks
        .iter()
        .filter(|t| t.status != crate::tasks::TaskStatus::Completed)
    {
        for blocker_id in &task.blocked_by {
            blocks_map
                .entry(blocker_id.clone())
                .or_default()
                .push(task.id.clone());
        }
    }
    // Derive busy coworkers from sessions: any session with a task_id
    // where the task is in_progress is considered busy.
    let in_progress_task_ids: HashSet<&str> = in_progress_tasks
        .iter()
        .map(|(id, _, _)| id.as_str())
        .collect();
    let busy_coworkers: HashSet<String> = {
        let ps = state.persistent_state.lock().await;
        ps.sessions
            .values()
            .filter(|s| {
                s.task_id
                    .as_deref()
                    .is_some_and(|tid| in_progress_task_ids.contains(tid))
            })
            .filter_map(|s| s.current_name.clone())
            .map(|n| n.to_lowercase())
            .collect()
    };

    // Coworker → task assignments, derived from sessions[].task_id.
    // The single source of truth for which coworker is assigned to which task.
    let name_task_assignments: HashMap<String, String> = state.get_name_task_assignments().await;

    // Task-to-channel, task-to-model, task-to-plan, task-to-execution-skill,
    // task-to-thread, task-to-message, task-to-parent, task-to-agent-type,
    // channel-lead, and lead-driven mappings
    let (
        task_channel,
        task_model_map,
        task_plan_map,
        task_execution_skill_map,
        task_thread_id_map,
        task_message_id_map,
        task_parent_map,
        task_agent_type_map,
        channel_lead_sessions,
        lead_driven_channels,
    ) = {
        let ps = state.persistent_state.lock().await;
        (
            ps.task_channel.clone(),
            ps.task_model.clone(),
            ps.task_plan.clone(),
            ps.task_execution_skill.clone(),
            ps.task_thread_id.clone(),
            ps.task_message_id.clone(),
            ps.task_parent.clone(),
            ps.task_agent_type.clone(),
            ps.channel_lead_sessions.clone(),
            ps.lead_driven_channels.clone(),
        )
    };

    // ── PR / GitHub state ───────────────────────────────────────────────
    // Fetch merged PR data (uses cooldown-based caching internally).
    let (merged_pr_numbers, _merged_prs_data) = super::pr::fetch_merged_pr_data(state);
    let (prs_needing_review, open_prs_data) = {
        let cache = state.pr_poll_data.read().unwrap();
        (cache.prs_needing_review, cache.open_prs_data.clone())
    };

    // Derive task→PR mapping from open_prs_data PR titles for orphan recovery.
    // This catches tasks with open PRs even when SessionRecord data is stale after restart.
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

    // ── PR↔task index (unified from SessionRecord + GitHub PR titles) ──
    let pr_task_index = {
        let ps = state.persistent_state.lock().await;
        let session_task_to_pr = super::state::task_to_pr_map_from_sessions(&ps.sessions);
        let pr_to_task = super::state::pr_to_task_map_from_sessions(&ps.sessions);
        PrTaskIndex::new(session_task_to_pr, github_open_pr_task_ids, pr_to_task)
    };

    // ── Reviewer state ──────────────────────────────────────────────────
    let (active_reviewers, reviewer_pr_assignments, reviewer_restart_counts) = {
        let ps = state.persistent_state.lock().await;
        let reviewers = compute_active_reviewers_from_spans(&ps, &headless_process_health);
        // Build reviewer → PR assignments from task_session_spans so that dead
        // reviewers (absent from active_coworkers) are still included.
        // This is required for decide_dead_reviewer_respawns to detect and
        // respawn reviewers whose processes have exited without posting a review.
        let assignments = build_reviewer_pr_assignments_from_spans(&ps);
        // Collect PR → restart_count for stuck reviewer backoff (from task_restart_count).
        let restart_counts: HashMap<u64, u32> = ps
            .task_restart_count
            .iter()
            .filter_map(|(task_id, &count)| ps.task_pr_number.get(task_id).map(|&pr| (pr, count)))
            .collect();
        (reviewers, assignments, restart_counts)
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
    //
    // Read from task_placeholder_comment_id (task-centric), mapped to PR numbers via
    // task_pr_number. Falls back to a three-tier lookup for PRs without a task entry:
    // 1. Check the in-memory TTL cache (120s for both positive and negative results)
    // 2. Fall back to API lookup via `pr_in_progress_placeholder_comment_id()`
    const PLACEHOLDER_CACHE_TTL_SECS: u64 = 120;
    let reviewer_in_progress_comment_ids: HashMap<u64, u64> = {
        let assigned_unreviewed_prs: Vec<u64> = reviewer_pr_assignments
            .values()
            .copied()
            .filter(|pr| !reviewed_prs.contains(pr))
            .collect();

        // Pre-fetch stored placeholder IDs from persistent state (single lock acquisition).
        // Read from task_placeholder_comment_id keyed by task_id, mapped to pr via task_pr_number.
        let stored_placeholder_ids: HashMap<u64, Option<u64>> = {
            let ps = state.persistent_state.lock().await;
            assigned_unreviewed_prs
                .iter()
                .map(|&pr| {
                    // Find a task_id for this PR via task_pr_number reverse lookup
                    let id = ps
                        .task_pr_number
                        .iter()
                        .find(|&(_, &p)| p == pr)
                        .and_then(|(task_id, _)| ps.task_placeholder_comment_id.get(task_id))
                        .copied();
                    (pr, id)
                })
                .collect()
        };

        let mut result = HashMap::new();
        for pr_number in assigned_unreviewed_prs {
            // Tier 1: Check stored placeholder_comment_id from task_placeholder_comment_id
            if let Some(Some(stored_id)) = stored_placeholder_ids.get(&pr_number) {
                result.insert(pr_number, *stored_id);
                continue;
            }

            // Tier 2: Check in-memory cache
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
                    // Tier 3: Cache miss or expired — fetch from GitHub
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
    // Task deps change rarely; cache the result for 30s to avoid recomputing
    // on every TaskDispatchTick (~5s).
    let coworkers_with_unblocked_deps = {
        let cached = {
            let cache = state.coworkers_with_unblocked_deps_cache.lock().unwrap();
            if let Some((timestamp, ref result)) = *cache
                && timestamp.elapsed().as_secs() < UNBLOCKED_DEPS_CACHE_SECS
            {
                Some(result.clone())
            } else {
                None
            }
        };
        match cached {
            Some(result) => result,
            None => {
                let result =
                    crate::tasks::get_coworkers_with_unblocked_dependents_from_tasks(&all_tasks);
                let mut cache = state.coworkers_with_unblocked_deps_cache.lock().unwrap();
                *cache = Some((std::time::Instant::now(), result.clone()));
                result
            }
        }
    };

    // ── Usage limit state ────────────────────────────────────────────────
    let (usage_limit_nudge_scheduled, usage_limit_nudge_at) = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        (nudge_at.is_some(), *nudge_at)
    };
    let now_utc = Utc::now();

    // Derive usage limit and error sets from headless process health.
    // The 4 flag-only sets are cached across ticks via a generation counter —
    // they only change when `headless_health` is written (~1s), not on every
    // snapshot tick (~5s). `coworkers_with_active_tools` is always recomputed
    // because its freshness check depends on wall-clock time.
    let health_generation = state
        .headless_health_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    let cached_health = {
        let cache = state.health_derived_cache.lock().unwrap();
        match *cache {
            Some((cached_gen, ref sets)) if cached_gen == health_generation => Some(sets.clone()),
            _ => None,
        }
    };
    let health_sets = cached_health.unwrap_or_else(|| {
        let sets = compute_health_sets(&headless_process_health);
        let mut cache = state.health_derived_cache.lock().unwrap();
        *cache = Some((health_generation, sets.clone()));
        sets
    });
    let CachedHealthSets {
        usage_limited_coworkers,
        auth_error_coworkers,
        api_error_coworkers,
        tool_name_conflict_coworkers,
    } = health_sets;

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
    let base_dir = state.paths.base_dir().to_path_buf();
    let archived_channels: HashSet<String> = {
        crate::channel::Channel::list_archived(&base_dir)
            .unwrap_or_default()
            .into_iter()
            .collect()
    };

    // Stale channel notes: populated in run_tick() only for NoteReviewTick (hourly).
    let stale_channel_notes = HashMap::new();

    // These debug fields are NOT populated during tick collection (hot path).
    // They are only populated on-demand via `with_debug_context()` when
    // capturing a snapshot for debugging (e.g., `midtown e2e capture`).
    let channel_messages = Vec::new();
    let daemon_logs = Vec::new();

    // ── Worktree registry ────────────────────────────────────────────────
    let (tasks_with_worktrees, task_worktree_map, merged_pr_branches, worktree_registry): (
        HashSet<String>,
        HashMap<String, String>,
        HashMap<u64, String>,
        crate::worktree_registry::WorktreeRegistry,
    ) = {
        let ps = state.persistent_state.lock().await;
        let mut task_ids = HashSet::new();
        let mut wt_map = HashMap::new();
        let mut pr_branches = HashMap::new();

        for (_, assignment) in ps.worktree_registry.all_assignments().iter() {
            // Collect task IDs and task→worktree mapping
            if let Some(ref task_id) = assignment.task_id {
                task_ids.insert(task_id.clone());
                wt_map.insert(task_id.clone(), assignment.worktree_id.clone());
            }

            // Build PR → branch mapping for merged PRs (used by cleanup effects)
            if let Some(pr_num) = assignment.pr_number {
                pr_branches.insert(pr_num, assignment.branch_name.clone());
            }
        }

        let worktree_registry = ps.worktree_registry.clone();

        (task_ids, wt_map, pr_branches, worktree_registry)
    };

    // ── Lead session refresh interval ──────────────────────────────────
    let lead_session_refresh_interval_secs = {
        let cfg = crate::config::get_project_daemon_config(state.paths.dir_key());
        std::env::var("MIDTOWN_LEAD_SESSION_REFRESH_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(cfg.lead_session_refresh_interval_secs)
            .unwrap_or(crate::daemon::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS)
    };

    // ── Pre-computed derived sets ──────────────────────────────────────
    let channel_lead_names: HashSet<String> = channel_lead_sessions.keys().cloned().collect();

    // ── Limits & timing ─────────────────────────────────────────────────
    // Only count in_progress tasks with active owners toward the limit.
    // Tasks whose owners are dead (e.g., after a restart) don't consume
    // coworker slots and should not block new spawns.
    let active_in_progress_count = in_progress_tasks
        .iter()
        .filter(|(_, _, owner)| owner.is_empty() || active_names.contains(owner))
        .count();
    let is_at_task_limit = active_in_progress_count >= state.max_in_progress_tasks;
    let dir_key = state.paths.dir_key().to_string();
    let project_name = state.project_name.clone();
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
        // Check ALL pool names (AVENUE_NAMES + OVERFLOW_NAMES), not just active
        // coworkers. A failed spawn never makes the coworker "active", so checking
        // only active_coworkers would miss cooldowns for freshly-allocated names
        // that failed to spawn (e.g., unowned task dispatch).
        let all_pool_names = crate::coworker::AVENUE_NAMES
            .iter()
            .chain(crate::coworker::OVERFLOW_NAMES.iter());
        let on_cooldown: HashSet<String> = all_pool_names
            .filter(|name| {
                !cooldowns.check(
                    "spawn_failure",
                    &name.to_lowercase(),
                    crate::daemon::constants::SPAWN_FAILURE_COOLDOWN,
                )
            })
            .map(|name| name.to_lowercase())
            .collect();
        (orphan_active, session_active, on_cooldown)
    };

    // Pre-evaluate merge-rebase nudge cooldowns for all active coworkers.
    // Checked against all active names; decision functions filter to open-PR coworkers.
    let merge_rebase_nudge_cooldown_names: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        active_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "merge_rebase_nudge",
                    name,
                    crate::daemon::constants::MERGE_REBASE_NUDGE_COOLDOWN,
                )
            })
            .cloned()
            .collect()
    };

    // Pre-evaluate which merged PR numbers have already triggered rebase nudges.
    // Prevents the infinite rebase loop: without this, `gh pr list --state merged`
    // returns the same 10 PRs every fetch, and coworkers get re-nudged after each
    // cooldown expiry for merges they already rebased onto.
    let rebase_nudge_processed_prs: HashSet<u64> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        merged_pr_numbers
            .iter()
            .filter(|pr_num| {
                !cooldowns.check(
                    "merge_rebase_pr_processed",
                    &pr_num.to_string(),
                    crate::daemon::constants::MERGE_REBASE_PR_PROCESSED_COOLDOWN,
                )
            })
            .copied()
            .collect()
    };

    // Pre-evaluate rebase regression cooldowns for all active coworkers.
    // Checked against all active names; decision functions filter to open-PR coworkers.
    let rebase_regression_cooldown_names: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        active_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "rebase_regression",
                    name,
                    crate::daemon::constants::REBASE_REGRESSION_COOLDOWN,
                )
            })
            .cloned()
            .collect()
    };

    // Pre-evaluate note staleness cooldowns for all channels with leads
    let note_staleness_cooldown_channels: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        channel_lead_sessions
            .keys()
            .filter(|ch| {
                !cooldowns.check(
                    "note_staleness",
                    ch,
                    std::time::Duration::from_secs(
                        crate::daemon::constants::NOTE_STALENESS_NUDGE_COOLDOWN_SECS,
                    ),
                )
            })
            .cloned()
            .collect()
    };

    // Pre-evaluate lead worktree freshness cooldowns for all channels with leads
    let lead_worktree_freshness_cooldown_channels: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        channel_lead_sessions
            .keys()
            .filter(|ch| {
                !cooldowns.check(
                    "lead_worktree_freshness",
                    ch,
                    crate::daemon::constants::LEAD_WORKTREE_FRESHNESS_COOLDOWN,
                )
            })
            .cloned()
            .collect()
    };

    // ── Fork / topic sessions ───────────────────────────────────────────
    let topic_sessions: HashMap<String, String> = {
        let ts = state.topic_sessions.lock().unwrap();
        ts.iter()
            .filter(|(_, sid)| sid.as_str() != "pending")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
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
    // on_success of SpawnForTask by dispatch_via_sessions.
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

    // ── In-flight task spawns ──────────────────────────────────────────
    // Snapshot the set of task IDs with pending spawn effects so
    // dispatch_owned_pending_tasks stays pure (no DaemonState access).
    let in_flight_task_spawns: HashSet<String> =
        state.in_flight_task_spawns.lock().unwrap().clone();

    // ── Per-task nudge cooldowns ─────────────────────────────────────
    // Pre-evaluate nudge cooldowns for all pending tasks with owners so
    // decide_pending_task_action can stay pure.
    let task_nudge_cooldown_ids: HashSet<String> = {
        let cooldowns = state.cooldowns.lock().unwrap();
        pending_tasks_with_owners
            .iter()
            .filter(|(task_id, _, _)| {
                let key = format!("pending-{}", task_id);
                !cooldowns.check("task_nudge", &key, std::time::Duration::from_secs(300))
            })
            .map(|(task_id, _, _)| task_id.clone())
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

    // ── Channel lead worktree staleness ─────────────────────────────────
    // Check if channel lead worktrees are behind origin/<default_branch>.
    // Uses merge-base ancestry check; results are cached for ~25s.
    let stale_channel_lead_worktrees: HashSet<String> =
        collect_stale_channel_lead_worktrees(state, &channel_lead_sessions, &sessions).await;

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

    // ── Dispatch pre-filters ──────────────────────────────────────────────
    // Pre-compute which tasks should be skipped during dispatch/recovery due
    // to PR status or task completion. This moves the per-task filtering out
    // of dispatch decision functions so they can use a simple HashSet::contains.
    let pr_protected_tasks: HashSet<String> = all_tasks
        .iter()
        .filter(|task| {
            super::dispatch::is_task_pr_protected(
                task,
                &merged_pr_numbers,
                &pr_task_index,
                &active_names,
            )
        })
        .map(|task| task.id.clone())
        .collect();

    let snapshot = WorldSnapshot {
        coworkers: SnapshotCoworkerState {
            active_coworkers,
            running_coworkers,
            coworker_snapshots,
            active_names,
            active_session_ids,
            session_name,
            coworker_start_times,
            coworker_stop_times,
            attached_coworkers,
        },
        pr: SnapshotPrState {
            merged_pr_numbers,
            open_prs_data,
            pr_task_index,
            orphaned_pr_lead_nudges_sent,
            github_rate_limit,
            freshly_fetched_rate_limit: None,
        },
        reviewer: SnapshotReviewerState {
            active_reviewers,
            reviewer_pr_assignments,
            reviewer_in_progress_comment_ids,
            reviewed_prs,
            prs_needing_review,
            reviewer_restart_counts,
            reviewer_escalations_posted,
        },
        health: SnapshotHealthState {
            headless_process_health,
            usage_limit_nudge_scheduled,
            usage_limit_nudge_at,
            usage_limited_coworkers,
            api_error_coworkers,
            auth_error_coworkers,
            tool_name_conflict_coworkers,
            coworkers_with_active_tools,
        },
        in_progress_tasks,
        busy_coworkers,
        name_task_assignments,
        all_tasks,
        pending_tasks_with_owners,
        pending_tasks_without_owners,
        pending_task_owners,
        task_channel,
        task_model_map,
        task_plan_map,
        task_execution_skill_map,
        task_thread_id_map,
        task_message_id_map,
        task_parent_map,
        task_agent_type_map,
        channel_lead_sessions,
        channel_lead_names,
        lead_driven_channels,
        coworkers_with_unblocked_deps,
        archived_channels,
        stale_channel_notes,
        channel_messages,
        daemon_logs,
        tasks_with_worktrees,
        task_worktree_map,
        worktree_registry,
        merged_pr_branches,
        lead_session_refresh_interval_secs,
        is_at_task_limit,
        max_in_progress_tasks: state.max_in_progress_tasks,
        blocks_map,
        now_utc,
        dir_key,
        project_name,
        default_channel,
        default_branch: state.default_branch.clone(),
        repo_owner,
        topic_sessions,
        sessions,
        session_task_map,
        session_name_map,
        name_session_map,
        orphan_spawn_cooldown_active,
        session_dispatch_cooldown_active,
        spawn_failure_cooldown_names,
        note_staleness_cooldown_channels,
        merge_rebase_nudge_cooldown_names,
        rebase_nudge_processed_prs,
        rebase_regression_cooldown_names,
        stale_channel_lead_worktrees,
        lead_worktree_freshness_cooldown_channels,
        in_flight_task_spawns,
        task_nudge_cooldown_ids,
        recently_recovered_session_ids,
        stale_working_dir_sessions,
        session_profile_map,
        limited_pool_profiles,
        pr_protected_tasks,
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
        coworkers: SnapshotCoworkerState {
            session_name: "test".to_string(),
            ..Default::default()
        },
        pr: SnapshotPrState::default(),
        reviewer: SnapshotReviewerState::default(),
        health: SnapshotHealthState::default(),
        name_task_assignments: HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        pending_task_owners: HashSet::new(),
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        archived_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
        channel_messages: vec![],
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        lead_session_refresh_interval_secs: 5400,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: HashSet::new(),
        stale_working_dir_sessions: HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
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
pub(crate) fn compute_active_reviewers_from_spans(
    ps: &crate::daemon::state::DaemonPersistentState,
    process_health: &HashMap<String, ProcessHealth>,
) -> HashSet<String> {
    let mut reviewers = HashSet::new();
    for span in ps.active_reviewer_spans() {
        let is_running = ps
            .sessions
            .get(&span.session_id)
            .map(|s| s.is_running)
            .unwrap_or(false);
        let is_alive = process_health
            .get(&span.agent_name)
            .map(|h| h.is_alive)
            .unwrap_or(false);
        if is_running || is_alive {
            reviewers.insert(span.agent_name.clone());
        }
    }
    reviewers
}

/// Build the reviewer → PR number assignment map from task_session_spans.
///
/// Reads from open reviewer spans (end_time = None, agent_type = "reviewer"),
/// mapping agent_name → PR number via task_pr_number. Dead reviewers (whose
/// process has exited but whose span is still open) are included so that
/// `decide_dead_reviewer_respawns` can detect and respawn them.
pub(crate) fn build_reviewer_pr_assignments_from_spans(
    ps: &crate::daemon::state::DaemonPersistentState,
) -> HashMap<String, u64> {
    let mut assignments = HashMap::new();
    for span in ps.active_reviewer_spans() {
        if let Some(&pr) = ps.task_pr_number.get(&span.task_id) {
            assignments.insert(span.agent_name.clone(), pr);
        }
    }
    assignments
}

/// How long to cache the coworkers-with-unblocked-deps result.
/// Task dependency relationships change rarely; 30s staleness is acceptable
/// because this set is only used for idle shutdown protection.
const UNBLOCKED_DEPS_CACHE_SECS: u64 = 30;

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
async fn collect_stale_channel_lead_worktrees(
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
