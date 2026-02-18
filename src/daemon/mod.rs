//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod architect;
mod auto_archive;
mod chat;
mod clusterer;
mod clustering;
mod constants;
mod dispatch;
pub(crate) mod effects;
pub(crate) mod events;
mod health;
pub mod helpers;
mod pr;
mod rpc;
mod rpc_auth;
mod rpc_channel;
mod rpc_coworker;
mod rpc_headed;
mod rpc_headless;
mod rpc_insight;
mod rpc_kanban;
mod rpc_reminder;
mod rpc_session;
mod rpc_status;
mod rpc_task;
pub(crate) mod sessions;
pub mod snapshot;
mod specialized;
mod startup;
pub(crate) mod state;
mod stream;
mod trackers;
mod webhook_fwd;

use constants::*;
pub use constants::{
    DEFAULT_MAX_COWORKERS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, PR_NUDGE_COOLDOWN_SECS,
    PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, PR_REVIEW_DELAY_SECS,
};
pub use state::{DaemonPersistentState, HeadlessSessionInfo};
pub use trackers::{
    CommentTracker, OrphanTracker, PrIssueTracker, PrIssueType, StuckConditionTracker,
    StuckConditionType,
};

// Test helper for orphan recovery tests
#[doc(hidden)]
pub use dispatch::should_recover_task_test_helper;

// Test helpers for clustering integration tests
#[doc(hidden)]
pub use clustering::apply_clustering_diff;
#[doc(hidden)]
pub use effects::Effect;

// Test helpers for E2E tests with captured snapshots
// Note: Only exporting pure functions that take &WorldSnapshot and return Vec<Effect>.
// Functions that take &DaemonState are not exported because:
// 1. DaemonState is pub(crate) which causes privacy warnings
// 2. Those functions often mutate state, making them harder to test in isolation
// 3. Pure functions are the gold standard for testing
#[doc(hidden)]
pub use dispatch::{
    build_subject_based_completion_effects, check_for_duplicate_task_workers, reset_orphaned_tasks,
};
#[doc(hidden)]
pub use events::DaemonEvent;
#[doc(hidden)]
pub use health::{
    check_and_restart_stuck_reviewers, check_and_restart_tool_name_conflicts,
    check_and_shutdown_idle_coworkers, check_for_usage_limits, detect_stale_attached_sessions,
    ensure_lead_alive, maybe_nudge_usage_limit_expiry,
};
#[doc(hidden)]
pub use pr::{collect_merged_pr_cleanup_effects, reconcile_orphaned_prs};

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom, Write};
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
use crate::rpc::RequestId;
use crate::web::{self, WebUpdate};
use crate::webhook::{WebhookConfig, start_webhook_server};
use crate::worktree::WorktreeManager;

/// An in-memory task assignment record with timing metadata.
///
/// Tracks in-memory task assignment for busy coworker tracking.
#[derive(Debug, Clone)]
pub(crate) struct TaskAssignment {
    pub task_id: String,
}

/// Max messages buffered per headed session before dropping oldest entries.
const HEADED_SESSION_QUEUE_MAX: usize = 200;
/// Lease timeout for headed adapters (seconds without heartbeat/poll).
const HEADED_LEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HeadedQueuedMessage {
    pub id: u64,
    pub kind: String,
    pub text: String,
    pub submit: bool,
}

#[derive(Debug, Clone)]
struct HeadedLease {
    adapter_id: String,
    provider: crate::auth::AuthProvider,
    last_seen: tokio::time::Instant,
}

#[derive(Debug, Default)]
struct HeadedSessionState {
    next_id: u64,
    acked_id: u64,
    lease: Option<HeadedLease>,
    messages: VecDeque<HeadedQueuedMessage>,
    /// Pending capture request: daemon sets this, wrapper fulfils it on next poll.
    capture_tx: Option<tokio::sync::oneshot::Sender<String>>,
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
    /// Maximum number of concurrent coworkers. Default: 16.
    pub max_coworkers: usize,
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

        // Max concurrent coworkers: env var > project config > global config > default (16)
        let max_coworkers = std::env::var("MIDTOWN_MAX_COWORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                if project_name.is_empty() {
                    crate::config::GlobalConfig::load().default.max_coworkers()
                } else {
                    crate::config::get_project_config(&project_name).max_coworkers()
                }
            })
            .unwrap_or(DEFAULT_MAX_COWORKERS);

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
            max_coworkers,
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

