//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod cache;
mod chat;
mod constants;
mod dispatch;
pub(crate) mod dispatch_priority;
pub(crate) mod effects;
pub(crate) mod events;
mod gh;
mod health;
pub mod helpers;
mod migration;
pub(crate) mod plugin_daemon;
mod pr;
pub(crate) mod profile_pool;
mod rpc;
mod rpc_auth;
mod rpc_channel;
mod rpc_coworker;
mod rpc_headless;
mod rpc_prs;
mod rpc_read_state;
mod rpc_reminder;
mod rpc_session;
mod rpc_status;
mod rpc_task;
mod rpc_workflow;
pub(crate) mod session_events;
#[path = "session_events_tests.rs"]
#[cfg(test)]
mod session_events_tests;
pub(crate) mod sessions;
#[allow(dead_code)]
pub(crate) mod sidecar;
pub mod snapshot;
mod startup;
pub(crate) mod state;
mod stream;
pub(crate) mod tick;
mod trackers;
pub(crate) mod wake_reason;
mod webhook_fwd;

use constants::*;
pub use constants::{
    DEFAULT_MAX_IN_PROGRESS_TASKS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, PR_NUDGE_COOLDOWN_SECS,
};
pub use state::DaemonPersistentState;
pub use trackers::{
    CommentTracker, PrIssueTracker, PrIssueType, StuckConditionTracker, StuckConditionType,
};

// Test helper for orphan recovery tests
#[doc(hidden)]
pub use dispatch::should_recover_task_test_helper;

#[doc(hidden)]
pub use effects::Effect;

// Test helpers for E2E tests.
// Pure functions that take &DaemonPersistentState + &[Task] and return Vec<Effect>.
#[doc(hidden)]
pub use dispatch::{
    auto_close_completed_tasks, build_subject_based_completion_effects,
    check_for_duplicate_task_workers, reset_orphaned_tasks,
};
#[doc(hidden)]
pub use events::DaemonEvent;
#[doc(hidden)]
pub use health::{
    check_and_restart_dead_reviewers, check_and_restart_tool_name_conflicts,
    check_for_usage_limits, detect_stale_attached_sessions, ensure_lead_alive,
    maybe_nudge_usage_limit_expiry,
};
#[doc(hidden)]
pub use pr::{
    collect_merge_rebase_nudge_effects, collect_merged_pr_cleanup_effects, reconcile_orphaned_prs,
};
#[doc(hidden)]
pub use state::SessionRecord;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, broadcast, watch};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::config;
use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::web::{self, WebUpdate};
use crate::webhook::{WebhookConfig, start_webhook_server};
use crate::worktree::WorktreeManager;

fn dm_mirror_agent_names(
    sessions: &HashMap<String, state::SessionRecord>,
    channel_lead_sessions: &HashMap<String, String>,
    project_name: &str,
) -> HashSet<String> {
    sessions
        .values()
        .filter(|r| !r.name.is_empty())
        .filter(|r| {
            r.name.as_str() != project_name
                && !channel_lead_sessions.contains_key(r.name.as_str())
                && !r.is_fork_session()
        })
        .map(|r| r.name.clone())
        .collect()
}

// Task assignments are tracked via `sessions[].task_id` on `SessionRecord` —
// no separate in-memory HashMap needed. See `get_task_id_for_coworker()` and
// `get_busy_coworker_names()` which derive this from persistent session state.

/// An in-memory record of a coworker's pending question (from AskUserQuestion tool).
///
/// Ephemeral — lost on daemon restart. Questions are cleared when the coworker
/// receives a nudge (i.e., when the Lead answers the question).
#[derive(Debug, Clone)]
pub(crate) struct PendingQuestion {
    /// Unique identifier for this question (monotonically increasing).
    pub id: u64,
    /// Name of the coworker waiting for an answer.
    pub coworker_name: String,
    /// The question text from the AskUserQuestion tool call.
    pub question: String,
    /// When the question was received.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Result of daemon execution — determines what happens after the event loop exits.
#[derive(Debug)]
pub enum DaemonExitStatus {
    /// Normal shutdown (SIGTERM/SIGINT). Process should exit.
    Shutdown,
    /// Exec-restart requested. Process should re-exec itself with the given args
    /// to preserve the original (unsandboxed) process context.
    ExecRestart {
        workdir: PathBuf,
        project_name: Option<String>,
    },
}

/// Configuration for the daemon server.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the Unix socket.
    pub socket_path: PathBuf,
    /// Path to the PID file for singleton enforcement.
    pub pid_file_path: PathBuf,
    /// Working directory for spawned coworkers.
    pub workdir: PathBuf,
    /// Enable verbose logging.
    pub verbose: bool,
    /// Port for the webhook server (None to disable).
    pub webhook_port: Option<u16>,
    /// GitHub webhook secret for signature verification.
    pub webhook_secret: Option<String>,
    /// Interval in seconds to restart webhook forwarder (for reliability).
    pub webhook_restart_interval_secs: u64,
    /// Interval in seconds to poll PRs for actionable issues.
    pub pr_poll_interval_secs: u64,
    /// Enable chat monitor for @mention routing. Default: true.
    pub chat_monitor_enabled: bool,
    /// Maximum number of in-progress tasks. Default: 8.
    pub max_in_progress_tasks: usize,
    /// Explicit project name (from --project flag). Overrides auto-detection.
    pub project_name: Option<String>,
    /// GitHub username for `gh` CLI authentication.
    /// When set, runs `gh auth switch --user <github_user>` at daemon startup.
    pub github_user: Option<String>,
    /// Interval in seconds for periodic lead session refresh (0 = disabled).
    pub lead_session_refresh_interval_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let project_name = crate::paths::detect_repo_name().unwrap_or_default();

        // Load config.toml for base daemon settings.
        // Merge: global daemon section < project daemon section < env vars
        let daemon_section = if project_name.is_empty() {
            crate::config::GlobalConfig::load().daemon
        } else {
            crate::config::get_project_daemon_config(&project_name)
        };

        // Webhook port: env var -> config.toml -> auto-assign per project
        // Note: MIDTOWN_WEBHOOK_PORT=0 disables webhook entirely
        let webhook_port = match std::env::var("MIDTOWN_WEBHOOK_PORT").ok() {
            Some(s) => s
                .parse()
                .ok()
                .map(|p: u16| if p == 0 { None } else { Some(p) })
                .unwrap_or(Some(DEFAULT_WEBHOOK_PORT)),
            None => match daemon_section.webhook_port {
                Some(0) => None,
                Some(p) => Some(p),
                None => {
                    // Auto-assign a unique port per project, or use default if no project
                    if project_name.is_empty() {
                        Some(DEFAULT_WEBHOOK_PORT)
                    } else {
                        Some(crate::config::assign_webhook_port(&project_name))
                    }
                }
            },
        };

        // Webhook secret: env var -> config.toml -> None
        let webhook_secret = std::env::var("MIDTOWN_WEBHOOK_SECRET")
            .ok()
            .or_else(|| daemon_section.webhook_secret.clone());

        // Restart interval: env var -> config.toml -> default
        let webhook_restart_interval_secs = std::env::var("MIDTOWN_WEBHOOK_RESTART_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(daemon_section.webhook_restart_interval_secs)
            .unwrap_or(DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS);

        // PR poll interval: env var -> config.toml -> default
        let pr_poll_interval_secs = std::env::var("MIDTOWN_PR_POLL_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(daemon_section.pr_poll_interval_secs)
            .unwrap_or(DEFAULT_PR_POLL_INTERVAL_SECS);

        // Chat monitor: env var -> config.toml -> true (enabled by default)
        let chat_monitor_enabled = match std::env::var("MIDTOWN_CHAT_MONITOR").ok() {
            Some(s) => s != "0",
            None => daemon_section.chat_monitor_enabled.unwrap_or(true),
        };

        // GitHub user: env var -> config.toml -> None
        let github_user = std::env::var("MIDTOWN_GITHUB_USER")
            .ok()
            .or_else(|| daemon_section.github_user.clone());

        // Lead session refresh interval: env var -> config.toml -> default
        let lead_session_refresh_interval_secs =
            std::env::var("MIDTOWN_LEAD_SESSION_REFRESH_INTERVAL")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(daemon_section.lead_session_refresh_interval_secs)
                .unwrap_or(crate::daemon::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS);

        // Max in-progress tasks: env var > deprecated env var > project config > global config > default (8)
        let max_in_progress_tasks = std::env::var("MIDTOWN_MAX_IN_PROGRESS_TASKS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                // Honor deprecated env var as migration fallback
                if let Ok(val) = std::env::var("MIDTOWN_MAX_COWORKERS") {
                    tracing::warn!(
                        "MIDTOWN_MAX_COWORKERS is deprecated. Use MIDTOWN_MAX_IN_PROGRESS_TASKS instead."
                    );
                    val.parse().ok()
                } else {
                    None
                }
            })
            .or_else(|| {
                if project_name.is_empty() {
                    crate::config::GlobalConfig::load()
                        .default
                        .max_in_progress_tasks()
                } else {
                    crate::config::get_project_config(&project_name).max_in_progress_tasks()
                }
            })
            .unwrap_or(DEFAULT_MAX_IN_PROGRESS_TASKS);

        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Ensure project config.toml exists (create minimal one if missing)
        if !project_name.is_empty() {
            let _ = crate::config::ensure_project_config(&project_name, &workdir);
        }

        Self {
            // Use repo-specific socket path to isolate daemons per project
            socket_path: crate::paths::daemon_socket(),
            // Use repo-specific PID file for singleton enforcement
            pid_file_path: crate::paths::daemon_pid_file(),
            workdir,
            verbose: false,
            webhook_port,
            webhook_secret,
            webhook_restart_interval_secs,
            pr_poll_interval_secs,
            chat_monitor_enabled,
            max_in_progress_tasks,
            project_name: None,
            github_user,
            lead_session_refresh_interval_secs,
        }
    }
}

/// Hardcoded list of required Claude Code plugins.
///
/// These plugins are essential for midtown's agents to function properly.
/// The daemon will automatically install any missing plugins on startup.
pub const REQUIRED_PLUGINS: &[&str] = &[
    "superpowers@claude-plugins-official",
    "code-review@claude-plugins-official",
    "pr-review-toolkit@claude-plugins-official",
    "commit-commands@claude-plugins-official",
    "feature-dev@claude-plugins-official",
    "explanatory-output-style@claude-plugins-official",
    "code-simplifier@claude-plugins-official",
];

/// Check that required Claude Code plugins are installed.
///
/// Logs warnings for any missing plugins but doesn't block daemon startup.
/// Actual installation is handled by `midtown start` (in the CLI).
async fn check_required_plugins() {
    if REQUIRED_PLUGINS.is_empty() {
        debug!("No required plugins configured");
        return;
    }

    // Get list of installed plugins
    let installed = match get_installed_plugins().await {
        Ok(plugins) => plugins,
        Err(e) => {
            warn!("Failed to check installed plugins: {}", e);
            return;
        }
    };

    // Find missing plugins
    let missing: Vec<_> = REQUIRED_PLUGINS
        .iter()
        .filter(|p| !installed.contains(**p))
        .collect();

    if missing.is_empty() {
        debug!("All required plugins are installed");
        return;
    }

    // Log missing plugins but don't try to install here
    // (installation should happen in `midtown start` for better UX)
    warn!(
        "Missing {} required plugins: {:?}. Run `midtown start` to install them.",
        missing.len(),
        missing
    );
}