/// Unified cache for PR-to-coworker mappings.
///
/// Replaces the previous separate fields (`cached_open_pr_branches`,
/// `cached_merged_pr_coworkers`) with a single struct. Merged refresh timing
/// uses the shared `CooldownTracker` rather than a standalone timestamp.
#[derive(Default)]
struct PrCoworkerCache {
    /// Coworker names extracted from open PR branch names.
    /// Updated every PR poll tick (~30s).
    open_pr_owners: HashSet<String>,
    /// Coworker names from recently merged PR branch names.
    /// Updated every `MERGED_PRS_FETCH_INTERVAL_SECS` (5 minutes via CooldownTracker).
    merged_pr_owners: HashSet<String>,
    /// Full branch names from recently merged PRs (e.g., "york/feature-x").
    /// Used for precise orphan filtering - avoids hiding genuinely orphaned
    /// worktrees when the same coworker has other merged PRs.
    merged_pr_branches: HashSet<String>,
    /// PR numbers of recently merged PRs. Used by task dispatch to skip
    /// tasks that reference a PR that's already merged (e.g., "Address
    /// review feedback on PR #709" when PR #709 is merged).
    merged_pr_numbers: HashSet<u64>,
    /// Coworker names whose open PR has all CI checks passing.
    /// Used by snapshot to determine PR break eligibility.
    ci_passed_pr_owners: HashSet<String>,
    /// Coworker names whose open PR has CI passed AND has review feedback to address.
    /// Used by snapshot for idle shutdown protection (prevents spawn→idle→break loop).
    review_feedback_pr_owners: HashSet<String>,
    /// Count of open PRs that need review (not draft, no Claude review, no formal review).
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
    /// Repository name (primary repo)
    repo_name: String,
    /// Repository owner (extracted from git remote URL at startup).
    /// Used by pure decision functions to determine if a PR is authored by the lead.
    repo_owner: Option<String>,
    /// Default branch name (detected at startup, e.g. "main" or "master")
    default_branch: String,
    /// Paths to all repos in the project (primary + additional)
    all_repo_paths: Vec<PathBuf>,
    /// Unified cooldown tracker for orphan spawning and task nudge rate limiting.
    cooldowns: std::sync::Mutex<crate::rules::CooldownTracker>,
    /// Tracks orphaned worktrees — detection time, warning cooldown, and auto-pruning
    orphan_tracker: std::sync::RwLock<OrphanTracker>,
    /// Unified persistent state (GitHub + reminders), saved to daemon-state.json.
    persistent_state: Mutex<state::DaemonPersistentState>,
    /// Broadcast sender for pushing channel messages to WebSocket clients
    web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
    /// Maximum number of concurrent coworkers
    max_coworkers: usize,
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
    /// Unified cache for PR-to-coworker mappings (open + merged + CI status).
    pr_coworker_cache: std::sync::RwLock<PrCoworkerCache>,
    /// Saved session IDs for coworkers on PR break, keyed by coworker name.
    /// When a coworker is shut down for PR break (CI passing, idle), we save their
    /// session ID here so they can be resumed with `--resume <id>` when PR activity
    /// (review comments, CI failure, etc.) requires them back.
    pr_break_sessions: std::sync::RwLock<HashMap<String, String>>,
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
    /// Task IDs with pending `AssignAndSpawn` effects that haven't completed yet.
    ///
    /// Prevents the task-level spawn race condition where two ticks both see the same
    /// pending task and generate duplicate `AssignAndSpawn` effects. The race occurs
    /// because:
    /// 1. Tick 1 evaluates, sees pending task, generates `AssignAndSpawn`
    /// 2. Effects start executing (disk write + spawn takes time)
    /// 3. Tick 2 fires, collects snapshot that still shows task as pending
    /// 4. Tick 2 generates another `AssignAndSpawn` for the same task
    ///
    /// Fix: After `evaluate_tick`, scan returned effects for `AssignAndSpawn` and
    /// add those task IDs here. In `spawn_for_pending_tasks`, skip tasks that are
    /// already in-flight. Clear entries when effects complete (success or failure).
    in_flight_task_spawns: std::sync::Mutex<HashSet<String>>,
    /// Internal tracking of coworker task assignments (coworker name → assignment).
    ///
    /// With isolated task lists, the daemon can't see coworker tasks on disk.
    /// This map tracks which coworker is working on which task, enabling busy
    /// detection for dispatch and idle protection.
    ///
    /// Updated when: AssignAndSpawn succeeds, task.claim RPC is received.
    /// Cleared when: coworker shuts down, task is completed or reset to pending.
    coworker_task_assignments: std::sync::Mutex<HashMap<String, TaskAssignment>>,
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
    /// Process health state for headless coworkers, keyed by coworker name.
    ///
    /// Populated by the session management layer from `HeadlessSession` stream events
    /// and process status. Read by `collect_world_snapshot()` for the health decision
    /// functions in `rules.rs`.
    pub(crate) headless_health: std::sync::RwLock<HashMap<String, snapshot::ProcessHealth>>,
    /// Coworkers currently in "attached" state (interactive session).
    ///
    /// When the Lead attaches to a headless coworker via `midtown session attach`,
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
    /// Kanban data cache with 30s TTL.
    ///
    /// Stores the full kanban GraphQL response (PRs, merged PRs, repos) keyed by
    /// a hash of the repo paths. Integrated into DaemonState (rather than a global
    /// static) so the daemon can inspect and clean it up alongside other caches.
    kanban_cache: rpc_kanban::KanbanCache,
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
    #[allow(dead_code)] // Used in rpc.rs via state.shutdown_tx.send()
    shutdown_tx: broadcast::Sender<()>,
    /// Session-scoped intercom queues for headed adapters (wrapper transport).
    ///
    /// Each session (e.g., "lead", "park") has an ordered queue and an
    /// exclusive adapter lease. Adapters consume via poll+ack; the daemon
    /// enqueues logical control messages (nudges/keys) without terminal coupling.
    headed_sessions: Mutex<HashMap<String, HeadedSessionState>>,
    /// Recent tool call/result items per agent, keyed by lowercase agent name.
    ///
    /// Updated by `BroadcastUniversalItems` effects as stream events arrive.
    /// Capped at `MAX_TOOL_ITEMS_PER_AGENT` per agent to bound memory.
    /// Cleared when a channel message from the agent is posted (work phase done),
    /// and when a coworker session stops (via `cleanup_coworker_state`).
    /// Exposed via `kanban.data` RPC (live, not cached) so the TUI can display
    /// per-coworker tool activity alongside chat messages.
    pub(crate) recent_tool_items:
        std::sync::RwLock<HashMap<String, Vec<crate::universal_events::UniversalItem>>>,
    /// Negative cache for `is_pr_reviewed`: PR numbers confirmed NOT to have a review yet.
    ///
    /// `is_pr_reviewed` caches positive results (reviewed) in persistent state forever.
    /// Without this cache, every unreviewed PR triggers a `gh pr view` GraphQL call on
    /// every PR poll tick (~every 45s). With this cache, we suppress repeat calls for
    /// PRs confirmed unreviewed within the last `PR_REVIEW_NEGATIVE_CACHE_SECS` seconds.
    ///
    /// Short TTL (2 min) ensures we eventually detect the review after it's posted.
    pr_review_negative_cache: std::sync::Mutex<HashMap<u64, std::time::Instant>>,
    /// LRU pool for coworker name allocation.
    ///
    /// Tracks available and allocated names. Names at the front of the queue are
    /// least-recently-used and will be allocated first. Released names go to the
    /// back. Restored from `persistent_state.sessions` on startup.
    pub(crate) name_pool: std::sync::Mutex<crate::name_pool::NamePool>,
    /// Reverse map: coworker name → session ID.
    ///
    /// Maintained in memory alongside `persistent_state.sessions`. Updated when
    /// a session init event arrives with a session ID, and cleared when a session
    /// stops. Enables O(1) lookup of the session ID for a given name.
    pub(crate) name_to_session: std::sync::Mutex<HashMap<String, String>>,
    /// Reverse map: session ID → coworker name.
    ///
    /// Inverse of `name_to_session`. Updated and cleared together with that map.
    pub(crate) session_to_name: std::sync::Mutex<HashMap<String, String>>,
    /// Reverse map: task ID → session ID.
    ///
    /// Enables O(1) lookup of the session working on a given task. Updated when
    /// a session is initialised with a task and cleared when the session stops.
    pub(crate) task_to_session: std::sync::Mutex<HashMap<String, String>>,
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
        let full_name = std::process::Command::new("gh")
            .current_dir(repo_path)
            .args([
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "--jq",
                ".nameWithOwner",
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
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

    /// Clean up all transient state for a coworker after its session stops.
    ///
    /// Called from both intentional shutdown (`shutdown_coworker_impl` in effects.rs)
    /// and unexpected session death (session monitor in the event loop). Without this
    /// shared function, the two paths can drift out of sync — e.g., session death
    /// missing cooldown/nudge/assignment cleanup that shutdown handles. See PR #1268.
    ///
    /// Handles: coworker deregistration, stop-time recording, coworker records,
    /// cooldowns, pending nudges, task assignments, recent tool activity,
    /// NamePool release, and session reverse-map cleanup (name_to_session,
    /// session_to_name, task_to_session).
    ///
    /// Does NOT handle session-specific operations (session_manager.shutdown vs
    /// session_manager.remove) or worktree unbinding — those differ between the
    /// intentional shutdown and session death paths.
    pub(crate) async fn cleanup_coworker_state(&self, name: &str) {
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
        // Clear task assignment tracking (coworker is no longer active)
        self.clear_coworker_assignments(name);
        // Clear recent tool activity (prevents stale activity on respawn)
        {
            let mut tool_map = self.recent_tool_items.write().unwrap();
            tool_map.remove(name);
        }
        // Release name back to NamePool and clean up session reverse maps.
        // Each lock is acquired and released independently (no nesting)
        // to avoid implicit lock-ordering dependencies.
        {
            let mut name_pool = self.name_pool.lock().unwrap();
            name_pool.release(name);
        }
        let removed_session_id = self.name_to_session.lock().unwrap().remove(name);
        if let Some(session_id) = removed_session_id {
            self.session_to_name.lock().unwrap().remove(&session_id);
            // Clean up task_to_session entries pointing to this session.
            self.task_to_session
                .lock()
                .unwrap()
                .retain(|_, sid| sid != &session_id);
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
        // Also clean up expired kanban cache entries
        self.kanban_cache.cleanup();
    }

    /// Check if the daemon is at the absolute coworker limit (including reviewer headroom).
    ///
    /// Reviewers may exceed `max_coworkers` by up to `REVIEW_HEADROOM` slots,
    /// so the absolute cap is `max_coworkers + REVIEW_HEADROOM`. This allows
    /// reviewer spawning to proceed even when the dev cap is fully used.
    ///
    /// The lead and channel leads are excluded: they register in CoworkerManager
    /// but are not dev/reviewer slots and must not consume capacity.
    fn is_at_coworker_limit(&self, channel_lead_names: &std::collections::HashSet<String>) -> bool {
        let non_lead_count = self
            .coworkers
            .list_running()
            .iter()
            .filter(|cw| {
                !cw.name.eq_ignore_ascii_case("lead") && !channel_lead_names.contains(&cw.name)
            })
            .count();
        non_lead_count >= self.max_coworkers + REVIEW_HEADROOM
    }

    /// Check if the daemon is at the dev coworker limit.
    ///
    /// Dev cap equals `max_coworkers` — REVIEW_HEADROOM is NOT subtracted here.
    /// Instead, `is_at_coworker_limit()` uses `max_coworkers + REVIEW_HEADROOM`
    /// so reviewers can exceed the normal dev cap by up to REVIEW_HEADROOM slots.
    ///
    /// The lead and channel leads are excluded: they register in CoworkerManager
    /// but are not dev/reviewer slots and must not consume capacity.
    fn is_at_dev_limit(&self, channel_lead_names: &std::collections::HashSet<String>) -> bool {
        let non_lead_count = self
            .coworkers
            .list_running()
            .iter()
            .filter(|cw| {
                !cw.name.eq_ignore_ascii_case("lead") && !channel_lead_names.contains(&cw.name)
            })
            .count();
        non_lead_count >= self.max_coworkers
    }

    /// Check if a coworker slot is available for spawning.
    ///
    /// This combines two checks:
    /// 1. We're not at the max coworker limit (absolute cap)
    /// 2. There's an available name in the name pool
    ///
    /// Use this for diagnostic messages and decisions about whether spawning
    /// is possible. For actual spawning, use the individual checks to get
    /// better error messages.
    fn has_available_coworker_slot(
        &self,
        channel_lead_names: &std::collections::HashSet<String>,
    ) -> bool {
        !self.is_at_coworker_limit(channel_lead_names)
            && self
                .coworkers
                .next_available_name_excluding(channel_lead_names)
                .is_some()
    }

    /// Check if a PR has a review comment from a Claude coworker.
    ///
    /// Uses the persistent state cache as the single source of truth. First
    /// checks the cache; if not found, makes GitHub API calls and caches
    /// positive results permanently (review status is monotonic).
    async fn is_pr_reviewed(&self, pr_number: u64) -> bool {
        // Fast path: check persistent cache (single source of truth)
        {
            let ps = self.persistent_state.lock().await;
            if ps.github.has_cached_review(pr_number) {
                debug!(
                    "PR #{} has cached Claude review (skipping API call)",
                    pr_number
                );
                return true;
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

        // Slow path: check via API calls
        let has_review = pr::pr_has_claude_review_uncached(pr_number);

        if has_review {
            // Cache positive results permanently (reviews are monotonic — they don't disappear)
            let mut ps = self.persistent_state.lock().await;
            ps.github.mark_reviewed_pr(pr_number);
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
        repo_name: String,
        all_repo_paths: Vec<PathBuf>,
        channel_router: crate::ChannelRouter,
        web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
        max_coworkers: usize,
        push_manager: Option<std::sync::Arc<crate::push::PushManager>>,
        default_branch: String,
        shutdown_tx: broadcast::Sender<()>,
    ) -> crate::Result<Self> {
        // Load unified persistent state (migrates from legacy files if needed)
        let mut persistent_state = state::DaemonPersistentState::load_for_repo(&repo_name)
            .unwrap_or_else(|e| {
                warn!("Failed to load daemon-state.json: {}, using defaults", e);
                state::DaemonPersistentState::default()
            });
        let backfilled = backfill_headless_sessions_from_logs(
            &repo_name,
            &mut persistent_state.headless_sessions,
        );
        if backfilled > 0 {
            info!(
                "Backfilled {} historical headless session(s) from headless-*.jsonl logs",
                backfilled
            );
            if let Err(e) = persistent_state.save_for_repo(&repo_name) {
                warn!("Failed saving backfilled historical sessions: {}", e);
            }
        }

        let user_display_name = config::get_user_display_name_for_project(&repo_name);

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

        // Clone repo_name for session_manager before moving it into Self
        let session_manager_repo_name = repo_name.clone();

        // Build NamePool from all known coworker names and restore state from persisted sessions.
        let all_names: Vec<&str> = crate::coworker::AVENUE_NAMES
            .iter()
            .chain(crate::coworker::OVERFLOW_NAMES.iter())
            .copied()
            .collect();
        let mut name_pool = crate::name_pool::NamePool::new(&all_names);
        let mut name_to_session: HashMap<String, String> = HashMap::new();
        let mut session_to_name: HashMap<String, String> = HashMap::new();
        let mut task_to_session: HashMap<String, String> = HashMap::new();
        {
            let allocated_names: Vec<String> = persistent_state
                .sessions
                .values()
                .filter_map(|r| r.current_name.clone())
                .collect();
            name_pool.restore(&allocated_names);
            for (session_id, record) in &persistent_state.sessions {
                if let Some(ref name) = record.current_name {
                    name_to_session.insert(name.clone(), session_id.clone());
                    session_to_name.insert(session_id.clone(), name.clone());
                }
                if let Some(ref task_id) = record.task_id {
                    task_to_session.insert(task_id.clone(), session_id.clone());
                }
            }
        }

        Ok(Self {
            coworkers,
            channel_router,
            socket_path,
            coworker_records: tokio::sync::RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::new()),
            repo_name,
            repo_owner,
            default_branch,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            orphan_tracker: std::sync::RwLock::new(OrphanTracker::new()),
            persistent_state: Mutex::new(persistent_state),
            web_updates_tx,
            max_coworkers,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            last_pr_poll_hash: Mutex::new(0),
            pr_coworker_cache: std::sync::RwLock::new(PrCoworkerCache::default()),
            pr_break_sessions: std::sync::RwLock::new(HashMap::new()),
            coworker_stop_times: std::sync::RwLock::new(HashMap::new()),
            stuck_tracker: Mutex::new(StuckConditionTracker::new()),
            ci_notification_buffer: Mutex::new(trackers::CiNotificationBuffer::new()),
            repo_name_cache: std::sync::RwLock::new(HashMap::new()),
            user_display_name,
            last_webhook_event_at: Mutex::new(None),
            in_flight_task_spawns: std::sync::Mutex::new(HashSet::new()),
            coworker_task_assignments: std::sync::Mutex::new(HashMap::new()),
            pending_nudges: std::sync::Mutex::new(HashMap::new()),
            comment_tracker: Mutex::new(trackers::CommentTracker::new()),
            insight_hashes: std::sync::Mutex::new(HashSet::new()),
            reviewer_escalations_posted: std::sync::Mutex::new(HashSet::new()),
            review_note_tracker: std::sync::Mutex::new(HashMap::new()),
            headless_health: std::sync::RwLock::new(HashMap::new()),
            attached_coworkers: std::sync::Mutex::new(HashMap::new()),
            session_manager: sessions::SessionManager::new(session_manager_repo_name),
            rpc_response_cache: Mutex::new(HashMap::new()),
            kanban_cache: rpc_kanban::KanbanCache::new(),
            pr_review_negative_cache: std::sync::Mutex::new(HashMap::new()),
            draining: std::sync::atomic::AtomicBool::new(false),
            restart_requested: std::sync::atomic::AtomicBool::new(false),
            shutdown_tx,
            headed_sessions: Mutex::new(HashMap::new()),
            recent_tool_items: std::sync::RwLock::new(HashMap::new()),
            name_pool: std::sync::Mutex::new(name_pool),
            name_to_session: std::sync::Mutex::new(name_to_session),
            session_to_name: std::sync::Mutex::new(session_to_name),
            task_to_session: std::sync::Mutex::new(task_to_session),
        })
    }

    /// Spawn a coworker as a headless session and initialize its record.
    ///
    /// Uses `CoworkerManager::prepare_spawn` for worktree lifecycle, then
    /// `SessionManager::spawn` for the headless process, and finally
    /// `CoworkerManager::register` to add the coworker to the tracking map.
    async fn spawn_coworker(&self, config: &crate::launch::LaunchConfig) -> crate::Result<()> {
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
            return Ok(());
        }

        // Inject project-resolved auth profile if not already set
        let config = if config.auth_profile_dir.is_none() {
            let mut c = config.clone();
            c.auth_profile_dir = Some(crate::auth::active_profile_dir_for_project_with_provider(
                &self.repo_name,
                c.auth_provider,
            ));
            c
        } else {
            config.clone()
        };

        // Prepare worktree and augment config with additional dirs
        // Note: Worktree creation now happens via Effect::EnsureWorktree in the
        // decision layer (rules.rs), not inline here. This follows the effect-based
        // architecture: I/O goes through the Effect pipeline.
        let (working_dir, launch_config) = self.coworkers.prepare_spawn(&config)?;

        // Build headless config from the unified launch config
        let mut headless_config = launch_config.to_headless_config(&self.repo_name);
        headless_config.cwd = Some(working_dir.clone());

        // Write role-appropriate settings file and set the path
        let settings_file = if config.role == crate::launch::CoworkerRole::Lead {
            crate::settings::write_lead_settings_file()?
        } else {
            crate::settings::write_coworker_settings_file()?
        };
        headless_config.settings_path = Some(settings_file.to_string_lossy().to_string());

        // Set up agent-teams infrastructure (mailbox) before spawning
        if let Some(ref team_name) = config.team_name {
            let member = crate::mailbox::TeamMember {
                name: name.clone(),
                agent_id: crate::mailbox::agent_id(&name, team_name),
                agent_type: match config.role {
                    crate::launch::CoworkerRole::Reviewer => "reviewer".to_string(),
                    crate::launch::CoworkerRole::Lead => "lead".to_string(),
                    crate::launch::CoworkerRole::Coworker => "coworker".to_string(),
                    crate::launch::CoworkerRole::ChannelLead { .. } => "channel-lead".to_string(),
                },
            };
            if let Err(e) = crate::mailbox::upsert_team_member(team_name, member) {
                tracing::warn!("Failed to set up team config for {}: {}", name, e);
            }
        }

        // Spawn the headless session (keyed by slot_id)
        // For resumed sessions, the session_id should be extracted from config.session_mode
        // and passed to spawn(). For fresh sessions, pass None.
        let session_id = match &config.session_mode {
            crate::launch::SessionMode::ResumeSession(sid) => Some(sid.clone()),
            _ => None,
        };
        let persisted_session_id = session_id.clone().unwrap_or_default();
        let initial_prompt = launch_config.initial_prompt.as_deref();
        self.session_manager
            .spawn(
                &name,
                &slot_id,
                &headless_config,
                initial_prompt,
                session_id,
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
        // session_id is None initially — it arrives later via the init StreamEvent
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

        // Persist session info immediately so `session.list` and `session.attach`
        // can find the entry without waiting for the shutdown-time save.
        // For resumed sessions, session_id is known from config; for fresh sessions
        // it starts empty and gets backfilled when the init StreamEvent arrives.
        {
            let mut ps = self.persistent_state.lock().await;
            ps.headless_sessions.insert(
                name.clone(),
                crate::daemon::state::HeadlessSessionInfo {
                    session_id: persisted_session_id,
                    last_active: chrono::Utc::now(),
                    purpose: config
                        .initial_prompt
                        .as_deref()
                        .map(|p| p.chars().take(120).collect::<String>())
                        .unwrap_or_default(),
                    pid: self.session_manager.get_pid(&name).await,
                    coworker_type: match &config.role {
                        crate::launch::CoworkerRole::Reviewer => Some("reviewer".to_string()),
                        crate::launch::CoworkerRole::ChannelLead { .. } => {
                            Some("channel-lead".to_string())
                        }
                        _ => Some("dev".to_string()),
                    },
                    task_id: None,
                    pr_number: config.pr_number,
                    channel: config.channel.clone(),
                    working_dir: Some(working_dir_for_persist),
                    provider: Some(config.auth_provider),
                    profile: Some(profile),
                    resume_on_startup: true,
                    // Use persisted_initial_prompt when set (e.g., session clear sends a
                    // decorated "fresh restart" message but stores the original prompt).
                    // Falls back to initial_prompt when not overridden.
                    initial_prompt: config
                        .persisted_initial_prompt
                        .clone()
                        .or_else(|| config.initial_prompt.clone()),
                },
            );
            if let Err(e) = ps.save_for_repo(&self.repo_name) {
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
        Ok(())
    }

    /// Check if a sender name represents the user (either "user" or the configured display name).
    fn is_user_sender(&self, from: &str) -> bool {
        from.eq_ignore_ascii_case("user")
            || self
                .user_display_name
                .as_ref()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(from))
    }

    /// Check if a task has a pending `AssignAndSpawn` effect that hasn't completed yet.
    ///
    /// Used by `spawn_for_pending_tasks` to avoid generating duplicate effects.
    pub(crate) fn is_task_spawn_in_flight(&self, task_id: &str) -> bool {
        self.in_flight_task_spawns.lock().unwrap().contains(task_id)
    }

    /// Mark a task as having a pending `AssignAndSpawn` effect.
    ///
    /// Called after `evaluate_tick` returns effects, before `execute_effects`.
    pub(crate) fn mark_task_spawn_in_flight(&self, task_id: &str) {
        self.in_flight_task_spawns
            .lock()
            .unwrap()
            .insert(task_id.to_string());
    }

    /// Clear the in-flight marker for a task after its spawn or nudge effect completes.
    ///
    /// Called from `execute_effects` when `AssignAndSpawn` or
    /// `NudgeCoworkerWithCallbacks` (with `RecordTaskAssignment`) succeeds or fails.
    pub(crate) fn clear_task_spawn_in_flight(&self, task_id: &str) {
        self.in_flight_task_spawns.lock().unwrap().remove(task_id);
    }

    /// Record that a coworker has been assigned a task.
    ///
    /// Called when `AssignAndSpawn` succeeds or `task.claim` RPC is received.
    pub(crate) fn record_task_assignment(&self, coworker: &str, task_id: &str) {
        let mut assignments = self.coworker_task_assignments.lock().unwrap();
        assignments.insert(
            coworker.to_lowercase(),
            TaskAssignment {
                task_id: task_id.to_string(),
            },
        );
    }

    /// Clear the task assignment for a specific task (by task ID).
    ///
    /// Called when a task is completed or reset to pending.
    pub(crate) fn clear_task_assignment_by_task(&self, task_id: &str) {
        let mut assignments = self.coworker_task_assignments.lock().unwrap();
        assignments.retain(|_, a| a.task_id != task_id);
    }

    /// Clear all task assignments for a coworker.
    ///
    /// Called when a coworker is shut down.
    pub(crate) fn clear_coworker_assignments(&self, coworker: &str) {
        let mut assignments = self.coworker_task_assignments.lock().unwrap();
        assignments.remove(&coworker.to_lowercase());
    }

    /// Restore task assignments from disk after daemon restart.
    ///
    /// Rebuilds the in-memory `coworker_task_assignments` map by reading
    /// in_progress tasks with owners from Claude Code's task storage.
    /// This ensures task assignments survive daemon restarts.
    ///
    /// Called during daemon startup, after DaemonState is constructed but
    /// before the event loop starts.
    pub(crate) fn restore_task_assignments_from_disk(&self) {
        let in_progress_tasks = crate::tasks::get_in_progress_tasks_with_subjects();

        let mut assignments = self.coworker_task_assignments.lock().unwrap();
        let mut restored_count = 0;

        for (task_id, _subject, owner) in in_progress_tasks {
            if !owner.is_empty() {
                assignments.insert(
                    owner.to_lowercase(),
                    TaskAssignment {
                        task_id: task_id.clone(),
                    },
                );
                restored_count += 1;
            }
        }

        if restored_count > 0 {
            info!(
                "Restored {} task assignment(s) from disk during daemon startup",
                restored_count
            );
        }
    }

    /// Get the set of coworker names that have active task assignments.
    pub(crate) fn get_busy_coworker_names(&self) -> HashSet<String> {
        let assignments = self.coworker_task_assignments.lock().unwrap();
        assignments.keys().cloned().collect()
    }

    /// Get busy coworkers from both disk-based task storage and internal tracking.
    ///
    /// This is the canonical way to check busy status. Callers should use this
    /// instead of `crate::tasks::get_busy_coworkers_for_repo()` directly, since
    /// the disk-based reader cannot see coworker task lists.
    pub(crate) fn get_all_busy_coworkers(&self) -> Vec<String> {
        let mut busy: HashSet<String> = crate::tasks::get_busy_coworkers_for_repo(&self.repo_name)
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
        busy.extend(self.get_busy_coworker_names());
        busy.into_iter().collect()
    }

    /// Check if a coworker is already assigned to a specific task.
    ///
    /// Used to prevent duplicate task assignment in Case 2 grouped task logic.
    /// Returns true if the coworker's current assignment matches the given task_id.
    ///
    /// NOTE: This method is retained for potential debugging use but should NOT be
    /// called from decision functions (evaluate_tick path). Decision logic should use
    /// `snap.coworker_task_assignments` instead to maintain the pure decision pattern.
    #[allow(dead_code)]
    pub(crate) fn is_coworker_assigned_to_task(&self, coworker: &str, task_id: &str) -> bool {
        let assignments = self.coworker_task_assignments.lock().unwrap();
        assignments
            .get(&coworker.to_lowercase())
            .is_some_and(|a| a.task_id == task_id)
    }

    /// Record a pending nudge sent to a coworker.
    ///
    /// Called after successfully sending a nudge via `NudgeCoworker` or
    /// `NudgeCoworkerWithCallbacks`. The pending nudge is used for attribution
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

    fn session_key(session: &str) -> String {
        session.trim().to_ascii_lowercase()
    }

    fn lease_is_active(lease: &HeadedLease) -> bool {
        lease.last_seen.elapsed() <= HEADED_LEASE_TIMEOUT
    }

    fn current_lease<'a>(
        state: &'a mut HeadedSessionState,
        session: &str,
        adapter_id: &str,
    ) -> Result<&'a mut HeadedLease, String> {
        if state.lease.is_none() {
            return Err(format!(
                "No active headed adapter for session '{}'",
                session
            ));
        }
        if state
            .lease
            .as_ref()
            .is_some_and(|l| !Self::lease_is_active(l))
        {
            state.lease = None;
            return Err(format!(
                "Headed adapter lease expired for session '{}'",
                session
            ));
        }
        let Some(lease) = state.lease.as_mut() else {
            return Err(format!(
                "No active headed adapter for session '{}'",
                session
            ));
        };
        if lease.adapter_id != adapter_id {
            return Err(format!(
                "Session '{}' is leased by adapter '{}' (not '{}')",
                session, lease.adapter_id, adapter_id
            ));
        }
        Ok(lease)
    }

    pub(crate) async fn headed_register(
        &self,
        session: &str,
        adapter_id: &str,
        provider: crate::auth::AuthProvider,
    ) -> Result<(u64, crate::auth::AuthProvider), String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions.entry(key.clone()).or_default();

        match session_state.lease.as_mut() {
            None => {
                session_state.lease = Some(HeadedLease {
                    adapter_id: adapter_id.to_string(),
                    provider,
                    last_seen: tokio::time::Instant::now(),
                });
            }
            Some(existing) if existing.adapter_id == adapter_id => {
                existing.provider = provider;
                existing.last_seen = tokio::time::Instant::now();
            }
            Some(existing) if !Self::lease_is_active(existing) => {
                session_state.lease = Some(HeadedLease {
                    adapter_id: adapter_id.to_string(),
                    provider,
                    last_seen: tokio::time::Instant::now(),
                });
            }
            Some(existing) => {
                return Err(format!(
                    "Session '{}' already has active headed adapter '{}'",
                    key, existing.adapter_id
                ));
            }
        }

        let lease_provider = session_state
            .lease
            .as_ref()
            .map(|l| l.provider)
            .unwrap_or(provider);
        Ok((session_state.acked_id, lease_provider))
    }

    pub(crate) async fn headed_unregister(
        &self,
        session: &str,
        adapter_id: &str,
    ) -> Result<(), String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let Some(session_state) = sessions.get_mut(&key) else {
            return Ok(());
        };
        match session_state.lease.as_ref() {
            Some(lease) if lease.adapter_id == adapter_id => {
                session_state.lease = None;
                Ok(())
            }
            Some(lease) => Err(format!(
                "Session '{}' is leased by adapter '{}' (not '{}')",
                key, lease.adapter_id, adapter_id
            )),
            None => Ok(()),
        }
    }

    pub(crate) async fn headed_heartbeat(
        &self,
        session: &str,
        adapter_id: &str,
    ) -> Result<(), String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions.entry(key.clone()).or_default();
        let lease = Self::current_lease(session_state, &key, adapter_id)?;
        lease.last_seen = tokio::time::Instant::now();
        Ok(())
    }

    pub(crate) async fn headed_poll(
        &self,
        session: &str,
        adapter_id: &str,
        after_id: u64,
        limit: usize,
    ) -> Result<(Vec<HeadedQueuedMessage>, bool), String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions.entry(key.clone()).or_default();
        let lease = Self::current_lease(session_state, &key, adapter_id)?;
        lease.last_seen = tokio::time::Instant::now();

        let capture_requested = session_state.capture_tx.is_some();

        let capped_limit = limit.clamp(1, 200);
        let messages = session_state
            .messages
            .iter()
            .filter(|m| m.id > after_id)
            .take(capped_limit)
            .cloned()
            .collect();
        Ok((messages, capture_requested))
    }