/// Get list of installed plugin IDs.
async fn get_installed_plugins() -> Result<HashSet<String>, String> {
    let output = tokio::process::Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .await
        .map_err(|e| format!("Failed to run claude plugin list: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude plugin list failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output - it's an array of objects with "id" field
    let plugins: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse plugin list JSON: {}", e))?;

    let ids: HashSet<String> = plugins
        .iter()
        .filter_map(|p| p.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    Ok(ids)
}

/// Raw PR data from GitHub polling.
///
/// Stores only GitHub API response data — no coworker ownership inference.
/// PR ownership is derived from `SessionRecord.pr_number` at snapshot time.
#[derive(Default)]
struct PrPollData {
    /// PR numbers of recently merged PRs. Used by task dispatch to skip
    /// tasks that reference a PR that's already merged (e.g., "Address
    /// review feedback on PR #709" when PR #709 is merged).
    merged_pr_numbers: HashSet<u64>,
    /// Count of open PRs that need review (not draft, no completed review).
    /// Updated every PR poll tick. Used to prioritize PR reviews over new task pickup.
    prs_needing_review: usize,
    /// Full open PR data from the last poll, formatted for RPC responses.
    /// Cached to avoid re-fetching via gh CLI on every `midtown status` call.
    /// Updated every PR poll tick (~30s).
    open_prs_data: Vec<serde_json::Value>,
    /// Full merged PR data from the last poll, formatted for RPC responses.
    /// Cached to avoid re-fetching via gh CLI on every `midtown status` call.
    /// Updated every `MERGED_PRS_FETCH_INTERVAL_SECS` (5 minutes).
    merged_prs_data: Vec<serde_json::Value>,
    /// Whether the first PR poll has completed. Used to delay orphan worktree
    /// flagging until we have PR data - otherwise we'd incorrectly flag worktrees
    /// with open PRs during startup when the cache is empty.
    pr_poll_initialized: bool,
}

/// Shared daemon state.
pub(crate) struct DaemonState {
    coworkers: CoworkerManager,
    channel_router: crate::ChannelRouter,
    socket_path: PathBuf,
    /// Unified per-coworker records: session health, workflow phase, last activity.
    /// Replaces the separate `coworker_lifecycles` and `coworker_state_reports`
    /// maps. Entries are created on spawn and removed on shutdown.
    coworker_records: tokio::sync::RwLock<HashMap<String, crate::rules::CoworkerRecord>>,
    /// Tracker to avoid spamming the same PR issues
    pr_issue_tracker: Mutex<PrIssueTracker>,
    /// Logical project name (e.g., "midtown"). Used for channel names,
    /// session identity, display, team names.
    project_name: String,
    /// Consolidated project paths + filesystem dir_key.
    paths: crate::paths::ProjectPaths,
    /// Repository owner (extracted from git remote URL at startup).
    /// Used by pure decision functions to determine if a PR is authored by the lead.
    repo_owner: Option<String>,
    /// Default branch name (detected at startup, e.g. "main" or "master")
    default_branch: String,
    /// Paths to all repos in the project (primary + additional)
    all_repo_paths: Vec<PathBuf>,
    /// Unified cooldown tracker for orphan spawning and task nudge rate limiting.
    cooldowns: std::sync::Mutex<crate::rules::CooldownTracker>,
    /// Unified persistent state (GitHub + reminders), saved to daemon-state.json.
    persistent_state: Mutex<state::DaemonPersistentState>,
    /// Broadcast sender for pushing channel messages to WebSocket clients
    web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
    /// Maximum number of in-progress tasks
    max_in_progress_tasks: usize,
    /// Web Push notification manager for sending notifications to PWA clients
    /// (shared with the webserver to avoid race conditions on subscription storage)
    push_manager: Option<std::sync::Arc<crate::push::PushManager>>,
    /// Scheduled time to nudge all coworkers after a usage limit expires.
    /// When a coworker hits an API usage/rate limit, we parse the expiry and store it here.
    /// The main loop checks this and nudges everyone when the time arrives.
    usage_limit_nudge_at: Mutex<Option<tokio::time::Instant>>,
    /// Hash of the last PR poll response body, used to skip re-processing when data hasn't changed.
    /// This doesn't reduce API calls, but avoids redundant lock acquisition and issue detection
    /// when the PR state hasn't changed between poll cycles.
    last_pr_poll_hash: Mutex<u64>,
    /// Raw PR data from GitHub polling (open + merged PR data, no ownership inference).
    pr_poll_data: std::sync::RwLock<PrPollData>,
    /// Coworker stop times keyed by lowercase name.
    /// Tracks when coworkers were sent on a break (shutdown). Used by workflow
    /// features that need to know the last activity time of inactive coworkers.
    coworker_stop_times: std::sync::RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>,
    /// Tracks stuck conditions that warrant nudging the lead (no review, unresolved feedback, etc.)
    stuck_tracker: Mutex<StuckConditionTracker>,
    /// Buffer for batching CI check success notifications.
    /// Multiple checks passing on the same target within a short window are
    /// aggregated into a single channel message to reduce noise.
    ci_notification_buffer: Mutex<trackers::CiNotificationBuffer>,
    /// Cached GitHub repo full names (owner/repo) by repo path.
    /// Repo names never change during a daemon session, so we cache indefinitely.
    repo_name_cache: std::sync::RwLock<HashMap<PathBuf, String>>,
    /// User display name from config (e.g. "Ben"). Used to recognize user @mentions
    /// and identify user-sent messages when the display name differs from "user".
    user_display_name: Option<String>,
    /// Timestamp of the last received webhook event (monotonic).
    /// Used by the PR poll task to determine webhook health: if recent,
    /// polling uses a relaxed interval; if stale or absent, polling is aggressive.
    last_webhook_event_at: Mutex<Option<tokio::time::Instant>>,
    /// Task IDs with pending spawn effects that haven't completed yet.
    ///
    /// Prevents the task-level spawn race condition where two ticks both see the same
    /// pending task and generate duplicate `SpawnForTask` effects. The race occurs
    /// because:
    /// 1. Tick 1 evaluates, sees pending task, generates `SpawnForTask`
    /// 2. Effects start executing (disk write + spawn takes time)
    /// 3. Tick 2 fires, collects snapshot that still shows task as pending
    /// 4. Tick 2 generates another `SpawnForTask` for the same task
    ///
    /// Fix: After `evaluate_tick`, scan returned effects for `SpawnForTask` and
    /// add those task IDs here. In `spawn_for_pending_tasks`, skip tasks that are
    /// already in-flight. Clear entries when effects complete (success or failure).
    in_flight_task_spawns: std::sync::Mutex<HashSet<String>>,
    /// PR numbers with pending CreateReviewTask effects.
    ///
    /// Guards against duplicate review task creation across ticks (!2511).
    /// Populated from `extract_review_pr_numbers_from_effects` after
    /// `evaluate_tick`, cleared when the CreateReviewTask effect completes.
    in_flight_review_prs: std::sync::Mutex<HashSet<u64>>,
    // Task assignments are tracked via sessions[].task_id — no separate HashMap needed.
    /// Pending nudges sent to coworkers, awaiting confirmation of submission.
    ///
    /// Key: coworker name (lowercase), Value: (message text, sent timestamp).
    /// Used for attribution tracking when recovering stuck queued nudges.
    /// If the queued text matches a pending nudge, we auto-submit with Enter;
    /// if it doesn't match (user input), we leave it alone.
    pending_nudges: std::sync::Mutex<HashMap<String, (String, std::time::Instant)>>,
    /// Tracks comment counts per PR for polling-based detection of new review comments.
    ///
    /// When webhooks are degraded, this allows the polling path to detect new
    /// non-owner comments and nudge PR owners. Uses the same cooldown as webhooks
    /// (`PrIssueType::ReviewComment`) to avoid duplicate notifications.
    comment_tracker: Mutex<trackers::CommentTracker>,
    /// In-memory deduplication of reported insights.
    ///
    /// Stores hashes of normalized insight content to prevent the same insight
    /// from being posted to the channel multiple times. Resets on daemon restart,
    /// which is acceptable because transcript cursors prevent re-extraction.
    insight_hashes: std::sync::Mutex<HashSet<u64>>,
    /// PR numbers for which a reviewer escalation warning has been posted.
    ///
    /// Prevents the escalation warning (stuck reviewer after max restarts) from
    /// firing every tick. Once posted, the PR number is added here and the warning
    /// is not repeated. Resets on daemon restart, which is acceptable because
    /// reviewer assignments also reset.
    reviewer_escalations_posted: std::sync::Mutex<HashSet<u64>>,
    /// PR numbers for which the lead has already been nudged about an orphaned PR
    /// (reviewed + CI green, no active task). Prevents repeated nudges on every tick.
    ///
    /// Resets on daemon restart, which is acceptable — at worst the lead gets one
    /// extra nudge after a restart if the PR is still orphaned.
    orphaned_pr_lead_nudges_sent: std::sync::Mutex<HashSet<u64>>,
    /// In-memory deduplication for reviewer `[Review Note]` channel messages.
    ///
    /// Tracks (reviewer, PR number) → timestamp of first note. When a reviewer
    /// posts a `[Review Note]` for a PR they've already posted one for within the
    /// cooldown window (60s), subsequent notes are suppressed. After the cooldown,
    /// follow-up notes (e.g., corrections) are allowed through.
    ///
    /// Resets on daemon restart, which is acceptable because reviewers are spawned
    /// fresh for each review session — a restart mid-review would re-spawn the
    /// reviewer, who would post new notes from scratch.
    review_note_tracker: std::sync::Mutex<HashMap<(String, u64), std::time::Instant>>,
    /// Thread IDs with a fork creation in progress.
    ///
    /// Guards against concurrent fork creation for the same thread. An entry is
    /// inserted when `create_fork_session` begins and removed when the session is
    /// fully created (or creation fails). Replaces the old "pending" sentinel in
    /// the removed `topic_sessions` map.
    pub(crate) pending_forks: std::sync::Mutex<HashSet<String>>,
    /// Process health state for headless coworkers, keyed by coworker name.
    ///
    /// Populated by the session management layer from `HeadlessSession` stream events
    /// and process status. Read by `prepare_tick()` for the health decision
    /// functions.
    pub(crate) headless_health: std::sync::RwLock<HashMap<String, snapshot::ProcessHealth>>,
    /// Monotonically increasing counter, bumped each time `headless_health` is written.
    /// `prepare_tick()` compares this against the generation stored in
    /// `health_derived_cache` to decide whether to reuse cached sets or recompute.
    headless_health_generation: std::sync::atomic::AtomicU64,
    /// Cached health-derived sets (4 HashSets), valid for the generation in `.0`.
    /// Invalidated when `headless_health_generation` advances past the cached generation.
    #[allow(dead_code)]
    health_derived_cache: std::sync::Mutex<Option<(u64, snapshot::CachedHealthSets)>>,
    /// Coworkers currently in "attached" state (interactive session).
    ///
    /// When the Lead attaches to a headless coworker via `midtown agent attach`,
    /// the headless process is paused and replaced with an interactive session.
    /// During this period, the coworker must be exempt from stuck detection and
    /// orphan recovery. Entries are added on attach, removed on detach or via
    /// auto-detach (`Effect::AutoDetachCoworker`) after `ATTACH_TIMEOUT`.
    attached_coworkers: std::sync::Mutex<HashMap<String, chrono::DateTime<chrono::Utc>>>,
    /// Manages running headless coworker sessions.
    ///
    /// Owns the child processes and provides spawn/nudge/shutdown primitives.
    /// Used by `spawn_coworker()` and effect handlers for coworker lifecycle.
    pub(crate) session_manager: sessions::SessionManager,
    /// Response cache for RPC idempotency.
    ///
    /// Caches responses by request ID for 60 seconds to prevent duplicate execution
    /// when clients retry after timeouts. This transforms RPC from "at-least-once"
    /// to "exactly-once" semantics.
    rpc_response_cache:
        Mutex<HashMap<crate::rpc::RequestId, (crate::rpc::Response, std::time::Instant)>>,
    /// Cached result of channel lead worktree freshness checks.
    ///
    /// The freshness check runs `git fetch` + `git rev-parse` which is expensive.
    /// Since `prepare_tick()` is called for both `SessionMonitorTick` (~30s)
    /// and `TaskDispatchTick` (~5s), we cache the result for 25s to avoid running
    /// git fetch on every tick.
    worktree_freshness_cache: std::sync::Mutex<Option<(std::time::Instant, HashSet<String>)>>,
    /// Cached set of coworkers whose completed tasks have unblocked pending follow-ups.
    /// Task dependency relationships change rarely; 30s staleness is acceptable
    /// because this set is only used for idle shutdown protection.
    #[allow(dead_code)]
    coworkers_with_unblocked_deps_cache:
        std::sync::Mutex<Option<(std::time::Instant, HashSet<String>)>>,
    /// PR data cache with 60s TTL for the `prs.status` RPC.
    ///
    /// Stores the PR GraphQL response (open PRs, merged PRs, repos) keyed by a
    /// hash of repo paths. Coworker state is served separately via `coworkers.status`
    /// at 1-2s poll intervals (no cache needed).
    pub(crate) prs_cache: rpc_prs::PrsCache,
    /// Draining mode flag - when true, daemon stops assigning new tasks to coworkers.
    ///
    /// Set via `coworker.stop_all` RPC handler before sending SIGTERM to coworkers,
    /// preventing new task assignments during the SIGTERM wait window.
    draining: std::sync::atomic::AtomicBool,
    /// Exec-restart requested flag — when set, the daemon re-execs itself after
    /// graceful shutdown instead of exiting. This preserves the original (unsandboxed)
    /// process context across restarts, avoiding sandbox-exec nesting failures.
    restart_requested: std::sync::atomic::AtomicBool,
    /// Broadcast sender for triggering daemon shutdown from RPC handlers.
    ///
    /// The main event loop subscribes to this channel. When an RPC handler
    /// (e.g., `daemon.exec-restart`) needs to trigger shutdown, it sends on
    /// this channel to break the main loop.
    shutdown_tx: broadcast::Sender<()>,
    /// Pre-formatted tool activity headers per agent, keyed by lowercase agent name.
    ///
    /// Populated from `tool_data` ToolBlocks on `PostToChannel` effects.
    /// Each entry is a display string like "✓ read foo.rs" or "› $ git status".
    /// Cleared when the agent posts a non-tool channel message (work phase done),
    /// and when a coworker session stops (via `cleanup_coworker_state`).
    /// Used by `coworkers.status` RPC for the TUI tool activity indicator.
    pub(crate) tool_activity_headers: std::sync::RwLock<HashMap<String, Vec<String>>>,
    /// Negative cache for `is_pr_reviewed`: PR numbers confirmed NOT to have a review yet.
    ///
    /// `is_pr_reviewed` caches positive results (reviewed) in persistent state forever.
    /// Without this cache, every unreviewed PR triggers a `gh pr view` GraphQL call on
    /// every PR poll tick (~every 45s). With this cache, we suppress repeat calls for
    /// PRs confirmed unreviewed within the last `PR_REVIEW_NEGATIVE_CACHE_SECS` seconds.
    ///
    /// Short TTL (2 min) ensures we eventually detect the review after it's posted.
    pr_review_negative_cache: std::sync::Mutex<HashMap<u64, std::time::Instant>>,
    /// Cache for "Review in progress" placeholder comment IDs per PR.
    /// Maps PR number → (comment_id, checked_at).
    /// None means "no placeholder found" (negative result).
    /// Positive results (Some(comment_id)) are kept until the reviewer completes.
    reviewer_placeholder_cache: std::sync::Mutex<HashMap<u64, (Option<u64>, std::time::Instant)>>,
    /// Pending questions from coworkers waiting for Lead input (AskUserQuestion tool).
    ///
    /// Ephemeral — lost on daemon restart. Entries are added by `handle_coworker_asking`
    /// and removed when the coworker receives a nudge via `handle_coworker_nudge`.
    /// The counter is used to assign monotonically increasing IDs.
    pub(crate) pending_questions: std::sync::Mutex<Vec<PendingQuestion>>,
    /// Counter for assigning unique IDs to pending questions.
    pending_question_id_counter: std::sync::atomic::AtomicU64,
    /// Maps coworker session name → auth profile email for pool-based spawns.
    ///
    /// Populated in `spawn_coworker()` when a profile is selected from the pool.
    /// Used by usage-limit detection to attribute a session's limit to the
    /// correct profile (so it can be marked as `is_usage_limited` in persistent state).
    ///
    /// Ephemeral — not persisted across daemon restarts. Entries are added
    /// on spawn and removed in `cleanup_coworker_state`.
    pub(crate) session_profile_map: std::sync::Mutex<HashMap<String, String>>,
    /// Maps `tool_use_id → channel message_id` for DM channel sub-agent threading.
    ///
    /// When a DM message carries `tool_use_id` (from a top-level tool_use block),
    /// the PostToChannel executor registers the mapping here. Sub-agent events in
    /// later drain cycles reference the tool_use_id via `parentToolUseID` to post
    /// as thread replies under the original tool call message.
    ///
    /// Cleaned up in `cleanup_coworker_state` when a coworker shuts down.
    pub(crate) dm_tool_threads: std::sync::Mutex<HashMap<String, String>>,
    /// Per-channel locks for workflow state file writes.
    ///
    /// Prevents TOCTOU races when concurrent `set_state` calls for different plugin
    /// Manages the long-running Python plugin daemon process.
    /// Spawns `uv run python -m midtown` when plugins are detected in
    /// discovery paths. Communicates via Unix socket.
    pub(crate) plugin_daemon: plugin_daemon::PluginDaemonManager,
    /// File-per-task storage for Midtown's own task persistence.
    ///
    /// Each task is stored as a JSON file in `~/.midtown/<project>/tasks/`.
    /// Replaces the scattered `task_*` HashMaps on `DaemonPersistentState`.
    /// Not behind a Mutex — only does file I/O (no shared mutable state).
    pub(crate) task_store: crate::task_store::TaskStore,
    /// Write-through task index for fast lookups without directory scanning.
    ///
    /// Updated after every `task_store.save()` call. Reconciled from disk
    /// via `task_store.build_index()` on daemon startup.
    pub(crate) task_index: std::sync::Mutex<HashMap<String, crate::task_store::TaskIndexEntry>>,
}

impl DaemonState {
    /// Get the GitHub full name (owner/repo) for a repo path, using cache.
    ///
    /// On first call for a given path, runs `gh repo view --json nameWithOwner`.
    /// Subsequent calls return the cached value without any API call.
    fn get_repo_full_name(&self, repo_path: &std::path::Path) -> String {
        // Fast path: check cache
        {
            let cache = self.repo_name_cache.read().unwrap();
            if let Some(name) = cache.get(repo_path) {
                return name.clone();
            }
        }

        // Slow path: fetch from GitHub API and cache
        let full_name = crate::process::cmd_stdout(
            std::process::Command::new("gh")
                .current_dir(repo_path)
                .args([
                    "repo",
                    "view",
                    "--json",
                    "nameWithOwner",
                    "--jq",
                    ".nameWithOwner",
                ])
                .output(),
        )
        .unwrap_or_default();

        let mut cache = self.repo_name_cache.write().unwrap();
        cache.insert(repo_path.to_path_buf(), full_name.clone());
        full_name
    }

    /// Record a coworker's stop time for orphan recovery grace period.
    ///
    /// Called internally by `Effect::ShutdownCoworker` and
    /// `Effect::ShutdownCoworkerWithCallbacks`. Also called when a coworker
    /// exits unexpectedly (signal handler cleanup). Without this, the next
    /// TaskDispatchTick sees the coworker's in_progress task as orphaned and
    /// falsely respawns them. See #874.
    fn record_coworker_stop_time(&self, name: &str) {
        let mut stop_times = self.coworker_stop_times.write().unwrap();
        stop_times.insert(name.to_lowercase(), chrono::Utc::now());
    }

    /// Clear the lead's stop time so `ensure_lead_alive()` respawns on the next tick.
    /// Called when a user message arrives while the lead is dead.
    pub(crate) fn clear_lead_respawn_cooldown(&self) {
        let mut stop_times = self.coworker_stop_times.write().unwrap();
        if stop_times
            .remove(&self.project_name.to_lowercase())
            .is_some()
        {
            tracing::info!("Cleared lead respawn cooldown — user message while lead is dead");
        }
    }

    /// Expedite lead respawn when a user posts while the lead is dead.
    ///
    /// 1. Clears the respawn cooldown so `ensure_lead_alive()` fires on the next tick.
    /// 2. Posts a system message to the main channel so the user sees immediate feedback.
    /// 3. Spawns or resumes the ops channel lead and nudges it to acknowledge the situation.
    pub(crate) async fn expedite_lead_respawn_on_user_message(&self) {
        // 1. Clear the cooldown so the lead respawns on the next tick (~5s).
        self.clear_lead_respawn_cooldown();

        // 2. Post a system message to the main channel so the user isn't left wondering.
        let channel_name = self.channel_router.default_channel_name();
        let sys_msg = crate::message::Message::for_channel(
            channel_name,
            "midtown",
            "Lead session is restarting -- your message has been received and will be handled shortly.".to_string(),
            crate::message::MessageType::System,
        );
        if let Err(e) = self.send_and_broadcast_async(&sys_msg).await {
            tracing::error!("Failed to post system message for lead respawn: {}", e);
        }

        // 3. Spawn or resume the ops channel lead and nudge it to acknowledge.
        let ops_channel = "ops";
        let session_name = crate::launch::channel_lead_session_name(ops_channel);

        let session_ready = if self.session_manager.is_alive(&session_name).await {
            true
        } else {
            let (session_id, session_id_cleared) = {
                let ps = self.persistent_state.lock().await;
                let sid = ps.channel_lead_sessions.get(ops_channel).cloned();
                // Check if the SessionRecord for this channel lead has a cleared session_id
                // (indicating a failed resume that invalidated the session data).
                let cleared = ps
                    .session_by_name(ops_channel)
                    .is_some_and(|record| record.session_id.is_empty());
                (sid, cleared)
            };

            let session_mode = match session_id {
                Some(ref id) if !id.is_empty() && !session_id_cleared => {
                    tracing::info!(
                        "Resuming ops channel lead session (session {}) for lead-dead expedite",
                        id
                    );
                    crate::launch::SessionMode::ResumeSession(id.clone())
                }
                _ => {
                    if session_id_cleared {
                        tracing::info!(
                            "Skipping stale ops channel lead session ID: session record was cleared"
                        );
                    } else {
                        tracing::info!(
                            "No saved session for ops channel lead, spawning fresh for lead-dead expedite"
                        );
                    }
                    crate::launch::SessionMode::Fresh
                }
            };

            let is_fresh = matches!(session_mode, crate::launch::SessionMode::Fresh);
            if is_fresh {
                let mut ps = self.persistent_state.lock().await;
                ps.channel_lead_sessions
                    .entry(ops_channel.to_string())
                    .or_insert_with(String::new);
                if let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
                    tracing::error!(
                        "Failed to save daemon state before spawning ops channel lead: {}",
                        e
                    );
                }
            }

            let config = crate::launch::LaunchConfig::channel_lead(
                ops_channel,
                self.paths.dir_key(),
                session_mode,
                "",
                None,
            );

            match self.spawn_coworker(&config).await {
                Ok(session_id) => {
                    // Update channel_lead_sessions with the real session ID immediately,
                    // replacing the empty placeholder inserted above. This eliminates the
                    // race window before the init StreamEvent arrives.
                    let mut ps = self.persistent_state.lock().await;
                    ps.channel_lead_sessions
                        .insert(ops_channel.to_string(), session_id);
                    if let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
                        tracing::error!(
                            "Failed to save daemon state after ops channel lead spawn: {}",
                            e
                        );
                    }
                    true
                }
                Err(e) => {
                    tracing::error!("Failed to spawn ops channel lead: {}", e);
                    false
                }
            }
        };

        if session_ready {
            let nudge_msg = "The main lead session is down. A user just posted a message. Post to #ops acknowledging the lead is down and being respawned.";
            tracing::info!("Nudging ops channel lead about dead lead + user message");
            if let Err(e) = self
                .session_manager
                .send_message(&session_name, nudge_msg)
                .await
            {
                tracing::error!("Failed to nudge ops channel lead: {}", e);
            }
        }
    }

    /// Clean up all transient state for a coworker after its session stops.
    ///
    /// Called from intentional shutdown (`shutdown_coworker_impl` in effects.rs),
    /// coworker break (`handle_coworker_break` in rpc_coworker.rs), and unexpected
    /// session death (session monitor in the event loop). Without this shared
    /// function, the paths can drift out of sync — e.g., session death missing
    /// cooldown/nudge/assignment cleanup that shutdown handles. See PR #1268.
    ///
    /// Handles: coworker deregistration, stop-time recording, coworker records,
    /// cooldowns, pending nudges, task assignments, recent tool activity,
    /// topic_sessions cleanup, SessionRecord persistent state update (marks
    /// `is_running=false` and `current_name=None`), optional worktree
    /// unbinding, and pending questions.
    ///
    /// Does NOT handle session-manager operations (session_manager.shutdown vs
    /// session_manager.remove) — those differ between the intentional shutdown,
    /// break, and unexpected session death paths.
    pub(crate) async fn cleanup_coworker_state(&self, name: &str) {
        self.cleanup_coworker_state_internal(name, false).await
    }

    /// Clean up all transient state for a dead coworker and release its worktree
    /// binding so collisions don't block immediate respawn.
    pub(crate) async fn cleanup_dead_coworker_state(&self, name: &str) {
        self.cleanup_coworker_state_internal(name, true).await
    }

    async fn cleanup_coworker_state_internal(&self, name: &str, clear_worktree_binding: bool) {
        // Deregister from coworker manager
        self.coworkers.deregister(name);
        // Record stop time for lifecycle tracking
        self.record_coworker_stop_time(name);
        // Clean up unified coworker record (health, workflow phase, etc.)
        {
            let mut records = self.coworker_records.write().await;
            records.remove(name);
        }
        // Clear cooldown entries (prevents stale state on respawn)
        {
            let mut cooldowns = self.cooldowns.lock().unwrap();
            cooldowns.clear_for_key(name);
        }
        // Clear any pending nudge
        self.clear_pending_nudge(name);
        // Task assignment tracking is derived from sessions[].task_id,
        // which is cleared when the session record is cleaned up.
        // Clear tool activity headers (prevents stale activity on respawn)
        {
            let mut headers_map = self.tool_activity_headers.write().unwrap();
            headers_map.remove(name);
        }
        // Clear profile pool mapping (prevents stale profile attribution on name reuse)
        {
            let mut map = self.session_profile_map.lock().unwrap();
            map.remove(&name.to_lowercase());
        }
        // Look up the session record for this coworker before cleanup.
        let (removed_session_id, bound_thread_id, bound_channel) = {
            let ps = self.persistent_state.lock().await;
            match ps.session_by_name(name) {
                Some(r) => (
                    Some(r.session_id.clone()),
                    r.bound_thread_id.clone(),
                    r.channel.clone(),
                ),
                None => (None, None, None),
            }
        };
        // Clear pending questions (prevents stale questions after crash/shutdown)
        {
            let mut questions = self.pending_questions.lock().unwrap();
            questions.retain(|q| q.coworker_name != name);
        }
        // Notify web clients that the thread is no longer fork-owned.
        if let (Some(thread_id), Some(channel)) = (bound_thread_id, bound_channel) {
            self.broadcast_web_update(crate::web::WebUpdate::ThreadOwnership(
                crate::web::ThreadOwnershipData {
                    thread_parent_id: thread_id,
                    channel,
                    has_dedicated_session: false,
                    owner: None,
                    parent_lead: None,
                },
            ));
        }

        // Session records and optional dead-coworker worktree unbinding are persisted
        // together to avoid duplicate writes during shutdown/death cleanup.
        if clear_worktree_binding || removed_session_id.is_some() {
            let mut ps = self.persistent_state.lock().await;
            let mut changed = false;

            if let Some(session_id) = removed_session_id {
                // Mark the SessionRecord as stopped in persistent state.
                if let Some(record) = ps.sessions.get_mut(&session_id) {
                    record.is_running = false;
                    changed = true;
                }
                // Close any open task-session spans for the exiting session.
            }

            if clear_worktree_binding {
                let now = chrono::Utc::now();
                let bound_worktree_ids: Vec<String> = ps
                    .worktree_registry
                    .all_assignments()
                    .iter()
                    .filter(|(_, assignment)| {
                        assignment
                            .current_coworker
                            .as_ref()
                            .is_some_and(|c| c.eq_ignore_ascii_case(name))
                    })
                    .map(|(worktree_id, _)| worktree_id.clone())
                    .collect();

                for worktree_id in &bound_worktree_ids {
                    ps.worktree_registry.mark_completed(worktree_id, now);
                }
                if !bound_worktree_ids.is_empty() {
                    changed = true;
                }

                if ps.worktree_registry.unbind_coworker(name) {
                    changed = true;
                }
            }

            if changed && let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
                tracing::warn!(
                    "Failed to save persistent state after cleanup for coworker '{}': {}",
                    name,
                    e
                );
            }
        }
    }

    /// Remove expired entries from the RPC response cache.
    ///
    /// Called periodically during PR polling ticks, alongside other cleanup
    /// operations (cooldowns, stale webhook events, etc.). Without this,
    /// expired entries remain in the HashMap forever — their TTL is only
    /// checked on read, but memory is never freed.
    async fn cleanup_rpc_response_cache(&self) {
        let now = std::time::Instant::now();
        let mut cache = self.rpc_response_cache.lock().await;
        cache.retain(|_, (_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);
        self.prs_cache.cleanup();
    }

    /// Select an auth profile from the pool configured for this coworker's role.
    ///
    /// Returns `None` if no pool is configured for this role, or if all profiles
    /// in the pool are currently usage-limited.
    ///
    /// Role-to-pool mapping:
    /// - `Coworker` → `execution.coworker_profiles`
    /// - `Reviewer` → `execution.reviewer_profiles`
    /// - `ChannelLead` → `execution.channel_lead_profiles`
    /// - `Lead` → always `None` (leads use a fixed profile)
    async fn select_profile_from_pool(
        &self,
        config: &crate::launch::LaunchConfig,
    ) -> Option<String> {
        let execution = crate::config::get_project_execution_config(self.paths.dir_key());
        let pool = match config.agent_type.as_str() {
            "midtown-code-author" => execution.coworker_profiles,
            "midtown-code-reviewer" => execution.reviewer_profiles,
            "midtown-channel-lead" => execution.channel_lead_profiles,
            "midtown-project-lead" => None,
            _ => None,
        }?;

        let ps = self.persistent_state.lock().await;
        crate::daemon::profile_pool::select_profile(&pool, &ps.profile_pool_state)
    }

    /// Check if the daemon is at the in-progress task limit.
    ///
    /// Reads task status from disk. Used by RPC handlers (`rpc_coworker.rs`,
    /// `chat.rs`) that operate outside the snapshot pipeline and don't have
    /// access to a pre-computed snapshot. The snapshot pipeline uses
    /// `tick_is_at_task_limit` (pre-computed in `prepare_tick()`)
    /// for pure decision functions.
    ///
    /// Only counts tasks with active owners (registered in CoworkerManager).
    /// Tasks whose owners are dead (e.g., after a restart) don't consume
    /// coworker slots and should not block new spawns.
    fn is_at_task_limit(&self) -> bool {
        let tasks = self.task_store.load_all();
        let registered: std::collections::HashSet<String> = self
            .coworkers
            .list()
            .iter()
            .map(|cw| cw.name.to_lowercase())
            .collect();
        let active_in_progress_count = tasks
            .iter()
            .filter(|t| t.status == crate::task_store::TaskStatus::InProgress)
            .filter(|t| {
                if t.agent_name.is_empty() {
                    true // ownerless tasks count (freshly dispatched)
                } else {
                    registered.contains(&t.agent_name.to_lowercase())
                }
            })
            .count();
        active_in_progress_count >= self.max_in_progress_tasks
    }

    /// Check if a PR has at least one completed review.
    ///
    /// Uses the persistent state cache as the single source of truth. First
    /// checks the cache; if not found, makes GitHub API calls and caches
    /// positive results permanently (review status is monotonic).
    async fn is_pr_reviewed(&self, pr_number: u64) -> bool {
        // Fast path: check persistent cache (single source of truth)
        {
            let ps = self.persistent_state.lock().await;
            if ps.github.has_cached_review(pr_number) {
                // Bug #3 fix: even on the fast path, ensure review comment IDs
                // are backfilled for Gate 3. Without this, reviews cached via
                // webhook (where the comment ID wasn't recorded) would have
                // empty IDs, causing Gate 3 to trivially pass.
                if !ps.github.get_review_comment_ids(pr_number).is_empty() {
                    debug!(
                        "PR #{} has cached completed review with IDs (skipping API call)",
                        pr_number
                    );
                    return true;
                }
                // IDs empty — fall through to backfill (lock dropped here).
                // Clear stale negative cache entry: the review exists (cached via
                // webhook), so the negative cache must not suppress the backfill.
                {
                    let mut neg_cache = self.pr_review_negative_cache.lock().unwrap();
                    neg_cache.remove(&pr_number);
                }
                debug!(
                    "PR #{} has cached review but no comment IDs — backfilling",
                    pr_number
                );
            }
        }

        // Negative cache: skip the API call if we recently confirmed no review exists.
        // This prevents a gh pr view GraphQL call on every poll tick for each unreviewed PR.
        {
            let neg_cache = self.pr_review_negative_cache.lock().unwrap();
            if let Some(checked_at) = neg_cache.get(&pr_number)
                && checked_at.elapsed().as_secs() < PR_REVIEW_NEGATIVE_CACHE_SECS
            {
                debug!(
                    "PR #{} not reviewed (negative cache hit, skipping API call)",
                    pr_number
                );
                return false;
            }
        }

        // Slow path: check via API calls (no lock held)
        let (cached, assigned_reviewer, assigned_session_id) = {
            let ps = self.persistent_state.lock().await;
            let cached = ps.github.has_cached_review(pr_number);
            let span = ps.active_reviewer_for_pr(pr_number);
            let reviewer = span.map(|s| s.name.clone());
            let session_id = span.map(|s| s.session_id.clone());
            (cached, reviewer, session_id)
        };
        let has_review = cached
            || pr::pr_has_completed_review_uncached(
                pr_number,
                assigned_reviewer.as_deref(),
                assigned_session_id.as_deref(),
            );

        if has_review {
            // Bug #2 fix: do all blocking I/O (subprocess calls) BEFORE
            // acquiring the async mutex. Running blocking calls under an
            // async mutex starves the Tokio runtime.
            let repo_full_name = self
                .all_repo_paths
                .first()
                .map(|p| self.get_repo_full_name(p))
                .unwrap_or_default();
            let ids = if !repo_full_name.is_empty() {
                pr::fetch_review_comment_ids(&repo_full_name, pr_number)
            } else {
                vec![]
            };

            // Now acquire the mutex only for the mutation operations
            let mut ps = self.persistent_state.lock().await;
            ps.github.mark_reviewed_pr(pr_number);

            for id in &ids {
                debug!(
                    "Polling: recording review comment ID {} for PR #{}",
                    id, pr_number
                );
                ps.github.add_review_comment_id(pr_number, *id);
            }

            drop(ps);
            // Clear placeholder cache: review is done, no need to track placeholder anymore
            let mut placeholder_cache = self.reviewer_placeholder_cache.lock().unwrap();
            placeholder_cache.remove(&pr_number);
        } else {
            // Cache negative result with TTL to avoid repeated API calls
            let mut neg_cache = self.pr_review_negative_cache.lock().unwrap();
            neg_cache.insert(pr_number, std::time::Instant::now());
        }

        has_review
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        socket_path: PathBuf,
        coworkers: CoworkerManager,
        paths: crate::paths::ProjectPaths,
        all_repo_paths: Vec<PathBuf>,
        channel_router: crate::ChannelRouter,
        web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
        max_in_progress_tasks: usize,
        push_manager: Option<std::sync::Arc<crate::push::PushManager>>,
        default_branch: String,
        shutdown_tx: broadcast::Sender<()>,
        session_agg_tx: tokio::sync::mpsc::UnboundedSender<session_events::SessionEvent>,
    ) -> crate::Result<Self> {
        let dir_key = paths.dir_key();
        let project_name = paths.project_name().to_string();

        // Load unified persistent state (migrates from legacy files if needed)
        let mut persistent_state = state::DaemonPersistentState::load_for_repo(dir_key)
            .unwrap_or_else(|e| {
                warn!("Failed to load daemon-state.json: {}, using defaults", e);
                state::DaemonPersistentState::default()
            });

        // Migrate tasks from old ~/.claude/tasks/ format to new ~/.midtown/ format
        migration::maybe_migrate_tasks(dir_key, &persistent_state);

        let user_display_name = config::get_user_display_name_for_project(dir_key);

        // Extract repo owner from git remote URL (once at startup, no API call needed).
        // Used by pure decision functions to determine if a PR is lead-authored.
        let repo_owner = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                extract_repo_name_from_url(&url)
            })
            .and_then(|name_with_owner| {
                // extract_repo_name_from_url returns "owner/repo", we want just "owner"
                name_with_owner.split('/').next().map(|s| s.to_string())
            });
        if let Some(ref owner) = repo_owner {
            info!("Detected repo owner: {}", owner);
        }

        // Clone dir_key for session_manager before moving paths into Self
        let session_manager_repo_name = dir_key.to_string();

        // Set up the task store and build the initial task index from disk.
        let task_store = crate::task_store::TaskStore::new(paths.tasks_dir());
        let task_index = task_store.build_index();
        // Reconcile the persistent state's task_index with the on-disk tasks.
        if !task_index.is_empty() {
            persistent_state.task_index = task_index.clone();
        }

        // Set up the plugin daemon manager with the workflows directory.
        let workflows_dir = paths.workflows_dir();
        let plugin_daemon_socket = paths.plugin_daemon_socket();
        let python_sdk_dir = crate::paths::resolve_python_sdk_dir();
        let plugin_daemon = plugin_daemon::PluginDaemonManager::new(
            plugin_daemon_socket,
            workflows_dir,
            python_sdk_dir,
        );

        Ok(Self {
            coworkers,
            channel_router,
            socket_path,
            coworker_records: tokio::sync::RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::with_permanent_nudges(
                persistent_state
                    .permanent_pr_nudges
                    .iter()
                    .cloned()
                    .collect(),
            )),
            project_name,
            paths,
            repo_owner,
            default_branch,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            persistent_state: Mutex::new(persistent_state),
            web_updates_tx,
            max_in_progress_tasks,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            last_pr_poll_hash: Mutex::new(0),
            pr_poll_data: std::sync::RwLock::new(PrPollData::default()),
            coworker_stop_times: std::sync::RwLock::new(HashMap::new()),
            stuck_tracker: Mutex::new(StuckConditionTracker::new()),
            ci_notification_buffer: Mutex::new(trackers::CiNotificationBuffer::new()),
            repo_name_cache: std::sync::RwLock::new(HashMap::new()),
            user_display_name,
            last_webhook_event_at: Mutex::new(None),
            in_flight_task_spawns: std::sync::Mutex::new(HashSet::new()),
            in_flight_review_prs: std::sync::Mutex::new(HashSet::new()),
            // Task assignments are tracked via sessions[].task_id
            pending_nudges: std::sync::Mutex::new(HashMap::new()),
            comment_tracker: Mutex::new(trackers::CommentTracker::new()),
            insight_hashes: std::sync::Mutex::new(HashSet::new()),
            reviewer_escalations_posted: std::sync::Mutex::new(HashSet::new()),
            orphaned_pr_lead_nudges_sent: std::sync::Mutex::new(HashSet::new()),
            review_note_tracker: std::sync::Mutex::new(HashMap::new()),
            pending_forks: std::sync::Mutex::new(HashSet::new()),
            worktree_freshness_cache: std::sync::Mutex::new(None),
            coworkers_with_unblocked_deps_cache: std::sync::Mutex::new(None),
            headless_health: std::sync::RwLock::new(HashMap::new()),
            headless_health_generation: std::sync::atomic::AtomicU64::new(0),
            health_derived_cache: std::sync::Mutex::new(None),
            attached_coworkers: std::sync::Mutex::new(HashMap::new()),
            session_manager: sessions::SessionManager::new(
                session_manager_repo_name,
                session_agg_tx,
            ),
            rpc_response_cache: Mutex::new(HashMap::new()),
            prs_cache: rpc_prs::PrsCache::new(rpc_prs::PRS_CACHE_TTL),
            pr_review_negative_cache: std::sync::Mutex::new(HashMap::new()),
            reviewer_placeholder_cache: std::sync::Mutex::new(HashMap::new()),
            draining: std::sync::atomic::AtomicBool::new(false),
            restart_requested: std::sync::atomic::AtomicBool::new(false),
            shutdown_tx,
            tool_activity_headers: std::sync::RwLock::new(HashMap::new()),
            pending_questions: std::sync::Mutex::new(Vec::new()),
            pending_question_id_counter: std::sync::atomic::AtomicU64::new(1),
            session_profile_map: std::sync::Mutex::new(HashMap::new()),
            dm_tool_threads: std::sync::Mutex::new(HashMap::new()),
            plugin_daemon,
            task_store,
            task_index: std::sync::Mutex::new(task_index),
        })
    }

    /// Spawn a coworker as a headless session and initialize its record.
    ///
    /// Uses `CoworkerManager::prepare_spawn` for worktree lifecycle, then
    /// `SessionManager::spawn` for the headless process, and finally
    /// `CoworkerManager::register` to add the coworker to the tracking map.
    /// Spawn a new headless coworker session. Returns the session ID used (either a
    /// pre-existing resumed ID, or a freshly generated one for new sessions).
    async fn spawn_coworker(&self, config: &crate::launch::LaunchConfig) -> crate::Result<String> {
        let name = config.name.clone();
        let slot_id = uuid::Uuid::new_v4().to_string();

        // Idempotent: if this coworker is already running, skip silently.
        // Multiple code paths (orphan recovery, task dispatch, PR call-in) can
        // independently decide to spawn the same coworker in the same tick.
        if self.session_manager.is_alive(&name).await {
            tracing::debug!(
                "Coworker {} already has a running session, skipping spawn",
                name
            );
            // Return the existing session's ID so callers can update their state.
            let ps = self.persistent_state.lock().await;
            let existing_id = ps
                .session_by_name(&name)
                .map(|s| s.session_id.clone())
                .unwrap_or_default();
            return Ok(existing_id);
        }

        // Inject project-resolved auth profile if not already set
        let mut config = if config.auth_profile_dir.is_none() {
            let mut c = config.clone();

            // Try pool-based profile selection first.
            if let Some(email) = self.select_profile_from_pool(&c).await {
                tracing::debug!("Pool selected profile '{}' for coworker '{}'", email, name);
                // Record session→profile mapping for usage-limit attribution.
                {
                    let mut map = self.session_profile_map.lock().unwrap();
                    map.insert(name.to_lowercase(), email.clone());
                }
                // Mark last_used_at so future spawns use LRU ordering.
                {
                    let mut ps = self.persistent_state.lock().await;
                    ps.profile_pool_state
                        .entry(email.clone())
                        .or_default()
                        .last_used_at = Some(chrono::Utc::now());
                    let _ = ps.save_for_repo(self.paths.dir_key());
                }
                c.auth_profile_dir = Some(crate::auth::profile_dir_for(c.auth_provider, &email));
            } else {
                // No pool configured or all profiles limited — fall back to single profile.
                c.auth_profile_dir =
                    Some(crate::auth::active_profile_dir_for_project_with_provider(
                        self.paths.dir_key(),
                        c.auth_provider,
                    ));
            }
            c
        } else {
            config.clone()
        };
        let normalized_model = helpers::normalize_model_for_provider_role(
            &config.model,
            config.auth_provider,
            &config.agent_type,
        );
        if normalized_model != config.model {
            warn!(
                "Normalizing model '{}' to '{}' for provider {:?} (name: {})",
                config.model, normalized_model, config.auth_provider, name
            );
            config.model = normalized_model;
        }

        // Prepare worktree and augment config with additional dirs
        // Note: Worktree creation now happens via Effect::EnsureWorktree in the
        // decision layer (rules.rs), not inline here. This follows the effect-based
        // architecture: I/O goes through the Effect pipeline.
        let (working_dir, launch_config) = self.coworkers.prepare_spawn(&config)?;

        // Build headless config from the unified launch config
        let mut headless_config = launch_config.to_headless_config(&self.paths);

        // Override agent_name from TaskStore if the task has a custom agent type.
        // This supports task handoff: when `midtown task handoff --agent <type>` changes
        // the agent type, subsequent resumes pick up the new agent definition via --agent.
        if let Some(ref task_id) = config.task_id
            && let Ok(store_task) = self.task_store.load(task_id)
            && !store_task.agent_type.is_empty()
            && store_task.agent_type != "midtown-code-author"
        {
            headless_config.agent_name = Some(store_task.agent_type.clone());
        }

        // Apply cwd_subdir: if a subdirectory is configured (e.g., channel_directory),
        // append it to the worktree root so the session runs in that subdirectory.
        if let Some(ref subdir) = launch_config.cwd_subdir {
            let sub_path = std::path::Path::new(&working_dir).join(subdir);
            if sub_path.is_dir() {
                headless_config.cwd = Some(sub_path.to_string_lossy().to_string());
            } else {
                warn!(
                    "cwd_subdir '{}' does not exist under '{}', falling back to worktree root",
                    subdir, working_dir
                );
                headless_config.cwd = Some(working_dir.clone());
            }
        } else {
            headless_config.cwd = Some(working_dir.clone());
        }

        // Write role-appropriate settings file for Claude-platform sessions.
        // Codex currently has no settings file equivalent.
        if crate::platform::Platform::from_provider(config.auth_provider)
            == crate::platform::Platform::Claude
        {
            let settings_file = if config.agent_type == "midtown-project-lead" {
                crate::settings::write_lead_settings_file()?
            } else {
                crate::settings::write_coworker_settings_file()?
            };
            headless_config.settings_path = Some(settings_file.to_string_lossy().to_string());
        }

        // Determine the session ID for this spawn.
        //
        // - ResumeSession(id): use the existing session ID.
        // - Fresh Claude/z.ai: pre-assign UUID and pass --session-id.
        // - Fresh Codex: pre-assign a provisional UUID. The real session ID
        //   arrives with the init event and replaces it via atomic migration.
        //   This ensures every session has a SessionRecord from spawn time.
        let session_id = match &config.session_mode {
            crate::launch::SessionMode::ResumeSession(sid) => Some(sid.clone()),
            _ => Some(uuid::Uuid::new_v4().to_string()),
        };
        // For fresh Claude-platform sessions, set the pre-generated session ID on the
        // headless config so it gets passed as --session-id to the CLI.
        // Codex sessions don't accept --session-id, so the UUID is provisional
        // (used internally for SessionRecord keying, migrated on init).
        if !matches!(
            config.session_mode,
            crate::launch::SessionMode::ResumeSession(_)
        ) && crate::platform::Platform::from_provider(config.auth_provider)
            == crate::platform::Platform::Claude
        {
            headless_config.session_id = session_id.clone();
        }
        // Inject MIDTOWN_SESSION_ID so coworkers can call `midtown agent fork`
        // without passing --session-id explicitly.
        //
        // Only inject for stable session IDs: Claude sessions adopt the pre-generated
        // UUID via --session-id so the env var always matches the real session ID.
        // Codex sessions use a provisional UUID that is replaced by the real ID on init,
        // so we skip injection there to avoid a stale reference after migration.
        // Resumed sessions have a real, stable ID regardless of provider.
        let is_stable_id = matches!(
            config.session_mode,
            crate::launch::SessionMode::ResumeSession(_)
        ) || crate::platform::Platform::from_provider(config.auth_provider)
            == crate::platform::Platform::Claude;
        if let Some(ref sid) = session_id
            && is_stable_id
        {
            crate::launch::inject_session_id_env(&mut headless_config.env, sid);
        }
        // Expand $MIDTOWN_SESSION_ID in the system prompt and initial prompt so the
        // AI sees the literal UUID rather than an env-var reference. Claude Code
        // sessions typically use single-quoted heredocs for multi-line shell args,
        // which prevents shell expansion — embedding the value directly avoids this.
        if let Some(ref sid) = session_id {
            headless_config.system_prompt =
                crate::launch::expand_session_id_in_prompt(&headless_config.system_prompt, sid);
        }
        let persisted_session_id = session_id.clone().unwrap_or_default();
        let initial_prompt = match (&session_id, launch_config.initial_prompt.as_deref()) {
            (Some(sid), Some(prompt)) => {
                Some(crate::launch::expand_session_id_in_prompt(prompt, sid))
            }
            (_, prompt) => prompt.map(|p| p.to_string()),
        };
        self.session_manager
            .spawn(
                &name,
                &slot_id,
                &headless_config,
                initial_prompt.as_deref(),
                session_id.clone(),
            )
            .await?;

        // If persisted_initial_prompt is set (e.g., session clear), override the
        // CoworkerSession's stored prompt so collect_session_info() returns the
        // canonical prompt at daemon shutdown — not the decorated "fresh restart" wrapper.
        if let Some(ref canonical) = config.persisted_initial_prompt {
            self.session_manager
                .set_canonical_initial_prompt(&name, Some(canonical.clone()))
                .await;
        }

        // Register in the CoworkerManager tracking map (keyed by slot_id)
        // Extract profile name from auth_profile_dir.
        // For Claude, profile_dir_for() returns `<base>/<name>/claude`, so the profile
        // name is the parent's file_name, not the leaf. For other providers, it's the leaf.
        let profile = config
            .auth_profile_dir
            .as_ref()
            .and_then(|p| {
                if config.auth_provider == crate::auth::AuthProvider::Claude {
                    // Path is ~/.midtown/auth/<profile>/claude — extract <profile>
                    p.parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|n| n.to_str())
                } else {
                    p.file_name().and_then(|n| n.to_str())
                }
            })
            .unwrap_or(crate::auth::DEFAULT_PROFILE)
            .to_string();

        let working_dir_for_persist = working_dir.clone();
        if let Err(e) = self.coworkers.register(
            &slot_id,
            &name,
            working_dir,
            None,
            config.model.clone(),
            config.auth_provider,
            profile.clone(),
        ) {
            // Race condition: another spawn beat us to registration. Clean up the
            // headless session we just created to prevent orphaned processes.
            tracing::warn!(
                "Spawn race detected for {}: name was taken, killing orphaned headless session",
                name
            );
            if let Err(kill_err) = self.session_manager.shutdown(&name).await {
                tracing::error!(
                    "Failed to kill orphaned headless session for {}: {}",
                    name,
                    kill_err
                );
            }
            return Err(e);
        }

        // Persist SessionRecord immediately so `session.list` can find the entry
        // without waiting for the init event. Every session gets a UUID at spawn:
        // Claude/z.ai sessions pass it as --session-id, Codex sessions use it as a
        // provisional key that gets migrated to the real ID on init.
        let session_id_for_record = persisted_session_id.clone();
        let working_dir_for_record = working_dir_for_persist.clone();
        {
            let mut ps = self.persistent_state.lock().await;
            let agent_type_str = config.agent_type.clone();
            // Look up bound thread from TaskStore — mirrors SpawnForTask path
            // in effects.rs so reviewers get thread-bound like dispatched dev tasks.
            let bound_thread_id = config
                .task_id
                .as_deref()
                .and_then(|tid| self.task_store.load(tid).ok())
                .and_then(|t| t.thread_id.clone());
            ps.upsert_session_running(
                session_id_for_record.clone(),
                crate::daemon::state::SessionRecord {
                    session_id: session_id_for_record.clone(),
                    task_id: config.task_id.clone(),
                    name: name.clone(),
                    working_dir: working_dir_for_record.clone(),
                    pr_number: config.pr_number,
                    initial_prompt: config
                        .persisted_initial_prompt
                        .clone()
                        .or_else(|| config.initial_prompt.clone()),
                    agent_type: agent_type_str,
                    is_running: true,
                    created_at: chrono::Utc::now(),
                    resume_on_startup: true,
                    last_active: chrono::Utc::now(),
                    purpose: config
                        .initial_prompt
                        .as_deref()
                        .map(|p| p.chars().take(120).collect::<String>())
                        .unwrap_or_default(),
                    pid: None, // Set after process starts
                    channel: config.channel.clone(),
                    provider: Some(config.auth_provider),
                    platform: Some(crate::platform::Platform::from_provider(
                        config.auth_provider,
                    )),
                    profile: Some(profile.clone()),
                    bound_thread_id: bound_thread_id.clone(),
                    color: config.color.clone(),
                    icon: config.icon.clone(),
                    avatar_badge: config.avatar_badge.clone(),
                    ..Default::default()
                },
            );
            if let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
                warn!("Failed to save persistent state after spawn: {}", e);
            }
        }

        // Insert fresh coworker record for health/workflow tracking
        let mut records = self.coworker_records.write().await;
        records.insert(name.clone(), crate::rules::CoworkerRecord::new_spawn());

        // Clear stale stop time so orphan recovery grace period doesn't reference
        // a previous session's shutdown timestamp.
        {
            let mut stop_times = self.coworker_stop_times.write().unwrap();
            stop_times.remove(&name.to_lowercase());
        }
        Ok(session_id.unwrap_or_default())
    }

    /// Check if a sender name represents the user (either "user" or the configured display name).
    fn is_user_sender(&self, from: &str) -> bool {
        from.eq_ignore_ascii_case("user")
            || self
                .user_display_name
                .as_ref()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(from))
    }

    /// Check if a task has a pending spawn effect that hasn't completed yet.
    ///
    /// Used by `spawn_for_pending_tasks` to avoid generating duplicate effects.
    pub(crate) fn is_task_spawn_in_flight(&self, task_id: &str) -> bool {
        self.in_flight_task_spawns.lock().unwrap().contains(task_id)
    }

    /// Mark a task as having a pending spawn effect.
    ///
    /// Called after `evaluate_tick` returns effects, before `execute_effects`.
    pub(crate) fn mark_task_spawn_in_flight(&self, task_id: &str) {
        self.in_flight_task_spawns
            .lock()
            .unwrap()
            .insert(task_id.to_string());
    }

    /// Check if a review task creation is already in-flight for this PR.
    pub(crate) fn is_review_pr_in_flight(&self, pr_number: u64) -> bool {
        self.in_flight_review_prs
            .lock()
            .unwrap()
            .contains(&pr_number)
    }

    /// Mark a PR as having a pending CreateReviewTask effect.
    pub(crate) fn mark_review_pr_in_flight(&self, pr_number: u64) {
        self.in_flight_review_prs.lock().unwrap().insert(pr_number);
    }

    /// Clear the in-flight marker for a review PR after its effect completes.
    pub(crate) fn clear_review_pr_in_flight(&self, pr_number: u64) {
        self.in_flight_review_prs.lock().unwrap().remove(&pr_number);
    }

    /// Clear the in-flight marker for a task after its spawn or nudge effect completes.
    ///
    /// Called from `execute_effects` when `SpawnForTask` or
    /// `NudgeCoworker` (with `RecordTaskAssignment`) succeeds or fails.
    pub(crate) fn clear_task_spawn_in_flight(&self, task_id: &str) {
        self.in_flight_task_spawns.lock().unwrap().remove(task_id);
    }

    /// Get the task ID for a coworker from session records.
    ///
    /// Looks up the coworker's running session and returns its `task_id`.
    /// This is the single source of truth for coworker→task mapping.
    pub(crate) async fn get_task_id_for_coworker(&self, coworker: &str) -> Option<String> {
        let ps = self.persistent_state.lock().await;
        ps.session_by_name(&coworker.to_lowercase())
            .filter(|r| r.is_running)
            .and_then(|r| r.task_id.clone())
    }

    /// Get all coworker→task_id mappings from session records.
    ///
    /// Derives the mapping by iterating running sessions with task bindings.
    /// Used by snapshot collection and RPC handlers.
    pub(crate) async fn get_name_task_assignments(&self) -> HashMap<String, String> {
        let ps = self.persistent_state.lock().await;
        ps.sessions
            .values()
            .filter(|r| !r.name.is_empty() && r.is_running)
            .filter_map(|r| {
                let task_id = r.task_id.as_ref()?;
                Some((r.name.clone(), task_id.clone()))
            })
            .collect()
    }

    /// Get names of coworkers with in-progress tasks, derived from sessions.
    ///
    /// A coworker is "busy" if its session has a `task_id` pointing to an
    /// in-progress task. This uses sessions as the source of truth instead
    /// of reading task file owners.
    pub(crate) async fn get_busy_session_names(&self) -> HashSet<String> {
        let ps = self.persistent_state.lock().await;
        let all_tasks = self.task_store.load_all();
        let in_progress_ids: HashSet<String> = all_tasks
            .iter()
            .filter(|t| t.status == crate::task_store::TaskStatus::InProgress)
            .map(|t| t.id.clone())
            .collect();
        ps.sessions
            .values()
            .filter(|s| {
                s.task_id
                    .as_deref()
                    .is_some_and(|tid| in_progress_ids.contains(tid))
            })
            .filter(|s| !s.name.is_empty())
            .map(|s| s.name.to_lowercase())
            .collect()
    }

    /// Clear the task_id from all session records matching a given task ID.
    ///
    /// Called when a task is completed, reset to pending, or unassigned.
    pub(crate) async fn clear_task_assignment_by_task(&self, task_id: &str) {
        // Clear from session records
        let mut ps = self.persistent_state.lock().await;
        let mut cleared = false;
        for record in ps.sessions.values_mut() {
            if record.task_id.as_deref() == Some(task_id) {
                record.task_id = None;
                cleared = true;
            }
        }
        if cleared && let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
            warn!(
                "Failed to save state after clearing task assignment for !{}: {}",
                task_id, e
            );
        }
    }

    /// Restore task assignments from disk after daemon restart.
    ///
    /// Reconciles `sessions[].task_id` with disk-based task storage:
    /// 1. Clears stale task_id values (task no longer in_progress)
    /// 2. Backfills missing task_id from in_progress tasks with owners
    pub(crate) async fn restore_task_assignments_from_disk(&self) {
        let all_tasks = self.task_store.load_all();
        let in_progress_tasks: Vec<(String, String, String)> = all_tasks
            .iter()
            .filter(|t| t.status == crate::task_store::TaskStatus::InProgress)
            .map(|t| (t.id.clone(), t.subject.clone(), t.agent_name.clone()))
            .collect();
        let in_progress_task_ids: std::collections::HashSet<&str> = in_progress_tasks
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect();

        let mut ps = self.persistent_state.lock().await;
        let mut restored_count = 0;
        let mut cleared_count = 0;

        // Clear stale task_id values from sessions whose tasks are no longer in_progress.
        // This handles tasks completed/reassigned while the daemon was down.
        for record in ps.sessions.values_mut() {
            if let Some(ref tid) = record.task_id
                && !in_progress_task_ids.contains(tid.as_str())
            {
                record.task_id = None;
                cleared_count += 1;
            }
        }

        // Backfill missing task_id from in_progress tasks with owners
        for (task_id, _subject, owner) in &in_progress_tasks {
            if owner.is_empty() {
                continue;
            }
            let owner_lower = owner.to_lowercase();
            if let Some(record) = ps.session_by_name_mut(&owner_lower)
                && record.task_id.is_none()
            {
                record.task_id = Some(task_id.clone());
                restored_count += 1;
            }
        }

        if restored_count > 0 || cleared_count > 0 {
            info!(
                "Task assignment restore: {} backfilled, {} stale cleared",
                restored_count, cleared_count
            );
            if let Err(e) = ps.save_for_repo(self.paths.dir_key()) {
                warn!(
                    "Failed to save state after restoring task assignments: {}",
                    e
                );
            }
        }
    }

    /// Test helper: set up a mock session with a task assignment.
    ///
    /// Creates a session record and name→session mapping so that
    /// `get_task_id_for_coworker` and `get_name_task_assignments` work.
    #[cfg(test)]
    pub(crate) async fn set_test_task_assignment(&self, coworker: &str, task_id: &str) {
        let session_id = format!("test-session-{}", coworker.to_lowercase());
        let coworker_lower = coworker.to_lowercase();
        // Create session record with task_id
        let mut ps = self.persistent_state.lock().await;
        let record =
            ps.sessions
                .entry(session_id.clone())
                .or_insert_with(|| state::SessionRecord {
                    session_id: session_id.clone(),
                    name: coworker_lower,
                    is_running: true,
                    ..Default::default()
                });
        record.task_id = Some(task_id.to_string());
    }

    /// Record a pending nudge sent to a coworker.
    ///
    /// Called after successfully sending a nudge via `NudgeSession` or
    /// `NudgeCoworker`. The pending nudge is used for attribution
    /// tracking: if queued text matches the pending nudge, we know it's
    /// daemon-sent and can auto-submit with Enter.
    pub(crate) fn record_pending_nudge(&self, name: &str, message: &str) {
        let mut pending = self.pending_nudges.lock().unwrap();
        pending.insert(
            name.to_lowercase(),
            (message.to_string(), std::time::Instant::now()),
        );
    }

    /// Clear the pending nudge for a coworker.
    ///
    /// Called when a queued nudge has been successfully auto-submitted,
    /// or when the coworker shuts down.
    pub(crate) fn clear_pending_nudge(&self, name: &str) {
        let mut pending = self.pending_nudges.lock().unwrap();
        pending.remove(&name.to_lowercase());
    }

    /// Nudge the Lead session via the headless session manager.
    pub(crate) async fn nudge_lead(&self, message: &str) {
        if let Err(e) = self
            .session_manager
            .send_message(&self.project_name, message)
            .await
        {
            tracing::debug!("Failed to nudge lead via session_manager: {}", e);
        }
    }
}