    pub(crate) async fn headed_ack(
        &self,
        session: &str,
        adapter_id: &str,
        msg_id: u64,
    ) -> Result<u64, String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions.entry(key.clone()).or_default();
        let lease = Self::current_lease(session_state, &key, adapter_id)?;
        lease.last_seen = tokio::time::Instant::now();

        if msg_id > session_state.acked_id {
            session_state.acked_id = msg_id;
        }
        while session_state
            .messages
            .front()
            .is_some_and(|m| m.id <= session_state.acked_id)
        {
            session_state.messages.pop_front();
        }

        Ok(session_state.acked_id)
    }

    pub(crate) async fn enqueue_headed_nudge(&self, session: &str, text: &str) {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions.entry(key).or_default();
        session_state.next_id = session_state.next_id.saturating_add(1);
        let next_id = session_state.next_id;

        session_state.messages.push_back(HeadedQueuedMessage {
            id: next_id,
            kind: "nudge_text".to_string(),
            text: text.to_string(),
            submit: true,
        });

        while session_state.messages.len() > HEADED_SESSION_QUEUE_MAX {
            if let Some(dropped) = session_state.messages.pop_front()
                && dropped.id > session_state.acked_id
            {
                warn!(
                    "Headed session queue exceeded {} messages - dropped message #{} (kind: {}, text: {})",
                    HEADED_SESSION_QUEUE_MAX,
                    dropped.id,
                    dropped.kind,
                    if dropped.text.len() > 100 {
                        format!("{}...", &dropped.text[..100])
                    } else {
                        dropped.text.clone()
                    }
                );
                session_state.acked_id = dropped.id;
            }
        }
    }

    /// Request a PTY capture from a headed session.
    ///
    /// Installs a oneshot sender. The next `headed.poll` will signal
    /// `capture_output: true` to the wrapper, which calls `headed.output`
    /// to deliver the content. Returns a receiver the caller awaits.
    pub(crate) async fn headed_request_capture(
        &self,
        session: &str,
    ) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        let session_state = sessions
            .get_mut(&key)
            .ok_or_else(|| format!("No headed session for '{}'", session))?;
        if session_state.lease.is_none() {
            return Err(format!("No active wrapper lease for '{}'", session));
        }
        // Replace any stale pending capture
        let (tx, rx) = tokio::sync::oneshot::channel();
        session_state.capture_tx = Some(tx);
        Ok(rx)
    }

    /// Deliver captured PTY output from the wrapper.
    ///
    /// Called by `headed.output` RPC. Sends the content to whoever is
    /// waiting on the oneshot receiver from `headed_request_capture`.
    pub(crate) async fn headed_deliver_output(&self, session: &str, output: String) {
        let key = Self::session_key(session);
        let mut sessions = self.headed_sessions.lock().await;
        if let Some(session_state) = sessions.get_mut(&key)
            && let Some(tx) = session_state.capture_tx.take()
        {
            let _ = tx.send(output);
        }
    }

    /// Nudge the Lead session.
    ///
    /// First tries the headless session_manager path (lead running headless).
    /// Falls back to the headed intercom queue (lead attached interactively).
    pub(crate) async fn nudge_lead(&self, message: &str) {
        if self.session_manager.is_alive("lead").await {
            if let Err(e) = self.session_manager.send_message("lead", message).await {
                tracing::debug!(
                    "Failed to nudge lead via session_manager ({}), falling back to headed intercom",
                    e
                );
                self.enqueue_headed_nudge("lead", message).await;
            }
        } else {
            // Lead is attached interactively — use headed intercom
            self.enqueue_headed_nudge("lead", message).await;
        }
    }
}