impl DaemonState {
    /// Scan effects for task assignment variants and mark their task IDs as in-flight.
    ///
    /// Called after `evaluate_tick` returns effects, before `execute_effects`.
    /// This prevents the next tick from generating duplicate spawns/nudges for the same task.
    /// Covers `SpawnForTask`, `NudgeCoworker`,
    /// and `SpawnCoworkerWithCallbacks` that contain a `RecordTaskAssignment` in on_success.
    pub(crate) fn mark_in_flight_spawns_from_effects(&self, effects: &[effects::Effect]) {
        for task_id in effects::extract_claimed_task_ids_from_effects(effects) {
            self.mark_task_spawn_in_flight(&task_id);
            debug!("Marked task !{} as in-flight spawn", task_id);
        }
        for pr_number in effects::extract_review_pr_numbers_from_effects(effects) {
            self.mark_review_pr_in_flight(pr_number);
            debug!("Marked PR #{} as in-flight review task creation", pr_number);
        }
    }

    /// Look up the session ID currently holding a given coworker name.
    ///
    /// Case-insensitive: the name is lowercased before lookup.
    /// Returns an empty string if no session is found, which
    /// matches the convention used by `NudgeSession` / `NudgeCoworker`
    /// effects (the execution layer warns on empty session IDs).
    pub(crate) async fn session_id_for_name(&self, name: &str) -> String {
        let ps = self.persistent_state.lock().await;
        ps.session_by_name(&name.to_lowercase())
            .filter(|s| s.is_running)
            .map(|s| s.session_id.clone())
            .unwrap_or_default()
    }

    /// Check if a name corresponds to any known agent session.
    /// Used for DM channel validation — allows posting to dm-<name>
    /// for any recognized agent type.
    pub(crate) async fn is_known_agent_name(&self, name: &str) -> bool {
        // Project lead
        if name == self.project_name {
            return true;
        }
        // Previously active coworker
        if self.coworker_records.read().await.contains_key(name) {
            return true;
        }
        // Check persistent state: active session, channel lead, or persisted record
        {
            let ps = self.persistent_state.lock().await;
            // Active session (any type)
            if ps.session_by_name(name).is_some() {
                return true;
            }
            if ps.channel_lead_sessions.contains_key(name) {
                return true;
            }
            // Persisted session record (covers stopped coworkers whose
            // coworker_records entry was cleaned up but SessionRecord remains).
            // Note: session_by_name already checks sessions.values(), so this
            // is redundant but kept for clarity since session_by_name only
            // matches exact name, and the old code checked all values.
        }
        false
    }

    /// Look up the name currently assigned to a given session ID.
    ///
    /// Infrastructure for the session-centric model — used by effect handlers
    /// and RPC adapters once the session-centric migration is further along.
    #[allow(dead_code)] // Scaffold-ahead-of-use for session-centric tasks (Task 9+)
    pub(crate) async fn name_for_session(&self, session_id: &str) -> Option<String> {
        let ps = self.persistent_state.lock().await;
        ps.sessions.get(session_id).map(|s| s.name.clone())
    }