impl DaemonState {
    /// Scan effects for task assignment variants and mark their task IDs as in-flight.
    ///
    /// Called after `evaluate_tick` returns effects, before `execute_effects`.
    /// This prevents the next tick from generating duplicate spawns/nudges for the same task.
    /// Covers `AssignAndSpawn` (fresh spawns), `NudgeCoworkerWithCallbacks`, and
    /// `SpawnCoworkerWithCallbacks` that contain a `RecordTaskAssignment` in on_success.
    pub(crate) fn mark_in_flight_spawns_from_effects(&self, effects: &[effects::Effect]) {
        for effect in effects {
            match effect {
                effects::Effect::AssignAndSpawn { task_id, .. } => {
                    self.mark_task_spawn_in_flight(task_id);
                    debug!("Marked task !{} as in-flight spawn", task_id);
                }
                effects::Effect::NudgeCoworkerWithCallbacks { on_success, .. }
                | effects::Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                    for sub_effect in on_success {
                        if let effects::Effect::RecordTaskAssignment { task_id, .. } = sub_effect {
                            self.mark_task_spawn_in_flight(task_id);
                            debug!("Marked task !{} as in-flight assignment", task_id);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Look up the session ID currently holding a given name.
    ///
    /// Used by chat @mention routing for session-targeted nudge delivery,
    /// and by effect handlers for session-centric dispatch.
    pub(crate) fn session_for_name(&self, name: &str) -> Option<String> {
        self.name_to_session.lock().unwrap().get(name).cloned()
    }

    /// Look up the name currently assigned to a given session ID.
    ///
    /// Infrastructure for the session-centric model — used by effect handlers
    /// and RPC adapters once the session-centric migration is further along.
    #[allow(dead_code)] // Scaffold-ahead-of-use for session-centric tasks (Task 9+)
    pub(crate) fn name_for_session(&self, session_id: &str) -> Option<String> {
        self.session_to_name
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
    }

    /// Look up the session ID currently working on a given task ID.
    ///
    /// Infrastructure for the session-centric model — used by effect handlers
    /// and RPC adapters once the session-centric migration is further along.
    #[allow(dead_code)] // Scaffold-ahead-of-use for session-centric tasks (Task 9+)
    pub(crate) fn session_for_task(&self, task_id: &str) -> Option<String> {
        self.task_to_session.lock().unwrap().get(task_id).cloned()
    }

    /// Send a WebUpdate to all connected WebSocket clients (no-op if web is disabled).
    fn broadcast_web_update(&self, update: WebUpdate) {
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
    ///
    /// Also handles insight cross-posting: when a message containing 💡 emoji is sent to a
    /// topic channel (not the main channel), a cross-post is created in the main channel
    /// with source_channel attribution.
    async fn send_and_broadcast_async(&self, message: &Message) -> crate::Result<()> {
        let router = self.channel_router.clone();
        let msg = message.clone();
        let write_result = tokio::task::spawn_blocking(move || router.send(&msg)).await;

        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(crate::Error::InvalidMessage(format!(
                    "spawn_blocking panic: {}",
                    e
                )));
            }
        }

        self.broadcast_web_update(web::channel_message_update(message));

        // Cross-post insights from topic channels to main channel
        if helpers::should_cross_post_insight(message, &self.repo_name)
            && let Err(e) = self.cross_post_insight_to_main(message).await
        {
            warn!("Failed to cross-post insight to main channel: {}", e);
            // Don't fail the original send if cross-posting fails
        }

        Ok(())
    }

    /// Send a web push notification to all subscribed PWA clients.
    ///
    /// This is fire-and-forget: push sending runs in a background task.
    fn send_push_notification(&self, title: &str, body: &str, tag: &str) {
        if let Some(ref pm) = self.push_manager {
            let payload = crate::push::PushPayload {
                title: title.to_string(),
                body: body.to_string(),
                tag: Some(tag.to_string()),
                url: None,
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

    /// Cross-post an insight message to the main channel.
    ///
    /// Formats the content as `#channel-name | content` and sends it to the
    /// main channel with `source_channel` set to the original topic channel name.
    async fn cross_post_insight_to_main(&self, original: &Message) -> crate::Result<()> {
        let formatted_content = helpers::format_cross_post_content(original);

        // Create cross-posted message with source_channel attribution
        let mut cross_post = Message::for_channel(
            self.channel_router.default_channel_name(),
            &original.from,
            &formatted_content,
            original.message_type.clone(),
        );
        cross_post.source_channel = Some(original.channel_name().to_string());

        // Send to main channel using the router
        let router = self.channel_router.clone();
        let msg = cross_post.clone();
        let write_result = tokio::task::spawn_blocking(move || router.send(&msg)).await;

        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(crate::Error::InvalidMessage(format!(
                    "spawn_blocking panic during cross-post: {}",
                    e
                )));
            }
        }

        // Broadcast the cross-posted message to WebSocket clients
        self.broadcast_web_update(web::channel_message_update(&cross_post));

        Ok(())
    }

    /// Broadcast a coworker status change to WebSocket clients.
    fn broadcast_coworker_update(&self, name: &str, status: &str, current_task: Option<&str>) {
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

        // Look up the task's assigned channel
        let ps = self.persistent_state.lock().await;
        ps.task_channel.get(&task_id).cloned()
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

/// Persist session info for all running coworkers before daemon shutdown.
///
/// Collects session data from SessionManager and enriches it with task/PR/purpose
/// info from CoworkerManager and persistent state, then saves to daemon-state.json.
fn merge_headless_sessions(
    persisted: &mut HashMap<String, crate::daemon::state::HeadlessSessionInfo>,
    running: HashMap<String, crate::daemon::state::HeadlessSessionInfo>,
) -> usize {
    // Mark existing entries as historical by default. Running entries below are
    // overwritten with fresh metadata and `resume_on_startup=true`.
    for info in persisted.values_mut() {
        info.resume_on_startup = false;
        info.pid = None;
    }

    let running_count = running.len();
    for (name, mut info) in running {
        info.resume_on_startup = true;
        persisted.insert(name, info);
    }

    running_count
}

fn parse_task_id_from_workdir(working_dir: &str) -> Option<u64> {
    let task_component = working_dir
        .split('/')
        .find(|segment| segment.starts_with("task-"))?;
    let id_part = task_component
        .strip_prefix("task-")
        .and_then(|rest| rest.split('-').next())?;
    id_part.parse::<u64>().ok()
}

fn infer_provider_from_model(model: Option<&str>) -> Option<crate::auth::AuthProvider> {
    let model = model?.to_ascii_lowercase();
    if model.contains("gpt")
        || model.contains("codex")
        || model.contains("o1")
        || model.contains("o3")
    {
        Some(crate::auth::AuthProvider::Codex)
    } else {
        Some(crate::auth::AuthProvider::Claude)
    }
}

fn parse_historical_session_info_from_log(
    path: &std::path::Path,
    coworker_name: &str,
) -> Option<crate::daemon::state::HeadlessSessionInfo> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut working_dir: Option<String> = None;
    let mut provider: Option<crate::auth::AuthProvider> = None;

    // Init event should be at the start, but scan a small prefix for resilience.
    for line in reader.lines().take(32).flatten() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let is_init = value.get("type").and_then(|v| v.as_str()) == Some("system")
            && value.get("subtype").and_then(|v| v.as_str()) == Some("init");
        if !is_init {
            continue;
        }
        session_id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        working_dir = value
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        provider = infer_provider_from_model(value.get("model").and_then(|v| v.as_str()));
        break;
    }

    let session_id = session_id?;
    let metadata = std::fs::metadata(path).ok();
    let last_active = metadata
        .and_then(|m| m.modified().ok())
        .map(chrono::DateTime::<chrono::Utc>::from)
        .unwrap_or_else(chrono::Utc::now);
    let task_id = working_dir.as_deref().and_then(parse_task_id_from_workdir);
    let purpose = task_id
        .map(|id| format!("task !{}", id))
        .unwrap_or_else(|| format!("historical session for {}", coworker_name));

    Some(crate::daemon::state::HeadlessSessionInfo {
        session_id,
        last_active,
        purpose,
        pid: None,
        coworker_type: Some("dev".to_string()),
        task_id,
        pr_number: None,
        channel: None,
        working_dir,
        provider,
        profile: None,
        resume_on_startup: false,
        initial_prompt: None,
    })
}

fn backfill_headless_sessions_from_logs(
    repo_name: &str,
    persisted: &mut HashMap<String, crate::daemon::state::HeadlessSessionInfo>,
) -> usize {
    let project_dir = crate::paths::projects_dir_for_repo(repo_name);
    backfill_headless_sessions_from_dir(&project_dir, persisted)
}

fn backfill_headless_sessions_from_dir(
    project_dir: &std::path::Path,
    persisted: &mut HashMap<String, crate::daemon::state::HeadlessSessionInfo>,
) -> usize {
    if !persisted.is_empty() {
        return 0;
    }

    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return 0;
    };

    let mut recovered = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        if !(file_name.starts_with("headless-") && file_name.ends_with(".jsonl")) {
            continue;
        }

        let name = file_name
            .trim_start_matches("headless-")
            .trim_end_matches(".jsonl")
            .to_lowercase();
        if name.is_empty() {
            continue;
        }

        if let Some(info) = parse_historical_session_info_from_log(&path, &name) {
            persisted.insert(name, info);
            recovered += 1;
        }
    }

    recovered
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
            // Check if this coworker is assigned as a reviewer
            let persistent = state.persistent_state.lock().await;
            let is_reviewer = persistent
                .github
                .pr_reviewers
                .values()
                .any(|assignment| assignment.reviewer == coworker.name);
            drop(persistent);

            if is_reviewer {
                // Reviewer coworker
                info.coworker_type = Some("reviewer".to_string());

                // Look up PR assignment from persistent state
                let persistent = state.persistent_state.lock().await;
                if let Some(assignment) = persistent
                    .github
                    .pr_reviewers
                    .values()
                    .find(|assignment| assignment.reviewer == coworker.name)
                {
                    let pr_num = assignment.pr_number;
                    info.pr_number = Some(pr_num);
                    info.purpose = format!("reviewer for PR #{}", pr_num);
                } else {
                    info.purpose = "reviewer (unassigned)".to_string();
                }
            } else {
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

    // Save enriched session info to persistent state
    {
        let mut persistent = state.persistent_state.lock().await;
        let running_count =
            merge_headless_sessions(&mut persistent.headless_sessions, session_info);
        persistent.save_for_repo(&state.repo_name)?;
        info!(
            "Persisted {} running session(s); {} total session(s) retained (including historical)",
            running_count,
            persistent.headless_sessions.len()
        );
    }

    Ok(())
}

/// Run the full snapshot→evaluate→execute pipeline for a daemon event.
async fn run_tick(event: &events::DaemonEvent, state: &DaemonState) {
    let mut snap = snapshot::collect_world_snapshot(state).await;

    // For RateLimitCheckTick, fetch fresh rate limit data before evaluation
    if matches!(event, events::DaemonEvent::RateLimitCheckTick) {
        snap.freshly_fetched_rate_limit = crate::github_rate_limit::GitHubRateLimit::fetch().await;
    }

    let tick_effects = events::evaluate_tick(event, &snap, state).await;
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

    // Load project config once — used for project name, repo paths, and worktree managers.
    let full_project_config = crate::config::load_full_project_config(&repo_name);

    // Derive project name: explicit --project flag > config.toml [project].name > repo name.
    // Create channel router for the repo
    let channel_base_dir = crate::paths::projects_dir_for_repo(&repo_name);
    let channel_router = crate::ChannelRouter::new(&channel_base_dir, "midtown");
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
            repo: repo_name.clone(),
        };
        match start_webhook_server(
            webhook_config,
            Some(coworker_manager.clone()),
            all_repo_paths.clone(),
            default_branch.clone(),
            config.max_coworkers,
        )
        .await
        {
            Ok((rx, updates_tx, mob_rx, push_mgr)) => {
                info!("Webhook server started on port {}", port);
                webhook_rx = Some(rx);
                web_updates_tx = Some(updates_tx);
                mobile_rx = Some(mob_rx);
                shared_push_manager = push_mgr;

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

    // Create daemon state (pass channel and web updates sender so messages
    // are broadcast to WebSocket clients in real-time)
    let state = Arc::new(DaemonState::new(
        config.socket_path.clone(),
        coworker_manager,
        repo_name.clone(),
        all_repo_paths,
        channel_router,
        web_updates_tx,
        config.max_coworkers,
        shared_push_manager,
        default_branch,
        shutdown_tx.clone(),
    )?);
    info!(
        "Max coworkers limit: {} dev slots, {} reviewer headroom (absolute cap: {})",
        config.max_coworkers,
        REVIEW_HEADROOM,
        config.max_coworkers + REVIEW_HEADROOM
    );

    // Recover coworker workflow state from their state files across daemon restarts.
    startup::recover_coworker_records(&repo_name, &state.coworkers, &state.coworker_records).await;

    // Collect PIDs of sessions we intend to recover BEFORE running the zombie scanner.
    // The scanner must skip these — they are intentionally detached processes that
    // will die naturally from broken pipes. Killing them before recover_headless_sessions
    // runs defeats session survival across daemon restarts.
    let session_pids_to_preserve = startup::recoverable_session_pids(&state.persistent_state).await;

    // Kill any zombie Claude headless processes left from crashes or unclean shutdowns.
    // Kills processes that are truly orphaned (PPID=1) OR are children of a stale
    // midtown daemon (a midtown process that is not the current daemon).
    // Excludes session-survival PIDs collected above.
    startup::kill_zombie_claude_processes(std::process::id(), &session_pids_to_preserve);

    // CRITICAL: Restore task assignments from disk BEFORE session recovery.
    // This must happen first so that the in-memory coworker_task_assignments map
    // is populated before any dispatch ticks fire. Otherwise, the task dispatch
    // sees in_progress tasks as "orphaned" and spawns duplicate coworkers.
    state.restore_task_assignments_from_disk();

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
    startup::check_claude_auth_status(&repo_name);

    // Recover headless coworker sessions from persisted state (session survival).
    // Spawns new processes with --resume <session_id> to continue previous work.
    // Old processes are NOT killed — they die naturally from broken pipes after
    // the previous daemon detached its stdin/stdout handles during shutdown.
    let recovery_effects =
        startup::recover_headless_sessions(&state.persistent_state, &repo_name).await;
    if !recovery_effects.is_empty() {
        info!(
            "Executing {} session recovery effect(s)",
            recovery_effects.len()
        );
        effects::execute_effects(recovery_effects, &state).await;
    }

    // Recover channel lead sessions for active (non-archived) topic channels.
    let channel_lead_effects =
        startup::recover_channel_lead_sessions(&state.persistent_state, &repo_name).await;
    if !channel_lead_effects.is_empty() {
        info!(
            "Executing {} channel lead recovery effect(s)",
            channel_lead_effects.len()
        );
        effects::execute_effects(channel_lead_effects, &state).await;
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

    // Timer for draining headless session events (every 2 seconds).
    // Must be fast to prevent stdout pipe buffer (64KB) from filling up,
    // which would block coworker processes and cause silent hangs.
    let mut session_drain_interval = interval(std::time::Duration::from_secs(2));
    session_drain_interval.tick().await;

    // Timer for flushing batched CI notifications (check every 5 seconds).
    // The actual flush delay is 15 seconds from the oldest buffered item.
    let mut ci_notification_flush_interval = interval(std::time::Duration::from_secs(5));
    // Skip the first tick (which fires immediately)
    ci_notification_flush_interval.tick().await;

    // Nudge any coworkers discovered on startup to continue their tasks.
    // This runs once at startup after the daemon has fully initialized.
    // Data gathering + pure decision → effects executed in background task.
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let nudge_effects = dispatch::gather_discovered_coworker_nudges(&state).await;
            effects::execute_effects(nudge_effects, &state).await;
        });
    }

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

                    // Check if this CI completion should trigger a reviewer spawn (retry logic).
                    // When the initial pending spawn (45s after PR opens) was skipped for any reason
                    // (coworker limit, CI pending, etc.), retry when CI becomes green.
                    let state_clone = Arc::clone(&state);
                    let ci_check_clone = ci_check.clone();
                    tokio::spawn(async move {
                        pr::handle_ci_completion_for_review_spawn(&state_clone, &ci_check_clone).await;
                    });

                    let mut buffer = state.ci_notification_buffer.lock().await;
                    buffer.add(ci_check);
                } else if let Err(e) = state.send_and_broadcast_async(&webhook_event.message).await {
                    error!("Failed to forward webhook message to channel: {}", e);
                }

                // Nudge PR owner when someone else comments on their PR
                if let Some(activity) = webhook_event.pr_activity {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        pr::handle_pr_comment_nudge(&state, activity).await;
                    });
                }