    /// Look up the session ID currently working on a given task ID.
    ///
    /// Infrastructure for the session-centric model — used by effect handlers
    /// and RPC adapters once the session-centric migration is further along.
    #[allow(dead_code)] // Scaffold-ahead-of-use for session-centric tasks (Task 9+)
    pub(crate) async fn session_for_task(&self, task_id: &str) -> Option<String> {
        let ps = self.persistent_state.lock().await;
        ps.session_by_task(task_id).map(|s| s.session_id.clone())
    }

    /// Update the write-through task index after a TaskStore save.
    ///
    /// Also updates the persistent state's task_index for serialization.
    pub(crate) async fn update_task_index(&self, task: &crate::task_store::Task) {
        let entry = crate::task_store::TaskIndexEntry {
            status: task.status,
            parent: task.parent.clone(),
            agent_name: task.agent_name.clone(),
            agent_type: task.agent_type.clone(),
        };
        // Update in-memory index
        self.task_index
            .lock()
            .unwrap()
            .insert(task.id.clone(), entry.clone());
        // Update persistent state index
        let mut ps = self.persistent_state.lock().await;
        ps.task_index.insert(task.id.clone(), entry);
    }

    /// Returns the name of the default (main) channel for this repo.
    ///
    /// Use this — rather than `repo_name` — when checking whether a task channel
    /// is the main channel versus a topic channel. The default channel name matches
    /// the repository name (e.g., "offload" for a project in the "offload" repo).
    pub(crate) fn default_channel_name(&self) -> &str {
        self.channel_router.default_channel_name()
    }

    /// Send a WebUpdate to all connected WebSocket clients (no-op if web is disabled).
    pub(crate) fn broadcast_web_update(&self, update: WebUpdate) {
        if let Some(ref tx) = self.web_updates_tx {
            let _ = tx.send(update);
        }
    }

    /// Async version of send_and_broadcast that uses spawn_blocking for the channel write.
    ///
    /// The channel_router.send() method acquires a file lock that can take up to 2 seconds under
    /// contention. When called in an async context (like RPC handlers), this can block the
    /// Tokio runtime thread and prevent other tasks from making progress. This async version
    /// moves the blocking file write to a dedicated thread pool.
    async fn send_and_broadcast_async(&self, message: &Message) -> crate::Result<()> {
        let router = self.channel_router.clone();
        let msg = message.clone();
        let write_result = tokio::task::spawn_blocking(move || router.send(&msg)).await;

        let send_result = match write_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(crate::Error::InvalidMessage(format!(
                    "spawn_blocking panic: {}",
                    e
                )));
            }
        };

        // If the channel was newly created (first message routed to it), notify
        // WebSocket clients so the frontend adds it to the sidebar immediately
        // instead of waiting for the next fetchChannels() poll.
        if send_result.is_new {
            self.broadcast_web_update(web::channel_list_changed(
                "created",
                &send_result.channel_name,
            ));
        }

        // Broadcast with the resolved channel name so WebSocket clients always
        // receive the correct channel, even if the original message had channel=None
        // (Channel::send fills it on disk, but the in-memory reference is unchanged).
        let mut broadcast_msg = message.clone();
        if broadcast_msg.channel.is_none() {
            broadcast_msg.channel = Some(send_result.channel_name);
        }
        self.broadcast_web_update(web::channel_message_update(&broadcast_msg));

        Ok(())
    }

    /// Send a web push notification to all subscribed PWA clients.
    ///
    /// This is fire-and-forget: push sending runs in a background task.
    fn send_push_notification(&self, title: &str, body: &str, tag: &str, url: Option<&str>) {
        if let Some(ref pm) = self.push_manager {
            let payload = crate::push::PushPayload {
                title: title.to_string(),
                body: body.to_string(),
                tag: Some(tag.to_string()),
                url: url.map(|u| u.to_string()),
            };
            let subs = pm.load_subscriptions();
            if subs.is_empty() {
                return;
            }
            // Clone the Arc to share the same PushManager instance with the async task
            let pm = pm.clone();
            tokio::spawn(async move {
                pm.send_to_all(&payload).await;
            });
        }
    }

    /// Broadcast a coworker status change to WebSocket clients.
    fn broadcast_coworker_update(
        &self,
        name: &str,
        status: &str,
        current_task: Option<&str>,
        color: Option<&str>,
        icon: Option<&str>,
        avatar_badge: Option<&str>,
    ) {
        // Look up the model from the coworker manager, defaulting to "sonnet" if not found
        let model = self
            .coworkers
            .get(name)
            .map(|cw| cw.model.clone())
            .unwrap_or_else(|| "sonnet".to_string());
        self.broadcast_web_update(web::coworker_status_update(
            name,
            status,
            current_task,
            &model,
            color,
            icon,
            avatar_badge,
        ));
    }

    /// Resolve the channel for a message based on its content.
    ///
    /// If the message mentions a task (e.g., "Task !42 reset to pending"),
    /// looks up that task's assigned channel in the task_channel mapping.
    /// Returns `Some(channel_name)` if found, `None` to use the default channel.
    ///
    /// This enables daemon-generated messages about tasks to automatically
    /// route to the task's topic channel instead of the main channel.
    pub(crate) async fn resolve_message_channel(&self, message: &str) -> Option<String> {
        // Extract task ID from the message content
        let task_id = helpers::extract_task_id(message)?;

        // Look up the task's assigned channel from TaskStore
        self.task_store.load(&task_id).ok().and_then(|t| t.channel)
    }
}

/// Load additional WorktreeManagers for multi-repo projects.
///
/// Creates a WorktreeManager for each additional repo (beyond the primary/workdir).
/// Failures are logged but don't prevent the daemon from starting.
fn load_additional_worktree_managers(
    full_config: Option<&crate::config::FullProjectConfig>,
    config: &DaemonConfig,
) -> Vec<WorktreeManager> {
    let full_config = match full_config {
        Some(c) => c,
        None => return vec![],
    };

    let primary = config.workdir.to_string_lossy().to_string();
    let repos = full_config.project.repos();

    repos
        .into_iter()
        .filter(|r| {
            // Skip the primary repo (it's already handled by the main WorktreeManager)
            let repo_path = std::path::Path::new(r);
            repo_path
                .canonicalize()
                .ok()
                .and_then(|canon| {
                    std::path::Path::new(&primary)
                        .canonicalize()
                        .ok()
                        .map(|p| canon != p)
                })
                .unwrap_or_else(|| *r != primary.as_str())
        })
        .filter_map(
            |repo_path| match WorktreeManager::new(std::path::PathBuf::from(repo_path)) {
                Ok(mgr) => {
                    info!("Additional repo for coworker worktrees: {}", repo_path);
                    Some(mgr)
                }
                Err(e) => {
                    warn!("Failed to create worktree manager for {}: {}", repo_path, e);
                    None
                }
            },
        )
        .collect()
}

/// Extract owner/repo from a git remote URL.
///
/// Handles both HTTPS and SSH URL formats:
/// - HTTPS: `https://github.com/owner/repo.git` -> `owner/repo`
/// - SSH: `git@github.com:owner/repo.git` -> `owner/repo`
fn extract_repo_name_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");

    // SSH format: git@github.com:owner/repo
    // Find the colon after the @ symbol (separates host from path)
    if let Some(at_pos) = url.find('@')
        && let Some(colon_pos) = url[at_pos..].find(':')
    {
        let path = &url[at_pos + colon_pos + 1..];
        if path.contains('/') {
            return Some(path.to_string());
        }
    }

    // HTTPS format: https://github.com/owner/repo
    // Extract the last two path components
    let parts: Vec<&str> = url.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        let repo = parts[0];
        let owner = parts[1];
        if !owner.is_empty() && !repo.is_empty() {
            return Some(format!("{}/{}", owner, repo));
        }
    }

    None
}

/// Validate that the configured github_user has access to the repository.
///
/// Makes a simple `gh repo view` call to verify the user can access the repo.
/// This catches misconfigured github_user early with a clear error message,
/// rather than having mysterious polling failures later.
fn validate_github_repo_access(github_user: &str, workdir: &PathBuf) -> crate::Result<()> {
    info!("Validating GitHub repo access for user: {}", github_user);

    // Get the repo's full name (owner/repo) for the error message
    let output = std::process::Command::new("gh")
        .current_dir(workdir)
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .output()
        .map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to run `gh repo view`: {}", e),
        })?;

    if output.status.success() {
        let repo_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!(
            "Validated github_user '{}' has access to repository '{}'",
            github_user, repo_name
        );
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Rate limit errors are transient — warn but don't block daemon startup.
    if stderr.contains("rate limit") {
        warn!(
            "GitHub API rate limit exceeded — skipping repo access validation for user '{}'. \
             Access will be validated on the next successful API call.",
            github_user
        );
        return Ok(());
    }

    // Get the repo name for a better error message (even if access check failed)
    let repo_name = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            extract_repo_name_from_url(&url)
        })
        .unwrap_or_else(|| "unknown".to_string());

    Err(crate::Error::Rpc {
        code: -32603,
        message: format!(
            "github_user '{}' does not have access to repository '{}'.\n\
             Please check your ~/.midtown/config.toml configuration.\n\
             GitHub error: {}",
            github_user,
            repo_name,
            stderr.trim()
        ),
    })
}

/// Acquire an exclusive lock on the PID file.
///
/// The lock is held for the lifetime of the returned File handle.
///
/// Returns an error if another daemon is already running (lock already held).
fn acquire_pid_lock(pid_path: &PathBuf, workdir: &Path) -> crate::Result<File> {
    // Ensure parent directory exists
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open or create the PID file
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(pid_path)?;

    // Try to acquire an exclusive lock (non-blocking)
    match file.try_lock_exclusive() {
        Ok(()) => {
            // We got the lock. Before writing our PID, read and kill any stale daemon.
            // This handles the case where the old daemon lost the lock (e.g., worktree
            // build replaced the binary) but didn't exit, keeping its children alive.
            let mut old_contents = String::new();
            let _ = file.read_to_string(&mut old_contents);
            if let Ok(old_pid) = old_contents.trim().parse::<u32>()
                && old_pid != std::process::id()
            {
                startup::kill_stale_daemon(old_pid, workdir);
            }

            // Write our PID. After read_to_string, the cursor is at EOF.
            // Seek back to the start before truncating so there are no null
            // bytes between the (now-zero) cursor and the written PID.
            let pid = std::process::id();
            file.seek(SeekFrom::Start(0))?;
            file.set_len(0)?;
            writeln!(file, "{}", pid)?;
            file.sync_all()?;
            Ok(file)
        }
        Err(e) => {
            // Lock is held by another process
            // Try to read the existing PID for a better error message
            let existing_pid = std::fs::read_to_string(pid_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());

            let msg = match existing_pid {
                Some(pid) => format!(
                    "Another daemon is already running (PID {}). Stop it first with 'midtown stop'.",
                    pid
                ),
                None => format!(
                    "Another daemon is already running. Stop it first with 'midtown stop'. ({})",
                    e
                ),
            };

            Err(crate::Error::Io(std::io::Error::other(msg)))
        }
    }
}

async fn persist_sessions_for_restart(state: &DaemonState) -> crate::Result<()> {
    // Collect base session info (session_id, pid, last_active) from SessionManager
    let mut session_info = state.session_manager.collect_session_info().await;

    // Enrich with task/PR assignments and working directories from CoworkerManager
    let coworkers = state.coworkers.list();
    for coworker in coworkers {
        if let Some(info) = session_info.get_mut(&coworker.name) {
            info.working_dir = Some(coworker.working_dir.clone());
            info.provider = Some(coworker.provider);
            info.profile = Some(coworker.profile.clone());

            // Determine coworker type and assignment based on current_task and PR assignment
            // Check if this coworker is assigned as a reviewer via active spans
            let is_reviewer = {
                let persistent = state.persistent_state.lock().await;
                let reviewer_span = persistent
                    .active_reviewer_sessions()
                    .into_iter()
                    .find(|s| s.name == coworker.name)
                    .map(|s| (s.task_id.clone(),));
                if let Some((task_id,)) = reviewer_span {
                    info.coworker_type = Some("reviewer".to_string());
                    let pr_num = task_id.as_ref().and_then(|tid| {
                        persistent
                            .sessions
                            .values()
                            .find(|s| s.task_id.as_deref() == Some(tid))
                            .and_then(|s| s.pr_number)
                    });
                    if let Some(pr_num) = pr_num {
                        info.pr_number = Some(pr_num);
                        info.purpose = format!("reviewer for PR #{}", pr_num);
                    } else {
                        info.purpose = "reviewer (unassigned)".to_string();
                    }
                    true
                } else {
                    false
                }
            };
            if !is_reviewer {
                // Regular dev coworker
                info.coworker_type = Some("dev".to_string());
                if let Some(task_str) = &coworker.current_task {
                    // Parse task ID from string like "!42" or "42"
                    let task_id: Option<u64> = task_str.trim_start_matches('!').parse().ok();
                    info.task_id = task_id;
                    info.purpose = format!("task {}", task_str);
                } else {
                    info.purpose = "dev (no task)".to_string();
                }
            }
        }
    }

    // Save enriched session info to persistent state.
    // Update SessionRecords (the primary store) with fresh runtime data from running sessions.
    {
        let mut persistent = state.persistent_state.lock().await;
        let mut running_count = 0usize;

        // Collect sessions that are currently marked running before we reset them.
        // Any that remain false after re-marking will need their spans closed.
        let previously_running: Vec<String> = persistent
            .sessions
            .values()
            .filter(|r| r.is_running)
            .map(|r| r.session_id.clone())
            .collect();

        // Mark all existing session records as not running by default.
        // Running sessions will be re-marked below.
        for record in persistent.sessions.values_mut() {
            record.is_running = false;
            record.resume_on_startup = false;
            record.pid = None;
        }

        for (name, info) in &session_info {
            running_count += 1;
            // Look up the correct SessionRecord by session_id (unique) rather than
            // by name (ambiguous — multiple historical records share the same name).
            let record = if let Some(ref sid) = info.session_id {
                persistent.sessions.get_mut(sid)
            } else {
                persistent.session_by_name_mut(name)
            };
            if let Some(record) = record {
                record.is_running = true;
                record.resume_on_startup = true;
                record.pid = info.pid;
                record.last_active = info.last_active;
                if let Some(ref wd) = info.working_dir {
                    record.working_dir = wd.clone();
                }
                if let Some(provider) = info.provider {
                    record.provider = Some(provider);
                }
                if let Some(ref profile) = info.profile {
                    record.profile = Some(profile.clone());
                }
                if info.initial_prompt.is_some() {
                    record.initial_prompt = info.initial_prompt.clone();
                }
            }
        }

        // Close spans for sessions that were running but are no longer found alive.
        for session_id in &previously_running {
            if persistent
                .sessions
                .get(session_id)
                .is_some_and(|r| !r.is_running)
            {}
        }

        persistent.save_for_repo(state.paths.dir_key())?;
        info!(
            "Persisted {} running session(s); {} total session record(s) retained",
            running_count,
            persistent.sessions.len()
        );
    }

    Ok(())
}

/// Run the full prepare→evaluate→execute pipeline for a daemon event.
async fn run_tick(event: &events::DaemonEvent, state: &DaemonState) {
    let tasks = tick::prepare_tick(state).await;

    // For RateLimitCheckTick, fetch fresh rate limit data before evaluation
    if matches!(event, events::DaemonEvent::RateLimitCheckTick) {
        let fresh = crate::github_rate_limit::GitHubRateLimit::fetch().await;
        let mut ps = state.persistent_state.lock().await;
        ps.tick_fresh_rate_limit = fresh;
    }

    // For NoteReviewTick, populate stale channel notes (hourly, not on hot path)
    if matches!(event, events::DaemonEvent::NoteReviewTick) {
        let mut ps = state.persistent_state.lock().await;
        let base_dir = crate::paths::projects_dir_for_repo(&ps.tick_dir_key);
        let threshold = chrono::Duration::hours(crate::channel::NOTE_STALENESS_THRESHOLD_HOURS);
        ps.tick_stale_channel_notes =
            crate::channel::find_stale_notes(&base_dir, ps.tick_now, threshold);
    }

    let tick_effects = events::evaluate_tick(event, &tasks, state).await;
    effects::execute_effects(tick_effects, state).await;
}

/// Run the daemon server with the given configuration.
///
/// This function will block until the daemon receives a shutdown signal
/// (SIGTERM, SIGINT, or exec-restart RPC).
///
/// Returns `DaemonExitStatus` to indicate whether the caller should exit
/// or re-exec the daemon binary (for sandbox-safe restarts).
pub async fn run(config: DaemonConfig) -> crate::Result<DaemonExitStatus> {
    // Install panic hook so unhandled panics are logged to both stderr AND daemon.log.
    // The daemon often runs detached (stderr is lost), so writing to the log file
    // ensures panics are visible for post-mortem debugging.
    let panic_log_path = crate::paths::daemon_log_file();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let panic_msg = format!("=== DAEMON PANIC at {} ===\n{}\n", timestamp, info);
        eprintln!("{}", panic_msg);
        // Also append to daemon.log so the panic is visible even when stderr is lost
        if let Ok(mut f) = std::fs::File::options()
            .append(true)
            .create(true)
            .open(&panic_log_path)
        {
            let _ = std::io::Write::write_all(&mut f, panic_msg.as_bytes());
        }
        default_hook(info);
    }));

    // Initialize logging — write to daemon.log file
    let filter = std::env::var("MIDTOWN_LOG_LEVEL")
        .ok()
        .unwrap_or_else(|| if config.verbose { "debug" } else { "info" }.to_string());
    let log_path = crate::paths::daemon_log_file();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::File::options()
        .append(true)
        .create(true)
        .open(&log_path)
        .expect("Failed to open daemon.log");
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(log_file)
        .with_ansi(false)
        .init();

    // Check for sandbox nesting early — prevents crash loop when daemon
    // is started from within a sandboxed session (2026-02-13 incident).
    if let Some(warning) = startup::check_sandbox_context() {
        warn!("{}", warning);
        // Log to stderr as well so it's visible when daemon is started interactively
        eprintln!("\n{}\n", warning);
    }

    // Ensure required plugins are installed (non-blocking, logs warnings on failure)
    check_required_plugins().await;

    // Get token for configured GitHub user and set GH_TOKEN env var.
    // This is faster and more reliable than `gh auth switch`:
    // - No global state modification (env var is process-local)
    // - No race conditions with other processes
    // - Token is fetched once, inherited by all child processes
    if let Some(ref github_user) = config.github_user {
        info!("Fetching gh CLI token for user: {}", github_user);
        let output = std::process::Command::new("gh")
            .args(["auth", "token", "--user", github_user])
            .output()
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Failed to run `gh auth token --user {}`: {}",
                    github_user, e
                ),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "gh auth token --user {} failed: {}. Is the user logged in? Run `gh auth login` first.",
                    github_user,
                    stderr.trim()
                ),
            });
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "gh auth token --user {} returned empty token. Is the user logged in?",
                    github_user
                ),
            });
        }
        // Set GH_TOKEN so all child `gh` processes use this token automatically.
        // SAFETY: This is called during single-threaded daemon startup before the
        // async runtime spawns any tasks, so no data races are possible.
        unsafe {
            std::env::set_var("GH_TOKEN", &token);
        }
        info!(
            "Set GH_TOKEN for user: {} (token length: {})",
            github_user,
            token.len()
        );

        // Validate that the github_user has access to this repository.
        // This catches misconfigured github_user early with a clear error,
        // rather than having mysterious failures during polling.
        validate_github_repo_access(github_user, &config.workdir)?;
    }

    // Acquire exclusive lock on PID file to enforce singleton behavior
    let pid_file = acquire_pid_lock(&config.pid_file_path, &config.workdir)?;
    info!("Acquired PID lock: {}", config.pid_file_path.display());

    // Ensure parent directory exists for socket
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Derive repo name using git-aware detection (handles worktrees correctly)
    let repo_name = crate::paths::detect_repo_name().unwrap_or_else(|| {
        // Fallback to workdir name if not in a git repo
        config
            .workdir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });

    // Construct ProjectPaths — carries both dir_key and project_name.
    let paths = crate::paths::ProjectPaths::new(&repo_name);
    let project_name = config
        .project_name
        .clone()
        .unwrap_or_else(|| paths.project_name().to_string());
    // Re-create paths with the final project_name (may differ from auto-derived if --project was used)
    let paths = crate::paths::ProjectPaths::with_project_name(paths.dir_key(), &project_name);
    info!(
        "Project: dir_key={}, project_name={}",
        paths.dir_key(),
        paths.project_name()
    );

    // Load project config once — used for project name, repo paths, and worktree managers.
    let full_project_config = crate::config::load_full_project_config(paths.dir_key());

    // Create channel router for the repo
    let channel_base_dir = crate::paths::projects_dir_for_repo(paths.dir_key());
    let channel_router = crate::ChannelRouter::new(&channel_base_dir, paths.project_name());
    info!("Channel base: {}", channel_base_dir.display());

    // Create seed channels if configured
    if let Some(ref full_config) = full_project_config {
        for seed_channel in &full_config.channels.seed {
            match crate::channel::Channel::create(&channel_base_dir, seed_channel) {
                Ok(_) => {
                    debug!("Seed channel '{}' ready", seed_channel);
                }
                Err(e) => {
                    warn!("Failed to create seed channel '{}': {}", seed_channel, e);
                }
            }
        }
        if !full_config.channels.seed.is_empty() {
            info!(
                "Created {} seed channels: {:?}",
                full_config.channels.seed.len(),
                full_config.channels.seed
            );
        }
    }

    // Create worktree manager and coworker manager early so they can be
    // shared with the web server (for the /api/status endpoint).
    // Worktree initialization happens BEFORE socket binding so the daemon is
    // fully ready when clients can connect (tests rely on this ordering).
    // Build list of all repo paths for multi-repo PR fetching.
    // Built early because it's needed by the web server, daemon state, and lead health checks.
    let all_repo_paths: Vec<PathBuf> = {
        let mut paths = vec![config.workdir.clone()];
        if let Some(ref full_config) = full_project_config {
            for repo in full_config.project.repos() {
                let path = PathBuf::from(repo);
                if path != config.workdir {
                    paths.push(path);
                }
            }
        }
        paths
    };

    let worktree_manager =
        WorktreeManager::new(config.workdir.clone()).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to initialize worktree manager: {}", e),
        })?;

    // Create the lead worktree (or reuse existing one)
    match worktree_manager.create_lead_worktree() {
        Ok(path) => info!("Lead worktree ready at {}", path.display()),
        Err(e) => warn!(
            "Failed to create lead worktree, falling back to main repo: {}",
            e
        ),
    };

    // For multi-repo projects, create worktree managers for additional repos
    let additional_worktree_managers =
        load_additional_worktree_managers(full_project_config.as_ref(), &config);

    // Bind socket AFTER all initialization is complete so clients (and tests)
    // can assume the daemon is fully ready when the socket becomes connectable.
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }
    let listener = UnixListener::bind(&config.socket_path)?;
    info!("Listening on {}", config.socket_path.display());
    let coworker_manager =
        CoworkerManager::with_additional_repos(worktree_manager, additional_worktree_managers);

    // Start webhook server and gh forwarder watchdog if configured
    let mut webhook_rx = None;
    let mut web_updates_tx = None;
    let mut mobile_rx: Option<tokio::sync::mpsc::Receiver<crate::web::MobileChannelPost>> = None;
    let mut web_command_rx: Option<tokio::sync::mpsc::Receiver<crate::web::WebCommand>> = None;
    let mut shared_push_manager: Option<std::sync::Arc<crate::push::PushManager>> = None;
    let (forwarder_shutdown_tx, forwarder_shutdown_rx) = watch::channel(false);

    // Detect the default branch early so it's available for both the webhook server and daemon state
    let default_branch = all_repo_paths
        .first()
        .and_then(|path| crate::worktree::detect_default_branch(path))
        .unwrap_or_else(|| "main".to_string());
    info!("Detected default branch: {}", default_branch);

    if let Some(port) = config.webhook_port {
        let webhook_config = WebhookConfig {
            port,
            secret: config.webhook_secret.clone(),
            dir_key: paths.dir_key().to_string(),
            project_name: paths.project_name().to_string(),
        };
        match start_webhook_server(
            webhook_config,
            Some(coworker_manager.clone()),
            all_repo_paths.clone(),
            default_branch.clone(),
            config.max_in_progress_tasks,
        )
        .await
        {
            Ok((rx, updates_tx, mob_rx, push_mgr, cmd_rx)) => {
                info!("Webhook server started on port {}", port);
                webhook_rx = Some(rx);
                web_updates_tx = Some(updates_tx);
                mobile_rx = Some(mob_rx);
                shared_push_manager = push_mgr;
                web_command_rx = Some(cmd_rx);

                // Spawn webhook forwarder watchdog task
                let restart_interval = config.webhook_restart_interval_secs;
                tokio::spawn(webhook_fwd::webhook_forwarder_watchdog(
                    port,
                    restart_interval,
                    forwarder_shutdown_rx,
                ));
            }
            Err(e) => {
                error!("Failed to start webhook server: {}", e);
            }
        }
    } else {
        debug!("Webhook server disabled (no port configured)");
    }

    // Set up shutdown signal handler (created before state so it can be shared)
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Create the aggregated session event channel. The receiver stays local
    // to the event loop (mpsc::UnboundedReceiver is !Sync, so it cannot live
    // inside Arc<DaemonState>). The sender is passed into DaemonState for use
    // by SessionManager when spawning forwarder tasks.
    let (session_agg_tx, mut session_agg_rx) = session_events::channel();

    // Create daemon state (pass channel and web updates sender so messages
    // are broadcast to WebSocket clients in real-time)
    let state = Arc::new(DaemonState::new(
        config.socket_path.clone(),
        coworker_manager,
        paths.clone(),
        all_repo_paths,
        channel_router,
        web_updates_tx,
        config.max_in_progress_tasks,
        shared_push_manager,
        default_branch,
        shutdown_tx.clone(),
        session_agg_tx,
    )?);
    info!(
        "Max in-progress tasks limit: {}",
        config.max_in_progress_tasks
    );

    // Recover coworker workflow state from their state files across daemon restarts.
    startup::recover_coworker_records(paths.dir_key(), &state.coworkers, &state.coworker_records)
        .await;

    // Collect PIDs of sessions we intend to recover BEFORE running the zombie scanner.
    // The scanner must skip these — they are intentionally detached processes that
    // will die naturally from broken pipes. Killing them before session recovery
    // runs defeats session survival across daemon restarts.
    let session_pids_to_preserve = startup::recoverable_session_pids(&state.persistent_state).await;

    // Kill any zombie Claude headless processes left from crashes or unclean shutdowns.
    // Kills processes that are truly orphaned (PPID=1) OR are children of a stale
    // midtown daemon (a midtown process that is not the current daemon).
    // Excludes session-survival PIDs collected above.
    startup::kill_zombie_claude_processes(std::process::id(), &session_pids_to_preserve);

    // CRITICAL: Restore task assignments from disk BEFORE session recovery.
    // Backfills sessions[].task_id from in_progress tasks with owners so that
    // dispatch can see the assignments before any ticks fire.
    state.restore_task_assignments_from_disk().await;

    // Pre-register recovering coworker names so dispatch doesn't double-assign.
    // This creates CoworkerRecords for coworkers about to be resumed, ensuring
    // they appear in active_names before the first TaskDispatchTick fires.
    let recovering_names = startup::recovering_coworker_names(&state.persistent_state).await;
    if !recovering_names.is_empty() {
        let mut records = state.coworker_records.write().await;
        for name in &recovering_names {
            if !records.contains_key(name) {
                info!("Pre-registering recovering coworker: {}", name);
                records.insert(name.to_string(), crate::rules::CoworkerRecord::new_spawn());
            }
        }
    }

    // Check Claude auth status before spawning any sessions.
    // Gives immediate feedback in the log if auth is expired/missing.
    startup::check_claude_auth_status(paths.dir_key());

    // Recover coworker sessions from session records. Channel leads are recovered
    // separately below.
    let (session_recovery_effects, recovered_session_ids) =
        startup::recover_from_session_records(&state.persistent_state, paths.dir_key()).await;
    if !session_recovery_effects.is_empty() {
        info!(
            "Executing {} session record recovery effect(s)",
            session_recovery_effects.len()
        );
        effects::execute_effects(session_recovery_effects, &state).await;
    }

    // Restore channel lead session mappings from persisted session records.
    // Channel leads are on-demand — they'll be spawned by triggers if absent.
    let _ = startup::recover_channel_lead_session_mappings(&state.persistent_state).await;
    {
        let ps = state.persistent_state.lock().await;
        if let Err(e) = ps.save_for_repo(paths.dir_key()) {
            warn!(
                "Failed to save recovered channel lead mappings on startup: {}",
                e
            );
        }
    }

    // Clear stale is_running flags for sessions that were not recovered.
    // Sessions with is_running=true but resume_on_startup=false (e.g., reviewers,
    // manually-stopped sessions) are skipped by recover_from_session_records but
    // retain their stale flag — causing dispatch to think they're still active.
    // Channel leads are included — they are on-demand and not recovered at startup.
    startup::clear_stale_running_sessions(&state.persistent_state, &recovered_session_ids).await;
    {
        let ps = state.persistent_state.lock().await;
        if let Err(e) = ps.save_for_repo(paths.dir_key()) {
            warn!(
                "Failed to save persistent state after clearing stale session flags: {}",
                e
            );
        }
    }

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    // Subscribe to shutdown broadcasts (triggered by RPC exec-restart handler)
    let mut shutdown_rx = shutdown_tx.subscribe();

    // Set up idle check interval
    let mut idle_check_interval = interval(IDLE_CHECK_INTERVAL);

    // Timer for periodic PR polling (integrated into main loop to prevent spawn races)
    let mut pr_poll_interval =
        tokio::time::interval(std::time::Duration::from_secs(config.pr_poll_interval_secs));
    // Skip the first tick (which fires immediately)
    pr_poll_interval.tick().await;
    info!(
        "PR polling interval set to {}s (in main event loop)",
        config.pr_poll_interval_secs
    );

    // Start chat monitor background task if enabled
    let (chat_monitor_shutdown_tx, chat_monitor_shutdown_rx) = watch::channel(false);
    if config.chat_monitor_enabled {
        let state = Arc::clone(&state);
        let channel_path = match state.channel_router.default_channel() {
            Ok(ch) => ch.channel_file_path().to_path_buf(),
            Err(e) => {
                error!("Failed to get default channel for chat monitor: {}", e);
                return Err(e);
            }
        };
        tokio::spawn(async move {
            chat::chat_monitor_loop(state, channel_path, chat_monitor_shutdown_rx).await;
        });
        info!("Chat monitor started (tailf on channel.jsonl)");
    } else {
        debug!("Chat monitor disabled (MIDTOWN_CHAT_MONITOR=0)");
    }

    // Timer for periodic orphan checking
    let mut orphan_check_interval =
        tokio::time::interval(std::time::Duration::from_secs(ORPHAN_CHECK_INTERVAL_SECS));
    // Skip the first tick (which fires immediately)
    orphan_check_interval.tick().await;

    // Timer for periodic GitHub API rate limit checks (every 2 minutes)
    let mut rate_limit_check_interval = tokio::time::interval(std::time::Duration::from_secs(120));
    // Skip the first tick (which fires immediately)
    rate_limit_check_interval.tick().await;
    info!("GitHub rate limit check interval set to 120s");

    // Timer for periodic GH_TOKEN refresh (every 5 minutes).
    // Picks up token changes if the user runs `gh auth login` or `gh auth refresh` externally.
    let gh_token_refresh_user = config.github_user.clone();
    let mut gh_token_refresh_interval = tokio::time::interval(std::time::Duration::from_secs(300));
    // Skip the first tick (token was just fetched at startup)
    gh_token_refresh_interval.tick().await;

    // Timer for periodic note staleness review (every hour)
    let mut note_review_interval = interval(NOTE_REVIEW_CHECK_INTERVAL);
    // Skip the first tick (which fires immediately)
    note_review_interval.tick().await;

    // Timer for periodic channel rotation
    let mut channel_rotation_interval = interval(CHANNEL_ROTATION_CHECK_INTERVAL);
    // Skip the first tick (which fires immediately)
    channel_rotation_interval.tick().await;

    // Timer for periodic orphan process cleanup (every 5 minutes)
    // This catches claude processes that were orphaned without going through
    // `midtown stop`.
    let mut orphan_process_interval = interval(std::time::Duration::from_secs(300));
    // Run cleanup immediately on startup, before the interval timer begins.
    // Orphans from a crashed/restarted daemon need to be killed before we
    // start spawning new coworkers.
    {
        let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
        let killed = crate::process::kill_orphaned_processes(pattern);
        if killed > 0 {
            info!(
                "Startup cleanup: killed {} orphaned claude process(es) from previous daemon",
                killed
            );
        }
    }
    orphan_process_interval.tick().await;

    // Timer for session health checks (every 5 seconds).
    // Event processing is now real-time via the session_agg_rx select! branch.
    // This interval only runs plugin health, health snapshot refresh, and
    // defense-in-depth process reconciliation.
    let mut session_health_interval = interval(std::time::Duration::from_secs(5));
    session_health_interval.tick().await;

    // Timer for flushing batched CI notifications (check every 5 seconds).
    // The actual flush delay is 15 seconds from the oldest buffered item.
    let mut ci_notification_flush_interval = interval(std::time::Duration::from_secs(5));
    // Skip the first tick (which fires immediately)
    ci_notification_flush_interval.tick().await;

    // Timer for re-scanning plugin directories (every 5 seconds).
    // Detects new plugins appearing, old plugins deleted, and directory
    // structure changes that require restarting the Python plugin daemon.
    let mut plugin_scan_interval = interval(std::time::Duration::from_secs(5));
    plugin_scan_interval.tick().await;

    // Spawn dedicated RPC listener task so connection acceptance is never
    // blocked by long-running tick handlers (PR polling, task dispatch, etc.).
    // Previously, listener.accept() was inside the main tokio::select! loop,
    // meaning a tick that takes >15s would cause RPC client timeouts.
    {
        let rpc_state = Arc::clone(&state);
        let rpc_shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            let mut shutdown_rx = rpc_shutdown_tx.subscribe();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _addr)) => {
                                debug!("New RPC connection");
                                let conn_shutdown = rpc_shutdown_tx.subscribe();
                                let conn_state = Arc::clone(&rpc_state);
                                tokio::spawn(rpc::handle_connection(stream, conn_shutdown, conn_state));
                            }
                            Err(e) => {
                                error!("RPC accept error: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });
    }

    // Spawn dedicated task for web/mobile channel posts so they aren't delayed
    // by heavier branches in the main select! loop (e.g., session drain, PR polling).
    if let Some(mut mob_rx) = mobile_rx.take() {
        let post_state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some(mobile_post) = mob_rx.recv().await {
                let content = &mobile_post.content;
                let channel = mobile_post.channel.as_deref();
                let thread_parent_id = mobile_post.thread_parent_id.as_deref();
                let sender = post_state.user_display_name.as_deref().unwrap_or("user");
                rpc_channel::handle_channel_post(
                    crate::rpc::RequestId::Null,
                    sender,
                    content,
                    channel,
                    thread_parent_id,
                    &post_state,
                )
                .await;
            }
        });
    }

    // Spawn dedicated task for web commands (archive/unarchive) that need
    // daemon-side processing with access to DaemonState.
    if let Some(mut cmd_rx) = web_command_rx.take() {
        let cmd_state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    crate::web::WebCommand::ArchiveChannel { name, response } => {
                        let resp = rpc_channel::handle_channel_archive(
                            crate::rpc::RequestId::Null,
                            &name,
                            &cmd_state,
                        )
                        .await;
                        let result = if resp.error.is_some() {
                            Err(resp
                                .error
                                .map(|e| e.message)
                                .unwrap_or_else(|| "Unknown error".into()))
                        } else {
                            Ok(())
                        };
                        let _ = response.send(result);
                    }
                    crate::web::WebCommand::UnarchiveChannel { name, response } => {
                        let resp = rpc_channel::handle_channel_unarchive(
                            crate::rpc::RequestId::Null,
                            &name,
                            &cmd_state,
                        );
                        let result = if resp.error.is_some() {
                            Err(resp
                                .error
                                .map(|e| e.message)
                                .unwrap_or_else(|| "Unknown error".into()))
                        } else {
                            Ok(())
                        };
                        let _ = response.send(result);
                    }
                }
            }
        });
    }

    // Main event loop
    loop {
        let state = Arc::clone(&state);

        tokio::select! {
            // Forward webhook messages to channel and nudge PR owners on comments
            Some(webhook_event) = async {
                match webhook_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!("Received webhook message: {}", webhook_event.message.content);

                // Record webhook event timestamp for adaptive PR polling.
                // The PR poll task reads this to decide between relaxed/aggressive intervals.
                {
                    let mut ts = state.last_webhook_event_at.lock().await;
                    *ts = Some(tokio::time::Instant::now());
                }

                // Buffer successful CI checks for batching; post other messages immediately.
                // When ci_check_passed is set, the webhook's `message` field is ignored in favor
                // of a later batched message (see WebhookEvent.ci_check_passed doc comment).
                let is_ci_check_passed = webhook_event.ci_check_passed.is_some();
                if let Some(ci_check) = webhook_event.ci_check_passed {
                    debug!("Buffering CI success for batching: {} on {}", ci_check.check_name, ci_check.target);

                    // Reviewer spawn retry on CI completion is now handled by the
                    // workflow script's pr.ci_passed handler calling rpc.spawn_reviewer().
                    // The polling backstop also catches PRs that need review.

                    let mut buffer = state.ci_notification_buffer.lock().await;
                    buffer.add(ci_check);
                } else if let Err(e) = state.send_and_broadcast_async(&webhook_event.message).await {
                    error!("Failed to forward webhook message to channel: {}", e);
                }

                // Detect and block external/fork PRs from webhook events.
                // If the PR is from a fork and not yet allowed, record it, notify,
                // and skip all downstream PR processing (reviewer spawn, session
                // storage, task association, etc.).
                if let Some(ref fork_repo) = webhook_event.fork_repo {
                    let pr_number = webhook_event.needs_review
                        .or(webhook_event.merged_pr)
                        .or(webhook_event.pr_opened.as_ref().map(|o| o.pr_number));

                    if let Some(pr_number) = pr_number {
                        let mut ps = state.persistent_state.lock().await;
                        let title = webhook_event.pr_opened.as_ref()
                            .map(|o| o.title.as_str())
                            .unwrap_or("");
                        let is_new = ps.github.record_external_pr(pr_number, fork_repo, title);

                        if ps.github.is_blocked_external_pr(pr_number) {
                            if is_new {
                                ps.github.mark_external_pr_notified(pr_number);
                                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                    warn!("Failed to persist external PR state: {}", e);
                                }
                                info!(
                                    "Webhook: Blocked external PR #{} from fork '{}'",
                                    pr_number, fork_repo
                                );
                                let default_ch = state.default_channel_name().to_string();
                                let channel = if default_ch.is_empty() { None } else { Some(default_ch) };
                                let msg = format!(
                                    "⚠️ PR #{} from fork `{}` is from an external repository. \
                                     External PRs are not processed automatically. \
                                     To allow it, run: `midtown pr allow {}`",
                                    pr_number, fork_repo, pr_number
                                );
                                effects::execute_effects(
                                    vec![effects::Effect::PostSystemMessage {
                                        message: msg,
                                        channel,
                                    }],
                                    &state,
                                ).await;
                            }
                            // Skip all downstream PR processing for this blocked external PR
                            continue;
                        }
                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                            warn!("Failed to persist external PR state: {}", e);
                        }
                    }
                }

                // Extract repo_full_name before pr_activity is moved (used later for placeholder cleanup)
                let webhook_repo_full_name = webhook_event
                    .pr_activity
                    .as_ref()
                    .and_then(|a| a.repo_full_name.clone());

                // Nudge PR owner when someone else comments on their PR
                if let Some(activity) = webhook_event.pr_activity {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        pr::handle_pr_comment_nudge(&state, activity).await;
                    });
                }

                // Record that this PR was handled by the webhook (for polling deference).
                // Reviewer spawning is now driven by the workflow script's pr.opened
                // handler calling rpc.spawn_reviewer(), not by the daemon's inline
                // pending_review_spawn queue.
                if let Some(pr_number) = webhook_event.needs_review {
                    let mut ps = state.persistent_state.lock().await;
                    ps.github.record_webhook_event(pr_number);
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("Failed to persist webhook event record: {}", e);
                    }
                    debug!(
                        "Webhook: PR #{} needs review — workflow script will handle spawning",
                        pr_number
                    );
                }

                // Handle PR-opened events: store author session + auto-set task PR association
                if let Some(ref pr_opened) = webhook_event.pr_opened {
                    let mut pr_effects = Vec::new();

                    // Store author session for PR handoff (allows any coworker to resume the PR)
                    if let Some(ref author) = pr_opened.author_coworker {
                        if let Some(session_id) = state.coworkers.get_session_id(author) {
                            pr_effects.push(effects::Effect::LinkPrToSession {
                                pr_number: pr_opened.pr_number,
                                session_id,
                                branch: pr_opened.branch.clone(),
                                author: author.clone(),
                                title: pr_opened.title.clone(),
                            });
                        } else {
                            debug!(
                                "PR #{} author {} has no known session ID (discovered coworker?)",
                                pr_opened.pr_number, author
                            );
                        }

                        // Auto-merge warning is now sent by the workflow script's
                        // pr.opened handler (policy, not mechanism).
                    }

                    // Auto-set task PR association when PR title contains [Midtown !XX]
                    if let Some(task_id) =
                        crate::task_store::extract_task_id_from_pr_title(&pr_opened.title)
                    {
                        pr_effects.push(effects::Effect::SetTaskPr {
                            task_id: task_id.to_string(),
                            pr_number: pr_opened.pr_number,
                            dir_key: state.paths.dir_key().to_string(),
                        });
                        info!(
                            "Auto-setting PR #{} association for task !{}",
                            pr_opened.pr_number, task_id
                        );

                        // Emit PrOpened workflow event if we know the task's channel.
                        if let Some(author) = &pr_opened.author_coworker {
                            let task_channel = state
                                .task_store
                                .load(&task_id.to_string())
                                .ok()
                                .and_then(|t| t.channel);
                            if let Some(ch) = task_channel {
                                pr_effects.push(effects::Effect::EmitWorkflowEvent(
                                    crate::workflow::WorkflowEvent::PrOpened {
                                        channel: ch,
                                        task_id: task_id.to_string(),
                                        pr_number: pr_opened.pr_number,
                                        coworker: author.clone(),
                                    },
                                ));
                            }
                        }
                    }

                    // NOTE: Task auto-completion has been moved to the PR merged handler
                    // to avoid completing tasks before review feedback is addressed and CI passes.

                    if !pr_effects.is_empty() {
                        effects::execute_effects(pr_effects, &state).await;
                    }
                }

                // Nudge lead to pull main when a PR merges
                if let Some(pr_number) = webhook_event.merged_pr {
                    // Channel message is informational only (no @lead to avoid
                    // chat monitor triggering a duplicate nudge)
                    let channel_text = format!(
                        "PR #{} merged into {}.",
                        pr_number, state.default_branch
                    );
                    let channel_msg = Message::text("midtown", channel_text);
                    if let Err(e) = state.send_and_broadcast_async(&channel_msg).await {
                        warn!("Failed to post merge notification for PR #{}: {}", pr_number, e);
                    }
                    // Direct nudge includes the actionable instruction
                    let nudge_text = format!(
                        "PR #{} merged into {}. Run `git pull` to stay current.",
                        pr_number, state.default_branch
                    );
                    state.nudge_lead(&nudge_text).await;
                    info!("Nudged lead about PR #{} merge", pr_number);

                    // Auto-complete task when PR title contains [Midtown !XX].
                    // Task completion sends its own push notification, so skip
                    // the generic "PR merged" push for task-linked PRs.
                    let mut task_handled = false;
                    if let Some(pr_merged_info) = webhook_event.pr_merged_info {
                        // Look up task context for workflow event routing.
                        // File I/O (read_task_for_repo) happens before acquiring the
                        // async mutex to avoid blocking other tasks that need the lock.
                        let (task_channel, task_event_ctx) = if let Some(task_id) =
                            crate::task_store::extract_task_id_from_pr_title(&pr_merged_info.title)
                        {
                            let task_id_str = task_id.to_string();
                            let task = state.task_store.load(&task_id_str).ok();
                            let channel = task.as_ref().and_then(|t| t.channel.clone());
                            let ctx = dispatch::TaskEventContext {
                                subject: task.as_ref().map(|t| t.subject.clone()),
                                description: task.as_ref().and_then(|t| t.description.clone()),
                                thread_id: task.as_ref().and_then(|t| t.thread_id.clone()),
                                message_id: task.and_then(|t| t.message_id),
                            };
                            (channel, Some(ctx))
                        } else {
                            (None, None)
                        };
                        let completion_effects = dispatch::build_task_completion_effects(
                            &pr_merged_info.title,
                            pr_merged_info.pr_number,
                            state.paths.dir_key(),
                            &state.project_name,
                            task_channel,
                            task_event_ctx,
                        );
                        if !completion_effects.is_empty() {
                            task_handled = true;
                            effects::execute_effects(completion_effects, &state).await;
                        }
                    }

                    // Push notification for non-Midtown PR merges only.
                    // Task-linked PRs get a "Task !XX completed" notification
                    // from task_completed_effects instead.
                    if !task_handled {
                        let push_body = format!(
                            "PR #{} merged into {}",
                            pr_number, state.default_branch
                        );
                        let push_url = dispatch::build_push_deep_link(
                            &state.project_name,
                            &state.project_name,
                            None,
                            None,
                        );
                        state.send_push_notification(
                            &format!("PR #{} merged", pr_number),
                            &push_body,
                            &format!("pr_merged_{}", pr_number),
                            Some(&push_url),
                        );
                    }
                }

                // Nudge lead when a CI check fails on the default branch
                if let Some(nudge_msg) = webhook_event.ci_failed_on_default_branch {
                    state.nudge_lead(&nudge_msg).await;
                    info!("Nudged lead about CI failure on default branch");
                }

                // Capture whether this is a strong formal review (APPROVED/CHANGES_REQUESTED)
                // before review_state_change is moved. Used below for review identity matching.
                let is_strong_formal_review = webhook_event.review_state_change.as_ref().is_some_and(|r| {
                    matches!(r.state, crate::webhook::ReviewState::Approved | crate::webhook::ReviewState::ChangesRequested)
                });

                // Nudge PR owner immediately on review state change (approved / changes_requested)
                if let Some(review_change) = webhook_event.review_state_change {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        pr::handle_webhook_review_state_change(&state, review_change).await;
                    });
                }

                // Nudge PR owner immediately on CI failure
                if let Some(ci_failure) = webhook_event.pr_ci_failure {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        pr::handle_webhook_ci_failure(&state, ci_failure).await;
                    });
                }

                // Cache review status immediately from webhook data (avoids API calls).
                //
                // Only mark as reviewed if the review author matches the assigned
                // reviewer. This prevents bot comments or unrelated formal reviews
                // from prematurely marking a PR as "reviewed and CI green" while
                // the assigned reviewer is still working. (Bug fix for !1924)
                if let Some(pr_number) = webhook_event.reviewed_pr {
                    let (assigned_reviewer, assigned_session_id) = {
                        let ps = state.persistent_state.lock().await;
                        let span = ps.active_reviewer_for_pr(pr_number);
                        let reviewer = span.map(|s| s.name.clone());
                        let session_id = span.map(|s| s.session_id.clone());
                        (reviewer, session_id)
                    };

                    let author_matches = match (&webhook_event.review_author, &assigned_reviewer) {
                        (Some(author), Some(reviewer)) => {
                            // Match by name (legacy) or by session ID (new format)
                            author.eq_ignore_ascii_case(reviewer)
                                || assigned_session_id.as_ref().is_some_and(|sid| sid == author)
                        }
                        (None, Some(_)) => {
                            // Review detected but author unknown — accept if it's
                            // a strong formal review (APPROVED/CHANGES_REQUESTED),
                            // reject weak states (COMMENTED/DISMISSED) that bots produce.
                            // The assigned reviewer may submit APPROVED with empty body.
                            is_strong_formal_review
                        }
                        (_, None) => {
                            // No assigned reviewer — don't cache from webhook alone.
                            // Without an assigned reviewer, we can't verify the review
                            // is from a midtown coworker. Let the polling path handle it
                            // (which checks for midtown type:review frontmatter).
                            false
                        }
                    };

                    if author_matches {
                        debug!(
                            "Webhook: caching review status for PR #{} (review by {:?}, assigned: {:?})",
                            pr_number, webhook_event.review_author, assigned_reviewer
                        );
                        let mut ps = state.persistent_state.lock().await;
                        ps.github.mark_reviewed_pr(pr_number);
                        // Persist review comment ID for Gate 3 merge gating
                        if let Some(comment_id) = webhook_event.review_comment_id {
                            debug!(
                                "Webhook: recording review comment ID {} for PR #{}",
                                comment_id, pr_number
                            );
                            ps.github.add_review_comment_id(pr_number, comment_id);
                        }
                        drop(ps);
                        // Clear placeholder cache: review is done
                        let mut placeholder_cache =
                            state.reviewer_placeholder_cache.lock().unwrap();
                        placeholder_cache.remove(&pr_number);
                        // Backstop: clean up stale review placeholder comments
                        // on GitHub. Only runs when author_matches to avoid
                        // deleting placeholders for in-flight reviewers.
                        if let Some(repo) = webhook_repo_full_name {
                            tokio::spawn(async move {
                                pr::cleanup_review_placeholders(pr_number, &repo).await;
                            });
                        }
                        // Route review feedback to the author task immediately
                        // (don't wait for the ~2min polling cycle).
                        let state_for_review = Arc::clone(&state);
                        tokio::spawn(async move {
                            pr::handle_webhook_review_complete(&state_for_review, pr_number)
                                .await;
                        });
                    } else {
                        debug!(
                            "Webhook: ignoring review for PR #{} — author {:?} does not match assigned reviewer {:?}",
                            pr_number, webhook_event.review_author, assigned_reviewer
                        );
                    }
                }

                // Record CI check duration for statistics tracking
                if let Some(duration) = webhook_event.check_duration {
                    debug!(
                        "Webhook: recording CI check duration for '{}': {}s",
                        duration.check_name, duration.duration_secs
                    );
                    let mut ps = state.persistent_state.lock().await;
                    ps.ci_stats.record_duration(&duration.check_name, duration.duration_secs);
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("Failed to save daemon-state.json after recording CI duration: {}", e);
                    }
                }

                // Route @mentions in webhook messages directly (chat monitor skips
                // "github" sender for loop protection, so we handle it here).
                // Skip CI success notifications — they're informational and should
                // not trigger coworker call-ins. Without this guard, @mentions in
                // messages like "@madison Check 'build' passed on PR #99" cause a
                // spawn loop: coworker called in → goes idle → next CI check
                // @mention triggers another call-in.
                if !is_ci_check_passed {
                    chat::route_mentions(&state, &webhook_event.message).await;
                }
            }

            // Real-time session event processing via aggregated channel.
            Some(first_event) = session_agg_rx.recv() => {
                // Batch drain: grab any other events already buffered
                let mut batch = vec![first_event];
                while let Ok(ev) = session_agg_rx.try_recv() {
                    batch.push(ev);
                }
                handle_session_event_batch(batch, &state).await;
            }

            // Periodic health checks and process reconciliation.
            // Event processing is now real-time via the session_agg_rx branch above.
            _ = session_health_interval.tick() => {
                // Plugin health
                state.plugin_daemon.check_health().await;
                if state.plugin_daemon.has_plugins() {
                    state.plugin_daemon.ensure_running().await;
                }

                // Refresh health snapshot for decision functions
                let health = state.session_manager.collect_health().await;
                {
                    let mut hh = state.headless_health.write().unwrap();
                    *hh = health;
                }
                state.headless_health_generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Defense-in-depth: reconcile process liveness
                let (reconciled, reconciled_stderr) =
                    state.session_manager.reconcile_process_health().await;
                if !reconciled.is_empty() {
                    warn!(
                        "Process reconciliation found {} dead session(s): {:?}",
                        reconciled.len(), reconciled
                    );
                    for name in &reconciled {
                        let stderr_lines = reconciled_stderr
                            .get(name)
                            .cloned()
                            .unwrap_or_default();
                        handle_session_stopped(name, &stderr_lines, &state).await;
                    }
                }
            }

            // Periodically monitor coworker sessions: idle shutdown, nudges, stuck detection
            _ = idle_check_interval.tick() => {
                // Sync internal state: remove coworkers that are no longer alive
                // in the session manager. With all sessions headless, the session
                // manager is the source of truth for liveness.
                let alive_names: std::collections::HashSet<String> =
                    state.session_manager.list_alive_names().await.into_iter().collect();
                state.coworkers.retain_alive(&alive_names);
                run_tick(&events::DaemonEvent::SessionMonitorTick, &state).await;
            }


            // Periodic task dispatch: orphan recovery, duplicate detection, spawning, cleanup
            _ = orphan_check_interval.tick() => {
                let tasks = tick::prepare_tick(&state).await;
                let tick_effects = events::evaluate_tick(&events::DaemonEvent::TaskDispatchTick, &tasks, &state).await;
                // Mark in-flight tasks BEFORE executing effects to prevent race conditions.
                // If the next tick fires while effects are executing, it will skip these tasks.
                state.mark_in_flight_spawns_from_effects(&tick_effects);
                effects::execute_effects(tick_effects, &state).await;
                // Stale branch cleanup: gather data on the main thread (lightweight
                // in-memory checks including cooldown recording), then fire-and-forget
                // the actual git operations in a background task. Recording the cooldown
                // here prevents double-dispatch if the next tick fires before the
                // background task starts.
                let task_owners: Vec<String> = {
                    let ps = state.persistent_state.lock().await;
                    ps.tick_in_progress_tasks
                        .iter()
                        .map(|(_, _, owner)| owner.clone())
                        .collect()
                };
                if let Some(cleanup_data) =
                    dispatch::gather_stale_branch_cleanup_data(&state, &task_owners).await
                {
                    let cleanup_state = state.clone();
                    tokio::spawn(async move {
                        let cleanup_effects =
                            dispatch::decide_stale_branch_cleanup(&cleanup_data);
                        effects::execute_effects(cleanup_effects, &cleanup_state).await;
                    });
                }
            }

            // Periodic channel log rotation (rotates all active channels)
            _ = channel_rotation_interval.tick() => {
                let base_dir = state.paths.base_dir().to_path_buf();
                let all_channels = crate::channel::Channel::list(&base_dir, false, None)
                    .unwrap_or_default();
                for channel_info in all_channels {
                    let ch = match crate::channel::Channel::new(&base_dir, &channel_info.name) {
                        Ok(ch) => ch,
                        Err(e) => {
                            error!("Failed to open channel '{}' for rotation: {}", channel_info.name, e);
                            continue;
                        }
                    };
                    if ch.needs_rotation(CHANNEL_ROTATION_MAX_AGE_HOURS) {
                        info!("Channel '{}' rotation triggered (oldest message > {}h)", channel_info.name, CHANNEL_ROTATION_MAX_AGE_HOURS);
                        match ch.rotate(CHANNEL_ROTATION_RETAIN_MINUTES) {
                            Ok(archived) if archived > 0 => {
                                info!("Channel '{}' rotated: {} messages archived", channel_info.name, archived);
                                let mut msg = Message::system(format!(
                                    "Channel '{}' log rotated: {} old messages archived",
                                    channel_info.name, archived
                                ));
                                msg.channel = Some(OPS_CHANNEL.to_string());
                                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                                    warn!("Failed to send rotation notification: {}", e);
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                error!("Channel '{}' rotation failed: {}", channel_info.name, e);
                            }
                        }
                    }
                }
            }

            // Periodic orphan process cleanup: kill claude processes that were
            // orphaned (PPID=1) when sessions were killed directly.
            _ = orphan_process_interval.tick() => {
                // Only kill truly orphaned processes (PPID=1) to avoid killing
                // claude sessions the user started manually or in other projects.
                let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
                let killed = crate::process::kill_orphaned_processes(pattern);
                if killed > 0 {
                    info!("Cleaned up {} orphaned claude process(es)", killed);
                }
            }

            // Flush batched CI notifications: aggregate success checks by target
            // into single messages to reduce channel noise.
            _ = ci_notification_flush_interval.tick() => {
                let mut buffer = state.ci_notification_buffer.lock().await;
                if buffer.should_flush() {
                    let batched = buffer.flush();
                    for batch in batched {
                        let msg = trackers::format_batched_ci_notification(&batch);
                        let message = Message::for_channel("ops", "github", msg, crate::message::MessageType::Text);
                        if let Err(e) = state.send_and_broadcast_async(&message).await {
                            error!("Failed to post batched CI notification: {}", e);
                        }
                    }
                }
            }

            // Periodic plugin daemon health check and reload: ensure the daemon
            // is running and tell it to reload changed workflow files.
            _ = plugin_scan_interval.tick() => {
                // Re-scan workflows_dir for workflow subdirectories so has_plugins()
                // stays current as workflows are added/removed on disk.
                state.plugin_daemon.refresh_has_plugins().await;
                if state.plugin_daemon.has_plugins() {
                    state.plugin_daemon.ensure_running().await;
                    state.plugin_daemon.send_reload().await;
                }
            }

            // Periodic PR polling: check open PRs for issues, spawn reviewers.
            // Integrated into main loop (not a separate task) to prevent spawn
            // races with TaskDispatchTick - both now share the same snapshot.
            _ = pr_poll_interval.tick() => {
                run_tick(&events::DaemonEvent::PrPollTick, &state).await;
            }

            // Periodic GitHub rate limit check: fetch current quotas and update state.
            // Runs every 2 minutes to monitor API consumption for adaptive throttling.
            _ = rate_limit_check_interval.tick() => {
                run_tick(&events::DaemonEvent::RateLimitCheckTick, &state).await;
            }

            // Periodic note staleness review: check for stale notes and nudge leads.
            _ = note_review_interval.tick() => {
                run_tick(&events::DaemonEvent::NoteReviewTick, &state).await;
            }

            // Periodic GH_TOKEN refresh: re-run `gh auth token` to pick up
            // externally refreshed credentials (e.g., user ran `gh auth login`).
            _ = gh_token_refresh_interval.tick() => {
                if let Some(ref github_user) = gh_token_refresh_user {
                    let user = github_user.clone();
                    tokio::task::spawn_blocking(move || {
                        startup::refresh_gh_token(&user);
                    });
                }
            }

            // Handle SIGTERM
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
                let _ = shutdown_tx.send(());
                break;
            }

            // Handle SIGINT (Ctrl+C)
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down...");
                let _ = shutdown_tx.send(());
                break;
            }

            // Handle RPC-triggered shutdown (exec-restart or explicit shutdown)
            _ = shutdown_rx.recv() => {
                info!("Shutdown triggered via RPC");
                break;
            }
        }
    }

    // Persist session info for survival across daemon restarts
    info!("Persisting session info for restart survival...");
    if let Err(e) = persist_sessions_for_restart(&state).await {
        warn!(
            "Failed to persist sessions for restart (sessions will not survive): {}",
            e
        );
    }

    // Shut down plugin daemon
    info!("Shutting down plugin daemon...");
    state.plugin_daemon.shutdown().await;

    // Mark all sessions to be detached (not killed) on drop
    // CRITICAL: Always detach even if persistence failed above - sessions should
    // survive the restart even if we can't restore their context
    state.session_manager.detach_all().await;

    // Shut down all coworker sessions (detach instead of kill)
    info!("Shutting down all coworker sessions (detach mode)...");
    let shutdown_count = state.session_manager.shutdown_all().await;
    if shutdown_count > 0 {
        info!(
            "Detached {} coworker session(s) for restart survival",
            shutdown_count
        );
    }

    // Signal webhook forwarder watchdog to stop
    info!("Stopping webhook forwarder watchdog...");
    let _ = forwarder_shutdown_tx.send(true);

    // PR polling is now in main loop, no separate task to stop

    // Signal chat monitor task to stop
    info!("Stopping chat monitor task...");
    let _ = chat_monitor_shutdown_tx.send(true);

    // Clean up socket file
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }

    // Release PID lock and clean up PID file
    // The lock is released when pid_file is dropped, but we explicitly clean up the file
    drop(pid_file);
    match std::fs::remove_file(&config.pid_file_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Failed to remove PID file: {}", e),
    }

    // Check if an exec-restart was requested (vs normal shutdown)
    let restart = state
        .restart_requested
        .load(std::sync::atomic::Ordering::Relaxed);

    if restart {
        info!("Daemon stopped — exec-restart requested");
        Ok(DaemonExitStatus::ExecRestart {
            workdir: config.workdir.clone(),
            project_name: config.project_name.clone(),
        })
    } else {
        info!("Daemon stopped");
        Ok(DaemonExitStatus::Shutdown)
    }
}