                // Queue a reviewer spawn after the delay (persisted in daemon-state.json)
                if let Some(pr_number) = webhook_event.needs_review {
                    let spawn_after = chrono::Utc::now()
                        + chrono::Duration::seconds(PR_REVIEW_DELAY_SECS as i64);
                    let mut ps = state.persistent_state.lock().await;
                    ps.github.record_webhook_event(pr_number);
                    ps.github.add_pending_review_spawn(pr_number, spawn_after);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to persist pending review spawn: {}", e);
                    }
                    info!(
                        "Webhook: PR #{} queued for review spawn in {}s",
                        pr_number, PR_REVIEW_DELAY_SECS
                    );
                }

                // Handle PR-opened events: store author session + auto-set task PR association
                if let Some(ref pr_opened) = webhook_event.pr_opened {
                    let mut pr_effects = Vec::new();

                    // Store author session for PR handoff (allows any coworker to resume the PR)
                    if let Some(ref author) = pr_opened.author_coworker {
                        if let Some(session_id) = state.coworkers.get_session_id(author) {
                            pr_effects.push(effects::Effect::StorePrAuthorSession {
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
                    }

                    // Auto-set task PR association when PR title contains [Midtown !XX]
                    if let Some(task_id) =
                        crate::tasks::extract_task_id_from_pr_title(&pr_opened.title)
                    {
                        pr_effects.push(effects::Effect::SetTaskPr {
                            task_id: task_id.to_string(),
                            pr_number: pr_opened.pr_number,
                            repo_name: state.repo_name.clone(),
                        });
                        info!(
                            "Auto-setting PR #{} association for task !{}",
                            pr_opened.pr_number, task_id
                        );
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

                    // Auto-complete task when PR title contains [Midtown #XX]
                    if let Some(pr_merged_info) = webhook_event.pr_merged_info {
                        let completion_effects = dispatch::build_task_completion_effects(
                            &pr_merged_info.title,
                            pr_merged_info.pr_number,
                            &state.repo_name,
                        );
                        if !completion_effects.is_empty() {
                            effects::execute_effects(completion_effects, &state).await;
                        }
                    }
                }

                // Nudge lead when a CI check fails on the default branch
                if let Some(nudge_msg) = webhook_event.ci_failed_on_default_branch {
                    state.nudge_lead(&nudge_msg).await;
                    info!("Nudged lead about CI failure on default branch");
                    state.send_push_notification(
                        "CI failed on default branch",
                        &nudge_msg,
                        "ci_failure",
                    );
                }

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

                // Cache review status immediately from webhook data (avoids API calls)
                if let Some(pr_number) = webhook_event.reviewed_pr {
                    debug!(
                        "Webhook: caching review status for PR #{} (review comment detected)",
                        pr_number
                    );
                    let mut ps = state.persistent_state.lock().await;
                    ps.github.mark_reviewed_pr(pr_number);
                }

                // Record CI check duration for statistics tracking
                if let Some(duration) = webhook_event.check_duration {
                    debug!(
                        "Webhook: recording CI check duration for '{}': {}s",
                        duration.check_name, duration.duration_secs
                    );
                    let mut ps = state.persistent_state.lock().await;
                    ps.ci_stats.record_duration(&duration.check_name, duration.duration_secs);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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

            // Process user channel posts through the daemon (handles nudge, etc.)
            Some(mobile_post) = async {
                match mobile_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let content = &mobile_post.content;
                let channel = mobile_post.channel.as_deref();
                let sender = state.user_display_name.as_deref().unwrap_or("user");
                rpc_channel::handle_channel_post(
                    RequestId::Null,
                    sender,
                    content,
                    channel,
                    &state,
                ).await;
            }

            // Drain events from headless sessions to prevent stdout buffer filling up.
            // Also detects process exits and updates health state for the snapshot.
            _ = session_drain_interval.tick() => {
                let (events, stopped, stderr_by_name) = state.session_manager.drain_events().await;

                // Update health state from SessionManager (used by snapshot for decision functions)
                let health = state.session_manager.collect_health().await;
                {
                    let mut hh = state.headless_health.write().unwrap();
                    *hh = health;
                }

                // Log headless events at debug level for diagnostics.
                // Also backfill session_id in persistent state when init events arrive.
                let mut needs_persist_save = false;
                for (name, session_events) in &events {
                    for event in session_events {
                        debug!(coworker = %name, event = ?event, "headless session event");

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
                            if let Some(info) = ps.headless_sessions.get_mut(name)
                                && (info.session_id.is_empty() || info.session_id != *sid)
                            {
                                info!("Backfilling session_id for '{}': {}", name, sid);
                                info.session_id = sid.clone();
                                needs_persist_save = true;
                            }
                            // Also backfill channel_lead_sessions for channel lead sessions.
                            // Channel leads use the channel name directly as their session name,
                            // so we can look up the key directly in channel_lead_sessions.
                            if let Some(stored_id) =
                                ps.channel_lead_sessions.get_mut(name.as_str())
                                && (stored_id.is_empty() || stored_id != sid)
                            {
                                info!(
                                    "Backfilling channel lead session_id for '{}': {}",
                                    name, sid
                                );
                                *stored_id = sid.clone();
                                needs_persist_save = true;
                            }
                            // Update in-memory reverse maps when session gets its ID.
                            if let Some(record) = ps.sessions.get(sid) {
                                if let Some(ref sname) = record.current_name {
                                    state
                                        .name_to_session
                                        .lock()
                                        .unwrap()
                                        .insert(sname.clone(), sid.clone());
                                    state
                                        .session_to_name
                                        .lock()
                                        .unwrap()
                                        .insert(sid.clone(), sname.clone());
                                }
                                if let Some(ref task_id) = record.task_id {
                                    state
                                        .task_to_session
                                        .lock()
                                        .unwrap()
                                        .insert(task_id.clone(), sid.clone());
                                }
                            }
                        }
                    }
                }
                if needs_persist_save {
                    let ps = state.persistent_state.lock().await;
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to save persistent state after session_id backfill: {}", e);
                    }
                }

                // Process headless lead output and post aggregated text to channel.
                // Routes through Effect pipeline to maintain architecture consistency.
                let lead_effects = stream::process_lead_output(&events);
                effects::execute_effects(lead_effects, &state).await;

                // Broadcast universal events (tool calls) to WebSocket clients.
                // Main lead's tool calls go to the main channel; each channel lead's
                // tool calls are tagged with the channel name so the web UI filters them.
                let universal_effects = {
                    let ps = state.persistent_state.lock().await;
                    stream::process_universal_events(&events, &ps.channel_lead_sessions)
                };
                effects::execute_effects(universal_effects, &state).await;

                // Defense-in-depth: check process liveness via try_wait() to catch
                // sessions where the process exited but drain_events didn't detect it
                // (e.g., pipe buffering issues, partial reads, timing races).
                let reconciled = state.session_manager.reconcile_process_health().await;
                let mut all_stopped: Vec<String> = stopped;
                if !reconciled.is_empty() {
                    warn!(
                        "Process reconciliation found {} dead session(s) missed by drain_events: {:?}",
                        reconciled.len(),
                        reconciled
                    );
                    all_stopped.extend(reconciled);
                }

                // Handle stopped sessions: deregister, record stop time, post to channel
                for name in all_stopped {
                    warn!("Headless session '{}' exited unexpectedly", name);

                    // Check if this was a failed resume attempt BEFORE removing
                    // the session (remove deletes it from the map).
                    // SAFETY: This check must happen before any cleanup operations
                    // that could remove the session from the map. All daemon event
                    // handling is single-threaded, so no concurrent remove() is possible.
                    let failed_resume = state.session_manager.was_failed_resume(&name).await;

                    // Remove from session manager tracking (session-death-specific:
                    // shutdown path uses session_manager.shutdown() instead)
                    state.session_manager.remove(&name).await;
                    // Clean up all transient coworker state (shared with shutdown path).
                    // This includes releasing the name back to NamePool and cleaning
                    // up session reverse maps (name_to_session, session_to_name,
                    // task_to_session).
                    state.cleanup_coworker_state(&name).await;

                    // Only clear session_id when the resume itself failed
                    // (session died within 30s of a resume spawn). This means
                    // the session data doesn't exist on disk and retrying the
                    // same session_id would loop. Sessions that ran longer
                    // likely have valid data on disk — keep their session_id
                    // so the next spawn can try to resume them.
                    if failed_resume {
                        let mut ps = state.persistent_state.lock().await;
                        if let Some(info) = ps.headless_sessions.get_mut(&name)
                            && !info.session_id.is_empty()
                        {
                            info!(
                                "Clearing stale session_id for '{}' after failed resume (was: {})",
                                name, info.session_id
                            );
                            info.session_id.clear();
                        }
                        // Also clear channel_lead_sessions for channel lead sessions.
                        // Channel leads use the channel name as their session name, so
                        // name == channel_name. Without this, the stale ID persists in
                        // channel_lead_sessions and causes a crash loop on daemon restart.
                        if let Some(stored_id) = ps.channel_lead_sessions.get_mut(name.as_str())
                            && !stored_id.is_empty()
                        {
                            info!(
                                "Clearing stale channel_lead_sessions entry for '{}' after failed resume (was: {})",
                                name, stored_id
                            );
                            stored_id.clear();
                        }
                        if let Err(e) = ps.save_for_repo(&state.repo_name) {
                            warn!("Failed to save persistent state after clearing session_id: {}", e);
                        }
                    }

                    // Format message with stderr if available
                    let message_text = if let Some(stderr_lines) = stderr_by_name.get(&name) {
                        if stderr_lines.is_empty() {
                            format!("⚠️ Coworker {} session exited unexpectedly", name)
                        } else {
                            // Include last N lines of stderr (up to 10 lines)
                            let last_n: Vec<&str> = stderr_lines
                                .iter()
                                .rev()
                                .take(10)
                                .rev()
                                .map(|s| s.as_str())
                                .collect();
                            format!(
                                "⚠️ Coworker {} session exited unexpectedly\n\nStderr ({} lines):\n{}",
                                name,
                                stderr_lines.len(),
                                last_n.join("\n")
                            )
                        }
                    } else {
                        format!("⚠️ Coworker {} session exited unexpectedly", name)
                    };

                    let msg = crate::message::Message::text("midtown", message_text);
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post session exit message for {}: {}", name, e);
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
                let snap = snapshot::collect_world_snapshot(&state).await;
                let tick_effects = events::evaluate_tick(&events::DaemonEvent::TaskDispatchTick, &snap, &state).await;
                // Mark in-flight tasks BEFORE executing effects to prevent race conditions.
                // If the next tick fires while effects are executing, it will skip these tasks.
                state.mark_in_flight_spawns_from_effects(&tick_effects);
                effects::execute_effects(tick_effects, &state).await;
                // Orphan worktree cleanup: gather data (blocking git ops + cache reads),
                // then build effects via pure decision function.
                // Pass task owners to suppress warnings for idle worktrees with no active work.
                let task_owners: Vec<String> = snap.in_progress_tasks.iter()
                    .map(|(_, _, owner)| owner.clone())
                    .collect();
                if let Some(orphan_data) = dispatch::gather_orphan_cleanup_data(&state, &task_owners).await {
                    let orphan_effects = dispatch::decide_orphan_cleanup(&orphan_data);
                    effects::execute_effects(orphan_effects, &state).await;
                }
                // Process any pending webhook review spawns whose delay has expired
                let review_snap = snapshot::collect_world_snapshot(&state).await;
                let review_effects = pr::process_pending_review_spawns(&review_snap, &state).await;
                effects::execute_effects(review_effects, &state).await;
            }

            // Periodic channel log rotation (only rotates the default/main channel)
            _ = channel_rotation_interval.tick() => {
                let default_channel = match state.channel_router.default_channel() {
                    Ok(ch) => ch,
                    Err(e) => {
                        error!("Failed to get default channel for rotation: {}", e);
                        continue;
                    }
                };
                if default_channel.needs_rotation(CHANNEL_ROTATION_MAX_AGE_HOURS) {
                    info!("Channel rotation triggered (oldest message > {}h)", CHANNEL_ROTATION_MAX_AGE_HOURS);
                    match default_channel.rotate(CHANNEL_ROTATION_RETAIN_MINUTES) {
                        Ok(archived) => {
                            info!("Channel rotated: {} messages archived", archived);
                            let msg = Message::system(
                                format!("Channel log rotated: {} old messages archived", archived)
                            );
                            if let Err(e) = state.send_and_broadcast_async(&msg).await {
                                warn!("Failed to send rotation notification: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Channel rotation failed: {}", e);
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
                        let message = Message::text("github", msg);
                        if let Err(e) = state.send_and_broadcast_async(&message).await {
                            error!("Failed to post batched CI notification: {}", e);
                        }
                    }
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

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// Per-coworker decision helpers for unit tests. The batch `decide_*` functions
// in `rules.rs` handle the full coworker set; these single-coworker variants
// make individual test cases easier to write.

#[path = "mod_tests.rs"]
#[cfg(test)]
mod tests;