// ─── Real-time Session Event Helpers ──────────────────────────────────────
//
// These free functions process events received from the aggregated session
// event channel (session_agg_rx). They are called from the select! branch
// in the main event loop.

/// Process a batch of session events received from the aggregated channel.
///
/// Groups events by session name, updates health flags, logs events,
/// processes effects (backfill, lead/coworker output), and handles stopped sessions.
async fn handle_session_event_batch(
    batch: Vec<session_events::SessionEvent>,
    state: &Arc<DaemonState>,
) {
    use crate::headless::StreamEvent;

    let mut events_by_name: HashMap<String, Vec<StreamEvent>> = HashMap::new();
    let mut stopped_sessions: Vec<(String, String)> = Vec::new(); // (name, slot_id)

    for session_event in batch {
        match session_event {
            session_events::SessionEvent::Event {
                name,
                slot_id,
                event,
            } => {
                debug!(coworker = %name, event = ?event, "session event (realtime)");
                state
                    .session_manager
                    .update_session_health(&slot_id, &event)
                    .await;
                state.session_manager.log_event(&slot_id, &event).await;
                events_by_name.entry(name).or_default().push(event);
            }
            session_events::SessionEvent::Stderr {
                name: _,
                slot_id,
                line,
            } => {
                state
                    .session_manager
                    .handle_stderr_line(&slot_id, &line)
                    .await;
            }
            session_events::SessionEvent::Stopped { name, slot_id } => {
                // Capture exit code before collect_health() runs, so health
                // decision functions see it on the next tick.
                state.session_manager.mark_stopped(&slot_id).await;
                stopped_sessions.push((name, slot_id));
            }
        }
    }

    // Update health snapshot after processing events so decision functions
    // see the latest state.
    let health = state.session_manager.collect_health().await;
    {
        let mut hh = state.headless_health.write().unwrap();
        *hh = health;
    }
    state
        .headless_health_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Process events through the effects pipeline (same logic as old drain branch)
    if !events_by_name.is_empty() {
        process_session_events_batch(&events_by_name, state).await;
    }

    // Handle stopped sessions (collect accumulated stderr for crash diagnostics)
    for (name, slot_id) in stopped_sessions {
        let stderr_lines = state.session_manager.take_recent_stderr(&slot_id).await;
        handle_session_stopped(&name, &stderr_lines, state).await;
    }
}

/// Process a batch of session events through the effects pipeline.
///
/// Handles session_id backfill on init events and routes lead/coworker output
/// through the Effect system.
async fn process_session_events_batch(
    events: &HashMap<String, Vec<crate::headless::StreamEvent>>,
    state: &Arc<DaemonState>,
) {
    // 1. Backfill session_id on init events for freshly spawned sessions
    let mut needs_persist_save = false;
    for (name, session_events) in events {
        for event in session_events {
            // Backfill session_id on init event for freshly spawned sessions
            if let crate::headless::StreamEvent::System {
                subtype,
                session_id: Some(sid),
                ..
            } = event
                && subtype == "init"
                && !sid.is_empty()
            {
                let mut ps = state.persistent_state.lock().await;
                // Look up the previous session_id from the SessionRecord.
                let previous_sid = ps
                    .session_by_name(name.as_str())
                    .map(|r| r.session_id.clone())
                    .unwrap_or_default();
                // Also backfill channel_lead_sessions for channel lead sessions.
                // Channel leads use the channel name directly as their session name,
                // so we can look up the key directly in channel_lead_sessions.
                if let Some(stored_id) = ps.channel_lead_sessions.get_mut(name.as_str())
                    && (stored_id.is_empty() || stored_id != sid)
                {
                    info!(
                        "Backfilling channel lead session_id for '{}': {}",
                        name, sid
                    );
                    *stored_id = sid.clone();
                    needs_persist_save = true;
                }
                // If this session had a provisional ID, migrate persistent/session maps
                // to the real ID emitted by init.
                let mut migrated_record = None;
                if previous_sid != *sid
                    && !previous_sid.is_empty()
                    && let Some(old_record) = ps.sessions.remove(&previous_sid)
                {
                    let mut updated = old_record;
                    updated.session_id = sid.clone();
                    updated.is_running = true;
                    migrated_record = Some(updated);
                    needs_persist_save = true;
                }
                // Ensure a SessionRecord exists for this session.
                // For spawned sessions, the record already exists from spawn_coworker().
                // For migrated sessions (provisional -> real ID), re-insert under the new key.
                // The else branch is a rare fallback for sessions without a pre-existing record.
                if let Some(record) = migrated_record {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        ps.sessions.entry(sid.clone())
                    {
                        entry.insert(record);
                        needs_persist_save = true;
                    }
                } else if let std::collections::hash_map::Entry::Vacant(entry) =
                    ps.sessions.entry(sid.clone())
                {
                    // Fallback: create a minimal SessionRecord for sessions that
                    // weren't created via spawn_coworker() (e.g., externally started
                    // sessions or very old daemon state).
                    entry.insert(crate::daemon::state::SessionRecord {
                        session_id: sid.clone(),
                        name: name.to_string(),
                        is_running: true,
                        created_at: chrono::Utc::now(),
                        last_active: chrono::Utc::now(),
                        ..Default::default()
                    });
                    needs_persist_save = true;
                }
                // SessionRecord is the single source of truth — no
                // reverse maps to update.
            }
        }
    }
    if needs_persist_save {
        let ps = state.persistent_state.lock().await;
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!(
                "Failed to save persistent state after session_id backfill: {}",
                e
            );
        }
    }

    // 2. Process lead, channel lead, and coworker text output.
    // Routes through Effect pipeline to maintain architecture consistency.
    // All functions need channel_lead_sessions — acquire the lock once.
    let (lead_effects, coworker_effects) = {
        let ps = state.persistent_state.lock().await;
        // Derive fork_bound_channels from SessionRecord (fork sessions
        // that are channel leads with a bound thread).
        let fork_bound_channels: HashMap<String, String> = ps
            .sessions
            .values()
            .filter(|s| s.is_fork_session() && s.channel.is_some())
            .map(|s| (s.name.clone(), s.channel.clone().unwrap()))
            .collect();
        // Collect channels where show_full_lead_output is disabled.
        let suppress_auto_output_channels: HashSet<String> = ps
            .channel_settings
            .iter()
            .filter(|(_, s)| !s.show_full_lead_output)
            .map(|(name, _)| name.clone())
            .collect();
        let lead_effects = stream::process_lead_output(
            events,
            &ps.channel_lead_sessions,
            &state.project_name,
            &fork_bound_channels,
            &suppress_auto_output_channels,
        );

        // Only agents without a native home channel get DM mirrors.
        // Root leads and fork sessions already stream to their real
        // channel / bound thread, so a dm-* copy is duplicate noise.
        let dm_agent_names =
            dm_mirror_agent_names(&ps.sessions, &ps.channel_lead_sessions, &state.project_name);
        let coworker_effects = stream::process_agent_output(events, &dm_agent_names);

        (lead_effects, coworker_effects)
    };
    effects::execute_effects(lead_effects, state).await;
    effects::execute_effects(coworker_effects, state).await;
}

/// Handle a single stopped session: cleanup, deregister, post exit message.
///
/// Extracted from the old drain branch's stopped-session loop. Handles
/// failed resume detection, session_id clearing, and channel posting.
async fn handle_session_stopped(name: &str, stderr_lines: &[String], state: &Arc<DaemonState>) {
    warn!("Headless session '{}' exited (realtime)", name);
    if !stderr_lines.is_empty() {
        warn!(
            "Session '{}' stderr ({} lines):\n{}",
            name,
            stderr_lines.len(),
            stderr_lines.join("\n")
        );
    }

    // Check if this was a failed resume attempt BEFORE removing
    // the session (remove deletes it from the map).
    // SAFETY: This check must happen before any cleanup operations
    // that could remove the session from the map. All daemon event
    // handling is single-threaded, so no concurrent remove() is possible.
    let failed_resume = state.session_manager.was_failed_resume(name).await;

    // Capture session_id BEFORE cleanup (cleanup removes session record).
    let session_id_for_cleanup = {
        let ps = state.persistent_state.lock().await;
        ps.session_by_name(name).map(|r| r.session_id.clone())
    };

    // Remove from session manager tracking (session-death-specific:
    // shutdown path uses session_manager.shutdown() instead)
    state.session_manager.remove(name).await;
    // Clean up all transient coworker state (shared with shutdown path).
    // Releases dead coworker worktree binding so immediate
    // respawn can continue.
    state.cleanup_dead_coworker_state(name).await;

    // Only clear session_id when the resume itself failed
    // (session died within 30s of a resume spawn). This means
    // the session data doesn't exist on disk and retrying the
    // same session_id would loop. Sessions that ran longer
    // likely have valid data on disk — keep their session_id
    // so the next spawn can try to resume them.
    if failed_resume {
        let mut ps = state.persistent_state.lock().await;
        // Clear stale session_id from SessionRecord so the next spawn
        // doesn't attempt to resume a session that failed immediately.
        if let Some(ref sid) = session_id_for_cleanup
            && let Some(record) = ps.sessions.get_mut(sid)
        {
            if !record.session_id.is_empty() {
                info!(
                    "Clearing stale session_id for '{}' after failed resume (was: {})",
                    name, record.session_id
                );
                record.session_id.clear();
            }
            // Clear task binding to prevent dispatch crash-loop.
            // Without this, session_task_map rebuilds the stale task->session link on
            // the next tick and dispatch re-attempts resume indefinitely.
            if let Some(task_id) = record.task_id.take() {
                info!(
                    "Clearing task !{} from failed-resume session {}",
                    task_id, sid
                );
            }
            // Channel leads are long-lived — keep resume_on_startup=true
            // so they're always eligible for resume and never GC'd.
            if record.agent_type != "midtown-channel-lead" {
                record.resume_on_startup = false;
            }
        }
        // For channel lead sessions: clear the stale session ID but
        // preserve the key in channel_lead_sessions so
        // ensure_channel_leads_alive knows this channel still needs a
        // lead and will emit RespawnChannelLead on the next tick.
        if let Some(stored_id) = ps.channel_lead_sessions.get(name) {
            if !stored_id.is_empty() {
                info!(
                    "Clearing stale channel_lead_sessions ID for '{}' after failed resume (was: {})",
                    name, stored_id
                );
            }
            ps.channel_lead_sessions
                .insert(name.to_string(), String::new());
        }
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!(
                "Failed to save persistent state after clearing session_id: {}",
                e
            );
        }
    }

    // Dead forks stay dead — no auto-respawn. The SessionRecord
    // is already marked is_running=false by cleanup_coworker_state.
    // Thread replies to dead forks will fall through to the channel lead.
    {
        // Determine session role for the exit message
        let is_lead = helpers::is_project_lead(name, &state.project_name);
        let session_role = if is_lead {
            "Lead"
        } else if state
            .persistent_state
            .lock()
            .await
            .channel_lead_sessions
            .contains_key(name)
        {
            "Channel lead"
        } else {
            "Coworker"
        };

        // Format message with accumulated stderr from realtime path
        let stderr_ref = if stderr_lines.is_empty() {
            None
        } else {
            Some(stderr_lines)
        };
        let message_text = helpers::format_unexpected_exit_message(session_role, name, stderr_ref);

        // All exit messages go to #ops (operational noise).
        let msg = crate::message::Message::for_channel(
            constants::OPS_CHANNEL,
            "midtown",
            message_text,
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast_async(&msg).await {
            warn!("Failed to post session exit message for {}: {}", name, e);
        }
    }
}

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// Per-coworker decision helpers for unit tests. The batch `decide_*` functions
// in `rules.rs` handle the full coworker set; these single-coworker variants
// make individual test cases easier to write.

/// Shared mutex for tests that modify the `PATH` environment variable.
///
/// All test modules (pr_tests, effects_tests, etc.) must use this single lock
/// so that PATH-mocking tests in different files serialize against each other.
/// Two separate per-file statics would allow tests from different files to run
/// concurrently and corrupt each other's `gh` CLI mock.
#[cfg(test)]
pub(crate) static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[path = "mod_tests.rs"]
#[cfg(test)]
mod tests;
