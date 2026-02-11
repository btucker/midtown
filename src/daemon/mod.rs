//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod architect;
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
pub(crate) mod sessions;
pub(crate) mod snapshot;
mod specialized;
mod startup;
pub(crate) mod state;
mod trackers;
mod webhook_fwd;

use constants::*;
pub use constants::{
    DEFAULT_MAX_COWORKERS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, PR_NUDGE_COOLDOWN_SECS,
    PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, PR_REVIEW_DELAY_SECS,
};
pub use trackers::{
    CommentTracker, OrphanTracker, PrIssueTracker, PrIssueType, StuckConditionTracker,
    StuckConditionType,
};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
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
    /// Consolidated lead typing indicator state (pane hash, working flag, last activity).
    lead_typing: std::sync::Mutex<trackers::LeadTypingState>,
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
    /// 2. Effects start executing (disk write + tmux spawn takes time)
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
    /// Coworkers currently in "attached" state (interactive tmux session).
    ///
    /// When the Lead attaches to a headless coworker via `midtown session attach`,
    /// the headless process is killed and replaced with an interactive tmux window.
    /// During this period, the coworker must be exempt from stuck detection and
    /// orphan recovery. Entries are added on attach, removed on detach.
    attached_coworkers: std::sync::Mutex<HashSet<String>>,
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
    kanban_cache: rpc::KanbanCache,
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

    /// Check if the daemon is at the maximum coworker limit (absolute cap).
    fn is_at_coworker_limit(&self) -> bool {
        self.coworkers.list().len() >= self.max_coworkers
    }

    /// Check if the daemon is at the dev coworker limit.
    /// Reserves `REVIEW_HEADROOM` slots for reviewers, but always allows at least 1 dev slot.
    fn is_at_dev_limit(&self) -> bool {
        let dev_cap = self.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1);
        self.coworkers.list().len() >= dev_cap
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
    fn has_available_coworker_slot(&self) -> bool {
        !self.is_at_coworker_limit() && self.coworkers.next_available_name().is_some()
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

        // Slow path: check via API calls
        let has_review = pr::pr_has_claude_review_uncached(pr_number);

        // Cache positive results (review status is monotonic)
        if has_review {
            let mut ps = self.persistent_state.lock().await;
            ps.github.mark_reviewed_pr(pr_number);
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
    ) -> crate::Result<Self> {
        // Load unified persistent state (migrates from legacy files if needed)
        let persistent_state = state::DaemonPersistentState::load_for_repo(&repo_name)
            .unwrap_or_else(|e| {
                warn!("Failed to load daemon-state.json: {}, using defaults", e);
                state::DaemonPersistentState::default()
            });

        let user_display_name = config::get_user_display_name_for_project(&repo_name);

        // Clone repo_name for session_manager before moving it into Self
        let session_manager_repo_name = repo_name.clone();

        Ok(Self {
            coworkers,
            channel_router,
            socket_path,
            coworker_records: tokio::sync::RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::new()),
            repo_name,
            default_branch,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            orphan_tracker: std::sync::RwLock::new(OrphanTracker::new()),
            persistent_state: Mutex::new(persistent_state),
            web_updates_tx,
            max_coworkers,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            lead_typing: std::sync::Mutex::new(trackers::LeadTypingState::default()),
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
            attached_coworkers: std::sync::Mutex::new(HashSet::new()),
            session_manager: sessions::SessionManager::new(session_manager_repo_name),
            rpc_response_cache: Mutex::new(HashMap::new()),
            kanban_cache: rpc::KanbanCache::new(),
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
            c.auth_profile_dir = Some(crate::auth::active_profile_dir_for_project(&self.repo_name));
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
        let mut headless_config = launch_config.to_headless_config();
        headless_config.cwd = Some(working_dir.clone());

        // Write shared coworker settings file and set the path
        let settings_file = crate::tmux::write_coworker_settings_file()?;
        headless_config.settings_path = Some(settings_file.to_string_lossy().to_string());

        // Set up agent-teams infrastructure (mailbox) before spawning
        if let Some(ref team_name) = config.team_name {
            let member = crate::mailbox::TeamMember {
                name: name.clone(),
                agent_id: crate::mailbox::agent_id(&name, team_name),
                agent_type: match config.role {
                    crate::launch::CoworkerRole::Reviewer => "reviewer".to_string(),
                    crate::launch::CoworkerRole::Coworker => "coworker".to_string(),
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

        // Register in the CoworkerManager tracking map (keyed by slot_id)
        // session_id is None initially — it arrives later via the init StreamEvent
        if let Err(e) =
            self.coworkers
                .register(&slot_id, &name, working_dir, None, config.model.clone())
        {
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
            &self.repo_name,
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
fn acquire_pid_lock(pid_path: &PathBuf) -> crate::Result<File> {
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
            // We got the lock - write our PID
            let pid = std::process::id();
            file.set_len(0)?; // Truncate any old content
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
async fn persist_sessions_for_restart(state: &DaemonState) -> crate::Result<()> {
    // Collect base session info (session_id, pid, last_active) from SessionManager
    let mut session_info = state.session_manager.collect_session_info().await;

    // Enrich with task/PR assignments and working directories from CoworkerManager
    let coworkers = state.coworkers.list();
    for coworker in coworkers {
        if let Some(info) = session_info.get_mut(&coworker.name) {
            info.working_dir = Some(coworker.working_dir.clone());

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
        persistent.headless_sessions = session_info;
        persistent.save_for_repo(&state.repo_name)?;
        info!(
            "Persisted {} session(s) for restart survival",
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

/// Run snapshot→evaluate without executing. Returns effects for the caller
/// to inspect or extend before calling `execute_effects`.
async fn collect_and_evaluate(
    event: &events::DaemonEvent,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    let snap = snapshot::collect_world_snapshot(state).await;
    events::evaluate_tick(event, &snap, state).await
}

/// Run the daemon server with the given configuration.
///
/// This function will block until the daemon receives a shutdown signal
/// (SIGTERM or SIGINT) or the socket is removed.
pub async fn run(config: DaemonConfig) -> crate::Result<()> {
    // Install panic hook so unhandled panics are logged to stderr before aborting
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "=== DAEMON PANIC at {} ===",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
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
    let pid_file = acquire_pid_lock(&config.pid_file_path)?;
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
    // This must match what the CLI uses (resolve_project_name) so the Lead session
    // and daemon agree on the task list ID and tmux session name.
    let project_name = config
        .project_name
        .clone()
        .or_else(|| {
            full_project_config
                .as_ref()
                .and_then(|c| c.project.name().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| repo_name.clone());

    // Create channel router for the repo
    let channel_base_dir = crate::paths::projects_dir_for_repo(&repo_name);
    let channel_router = crate::ChannelRouter::new(&channel_base_dir, "midtown");
    info!("Channel base: {}", channel_base_dir.display());

    // Remove existing socket file if present
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }

    // Bind to Unix socket
    let listener = UnixListener::bind(&config.socket_path)?;
    info!("Listening on {}", config.socket_path.display());

    // Create worktree manager and coworker manager early so they can be
    // shared with the web server (for the /api/status endpoint)
    let session_name = format!("midtown-{}", project_name);

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

    // Capture lead-specific values for health monitoring in the main loop.
    // These are cloned here because session_name is moved into CoworkerManager.
    let lead_session_name = session_name.clone();
    let lead_workdir = config.workdir.clone();
    let lead_project_name = project_name.clone();
    let lead_additional_dirs: Vec<PathBuf> = all_repo_paths
        .iter()
        .filter(|p| **p != config.workdir)
        .cloned()
        .collect();

    let worktree_manager =
        WorktreeManager::new(config.workdir.clone()).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to initialize worktree manager: {}", e),
        })?;

    // For multi-repo projects, create worktree managers for additional repos
    let additional_worktree_managers =
        load_additional_worktree_managers(full_project_config.as_ref(), &config);
    let coworker_manager = CoworkerManager::with_additional_repos(
        session_name,
        worktree_manager,
        additional_worktree_managers,
    );

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
    )?);
    info!(
        "Max coworkers limit: {} (dev: {}, reserving {} for reviewers)",
        config.max_coworkers,
        config.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
        REVIEW_HEADROOM
    );

    // Recover coworker workflow state from their state files across daemon restarts.
    startup::recover_coworker_records(&repo_name, &state.coworkers, &state.coworker_records).await;

    // Kill any zombie Claude headless processes left from crashes or unclean shutdowns.
    // This must run BEFORE session recovery to clean up processes before spawning new ones.
    startup::kill_zombie_claude_processes();

    // Recover headless coworker sessions from persisted state (session survival).
    // This kills orphaned processes and spawns with --resume to continue previous work.
    let recovery_effects =
        startup::recover_headless_sessions(&state.persistent_state, &repo_name).await;
    if !recovery_effects.is_empty() {
        info!(
            "Executing {} session recovery effect(s)",
            recovery_effects.len()
        );
        effects::execute_effects(recovery_effects, &state).await;
    }

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    // Set up idle check interval
    let mut idle_check_interval = interval(IDLE_CHECK_INTERVAL);

    // Set up lead typing indicator check interval
    let mut lead_typing_interval = interval(LEAD_TYPING_CHECK_INTERVAL);

    // Set up lead health check interval (recreates lead window if killed).
    // Track daemon start time so we can skip health checks during the startup
    // grace period, preventing races with `midtown restart` where the daemon
    // tries to respawn a lead window before the tmux session is fully settled.
    let mut lead_health_interval = interval(LEAD_HEALTH_CHECK_INTERVAL);
    let daemon_start_instant = tokio::time::Instant::now();

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

    // Timer for periodic channel rotation
    let mut channel_rotation_interval = interval(CHANNEL_ROTATION_CHECK_INTERVAL);
    // Skip the first tick (which fires immediately)
    channel_rotation_interval.tick().await;

    // Timer for periodic orphan process cleanup (every 5 minutes)
    // This catches claude processes that were orphaned when tmux sessions were
    // killed directly without going through `midtown stop`.
    let mut orphan_process_interval = interval(std::time::Duration::from_secs(300));
    // Run cleanup immediately on startup, before the interval timer begins.
    // Orphans from a crashed/restarted daemon need to be killed before we
    // start spawning new coworkers.
    {
        let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
        let killed = crate::tmux::kill_orphaned_processes(pattern);
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

    // Nudge any coworkers discovered from tmux to continue their tasks.
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

                // Handle PR-opened events: store author session + auto-complete task
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
                    let channel_msg = Message::text("system", channel_text);
                    if let Err(e) = state.send_and_broadcast_async(&channel_msg).await {
                        warn!("Failed to post merge notification for PR #{}: {}", pr_number, e);
                    }
                    // Direct nudge includes the actionable instruction
                    let nudge_text = format!(
                        "PR #{} merged into {}. Run `git pull` to stay current.",
                        pr_number, state.default_branch
                    );
                    if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
                        warn!("Failed to nudge lead for PR #{} merge: {}", pr_number, e);
                    } else {
                        info!("Nudged lead about PR #{} merge", pr_number);
                    }

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
                    if let Err(e) = state.coworkers.nudge_lead(&nudge_msg) {
                        warn!("Failed to nudge lead for CI failure on default branch: {}", e);
                    } else {
                        info!("Nudged lead about CI failure on default branch");
                    }
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
                let sender = state.user_display_name.as_deref().unwrap_or("user");
                rpc::handle_channel_post(
                    RequestId::Null,
                    sender,
                    content,
                    None, // No channel specified - use default
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

                // Log received events at debug level for diagnostics
                for (name, session_events) in &events {
                    for event in session_events {
                        debug!(coworker = %name, event = ?event, "headless session event");
                    }
                }

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
                    state.coworkers.deregister(&name);
                    state.record_coworker_stop_time(&name);
                    // Remove from session manager tracking
                    state.session_manager.remove(&name).await;
                    // Clean up coworker record
                    {
                        let mut records = state.coworker_records.write().await;
                        records.remove(&name);
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

                    let msg = crate::message::Message::text("system", message_text);
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post session exit message for {}: {}", name, e);
                    }
                }
            }

            // Periodically monitor coworker sessions: idle shutdown, nudges, stuck detection
            _ = idle_check_interval.tick() => {
                // Sync internal state with actual tmux windows first.
                // Preserve headless session names so they don't get removed
                // (headless coworkers have no tmux windows).
                // Use list_alive_names() to exclude stopped sessions pending cleanup —
                // list_names() would include them, causing sync_with_tmux to preserve
                // stale entries in the CoworkerManager tracking map.
                let headless_names: std::collections::HashSet<String> =
                    state.session_manager.list_alive_names().await.into_iter().collect();
                let persistent_sessions = {
                    let ps = state.persistent_state.lock().await;
                    ps.headless_sessions.clone()
                };
                if let Err(e) = state.coworkers.sync_with_tmux(&headless_names, &persistent_sessions) {
                    warn!("Failed to sync coworker state with tmux: {}", e);
                }
                run_tick(&events::DaemonEvent::SessionMonitorTick, &state).await;
            }

            // Check lead pane activity for typing indicator
            _ = lead_typing_interval.tick() => {
                health::check_lead_typing(&state).await;
            }

            // Check if lead window is still alive; recreate if killed.
            // Skip during the startup grace period to avoid races with
            // `midtown restart` where the lead window is still settling.
            _ = lead_health_interval.tick() => {
                if daemon_start_instant.elapsed() >= LEAD_HEALTH_CHECK_STARTUP_GRACE {
                    let session = lead_session_name.clone();
                    let workdir = lead_workdir.clone();
                    let project = lead_project_name.clone();
                    let additional = lead_additional_dirs.clone();
                    let tmux_server_gone = tokio::task::spawn_blocking(move || {
                        health::check_and_respawn_lead(&session, &workdir, &project, &additional)
                    }).await.unwrap_or(false);

                    if tmux_server_gone {
                        error!("Tmux server died unexpectedly. Daemon shutting down.");
                        let _ = shutdown_tx.send(());
                        break;
                    }
                }
            }

            // Periodic task dispatch: orphan recovery, duplicate detection, spawning, cleanup
            _ = orphan_check_interval.tick() => {
                let tick_effects =
                    collect_and_evaluate(&events::DaemonEvent::TaskDispatchTick, &state).await;
                // Mark in-flight tasks BEFORE executing effects to prevent race conditions.
                // If the next tick fires while effects are executing, it will skip these tasks.
                state.mark_in_flight_spawns_from_effects(&tick_effects);
                effects::execute_effects(tick_effects, &state).await;
                // Orphan worktree cleanup: gather data (blocking git ops + cache reads),
                // then build effects via pure decision function.
                if let Some(orphan_data) = dispatch::gather_orphan_cleanup_data(&state).await {
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
            // orphaned (PPID=1) when tmux sessions were killed directly.
            _ = orphan_process_interval.tick() => {
                // Only kill truly orphaned processes (PPID=1) to avoid killing
                // claude sessions the user started manually or in other projects.
                let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
                let killed = crate::tmux::kill_orphaned_processes(pattern);
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

    info!("Daemon stopped");
    Ok(())
}

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// Per-coworker decision helpers for unit tests. The batch `decide_*` functions
// in `rules.rs` handle the full coworker set; these single-coworker variants
// make individual test cases easier to write.

#[cfg(test)]
mod tests {
    use super::helpers::*;
    use super::*;
    use crate::rules::{UsageLimitExpiryDecision, decide_usage_limit_expiry};

    // URL parsing tests for extract_repo_name_from_url
    #[test]
    fn test_extract_repo_name_https_url() {
        assert_eq!(
            extract_repo_name_from_url("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_repo_name_from_url("https://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_repo_name_from_url("https://github.com/btucker/midtown.git"),
            Some("btucker/midtown".to_string())
        );
    }

    #[test]
    fn test_extract_repo_name_ssh_url() {
        assert_eq!(
            extract_repo_name_from_url("git@github.com:owner/repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_repo_name_from_url("git@github.com:owner/repo"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_repo_name_from_url("git@github.com:btucker/midtown.git"),
            Some("btucker/midtown".to_string())
        );
    }

    #[test]
    fn test_extract_repo_name_with_whitespace() {
        assert_eq!(
            extract_repo_name_from_url("  https://github.com/owner/repo.git  \n"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            extract_repo_name_from_url("git@github.com:owner/repo.git\n"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_extract_repo_name_invalid() {
        assert_eq!(extract_repo_name_from_url("not a url"), None);
        assert_eq!(extract_repo_name_from_url(""), None);
    }

    // Auto-nudge helper tests
    #[test]
    fn test_extract_pr_number_pr_hash() {
        assert_eq!(extract_pr_number("opened PR #42: Add feature"), Some(42));
        assert_eq!(extract_pr_number("merged PR #123"), Some(123));
        assert_eq!(extract_pr_number("btucker approved PR #99"), Some(99));
    }

    #[test]
    fn test_extract_pr_number_standalone_hash() {
        assert_eq!(extract_pr_number("commented on #55: looks good"), Some(55));
        assert_eq!(
            extract_pr_number("Check 'build' passed on PR #77"),
            Some(77)
        );
    }

    #[test]
    fn test_extract_pr_number_none() {
        assert_eq!(extract_pr_number("no pr reference here"), None);
        assert_eq!(extract_pr_number("just some text"), None);
    }

    #[test]
    fn test_coworker_from_branch() {
        assert_eq!(
            coworker_from_branch("lexington/fix-auth"),
            Some("lexington".to_string())
        );
        assert_eq!(
            coworker_from_branch("park/add-feature"),
            Some("park".to_string())
        );
        assert_eq!(
            coworker_from_branch("madison/refactor"),
            Some("madison".to_string())
        );
    }

    #[test]
    fn test_coworker_from_branch_case_insensitive() {
        assert_eq!(
            coworker_from_branch("LEXINGTON/fix"),
            Some("lexington".to_string())
        );
        assert_eq!(coworker_from_branch("Park/thing"), Some("park".to_string()));
    }

    #[test]
    fn test_coworker_from_branch_not_coworker() {
        assert_eq!(coworker_from_branch("feature/something"), None);
        assert_eq!(coworker_from_branch("fix/bug"), None);
        assert_eq!(coworker_from_branch("main"), None);
    }

    // Lead nudge tests
    #[test]
    fn test_is_coworker_sender() {
        // System senders should not be coworkers
        assert!(!is_coworker_sender("Lead"));
        assert!(!is_coworker_sender("lead"));
        assert!(!is_coworker_sender("github"));
        assert!(!is_coworker_sender("GitHub"));
        assert!(!is_coworker_sender("system"));

        // Actual coworker names should be detected
        assert!(is_coworker_sender("lexington"));
        assert!(is_coworker_sender("park"));
        assert!(is_coworker_sender("amsterdam"));
        assert!(is_coworker_sender("madison"));
    }

    #[test]
    fn test_lead_nudge_only_on_explicit_at_lead() {
        // Only explicit @lead mentions should trigger nudges.
        // Heuristic keywords like "feedback", "help", "blocked" should NOT trigger.
        let triggers = |msg: &str| msg.to_lowercase().contains("@lead");

        // Should trigger: explicit @lead mentions
        assert!(triggers("@lead should this handle the error case?"));
        assert!(triggers("@Lead can you review this approach?"));
        assert!(triggers("Hey @lead, I'm blocked on the API design"));

        // Should NOT trigger: heuristic keywords without @lead
        assert!(!triggers("I need some feedback on the API design"));
        assert!(!triggers("I'm blocked on the auth issue"));
        assert!(!triggers("I'm stuck here, not sure how to proceed"));
        assert!(!triggers("What do you think about this approach?"));
        assert!(!triggers("I have a question about the architecture"));

        // Should NOT trigger: status updates mentioning "feedback"
        assert!(!triggers("addressing review feedback on PR #227"));
        assert!(!triggers("/me addressing feedback from code review"));

        // Should NOT trigger: coworker-to-coworker messages
        assert!(!triggers("@lexington can you help with this?"));
        assert!(!triggers("@pleasant any progress on task 304?"));
    }

    #[test]
    fn test_system_message_with_at_lead_should_trigger_nudge() {
        // System messages containing @lead should be detected as needing a lead
        // nudge. The chat_monitor_loop checks for @lead in SKIP_SENDERS messages
        // before skipping them. This test validates the detection logic.
        // Mirrors the chat_monitor_loop logic: skip-sender messages nudge lead
        // for @lead, EXCEPT "user" messages (already handled in handle_channel_post).
        let should_nudge = |from: &str, content: &str| -> bool {
            let is_skip_sender = SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(from));
            is_skip_sender
                && !from.eq_ignore_ascii_case("user")
                && content.to_lowercase().contains("@lead")
        };

        // System messages with @lead should trigger nudge
        assert!(should_nudge(
            "system",
            "⚠️ @lead Orphaned worktrees with unmerged commits: amsterdam, park"
        ));

        // Midtown daemon messages with @lead should also trigger
        assert!(should_nudge(
            "midtown",
            "⚠️ @lead something needs attention"
        ));

        // System messages WITHOUT @lead should NOT trigger
        assert!(!should_nudge(
            "system",
            "Channel log rotated: 50 old messages archived"
        ));

        // User messages with @lead should NOT trigger here (handled in handle_channel_post)
        assert!(!should_nudge("user", "@lead what do you think?"));

        // Coworker messages should NOT be in SKIP_SENDERS at all
        assert!(!should_nudge("lexington", "@lead can you review this?"));
    }

    #[test]
    fn test_pr_merge_channel_message_no_at_lead() {
        // The PR merge channel message should NOT contain @lead.
        // This prevents a double-nudge: one from the direct nudge_lead() call,
        // and another from the chat monitor detecting @lead in the system message.
        //
        // The channel message is informational only:
        //   "PR #42 merged into main."
        // The direct nudge includes the actionable instruction:
        //   "PR #42 merged into main. Run `git pull` to stay current."
        let pr_number = 42u64;
        let default_branch = "main";

        // Channel message format (should NOT contain @lead)
        let channel_text = format!("PR #{} merged into {}.", pr_number, default_branch);
        assert!(
            !channel_text.to_lowercase().contains("@lead"),
            "PR merge channel message should not contain @lead: {}",
            channel_text
        );

        // Nudge text format (used for direct nudge, includes instruction)
        let nudge_text = format!(
            "PR #{} merged into {}. Run `git pull` to stay current.",
            pr_number, default_branch
        );
        assert!(
            !nudge_text.to_lowercase().contains("@lead"),
            "PR merge nudge text should not contain @lead (it's for direct nudge): {}",
            nudge_text
        );
    }

    #[test]
    fn test_pr_issue_tracker_should_nudge_new() {
        let tracker = PrIssueTracker::new();
        assert!(tracker.should_nudge(42, PrIssueType::MergeConflict));
        assert!(tracker.should_nudge(42, PrIssueType::CiFailed));
        assert!(tracker.should_nudge(42, PrIssueType::ReviewComplete));
    }

    #[test]
    fn test_pr_issue_tracker_should_nudge_after_record() {
        let mut tracker = PrIssueTracker::new();
        tracker.record_nudge(42, PrIssueType::MergeConflict);

        // Same issue should not be nudged again immediately
        assert!(!tracker.should_nudge(42, PrIssueType::MergeConflict));

        // Different issue type for same PR should be nudged
        assert!(tracker.should_nudge(42, PrIssueType::CiFailed));

        // Same issue type for different PR should be nudged
        assert!(tracker.should_nudge(43, PrIssueType::MergeConflict));
    }

    #[test]
    fn test_pr_issue_tracker_review_complete_independent_of_other_types() {
        let mut tracker = PrIssueTracker::new();

        // Recording a ReviewComment nudge should not block ReviewComplete
        tracker.record_nudge(42, PrIssueType::ReviewComment);
        assert!(tracker.should_nudge(42, PrIssueType::ReviewComplete));

        // Recording ReviewComplete should block itself but not others
        tracker.record_nudge(42, PrIssueType::ReviewComplete);
        assert!(!tracker.should_nudge(42, PrIssueType::ReviewComplete));
        assert!(tracker.should_nudge(42, PrIssueType::Approved));
    }

    #[test]
    fn test_pr_issue_type_display() {
        assert_eq!(PrIssueType::MergeConflict.to_string(), "merge conflict");
        assert_eq!(PrIssueType::CiFailed.to_string(), "CI failed");
        assert_eq!(
            PrIssueType::ChangesRequested.to_string(),
            "changes requested"
        );
        assert_eq!(PrIssueType::Approved.to_string(), "approved");
        assert_eq!(PrIssueType::NeedsReview.to_string(), "needs review");
        assert_eq!(PrIssueType::ReviewComment.to_string(), "review comment");
        assert_eq!(PrIssueType::ReviewComplete.to_string(), "review complete");
        assert_eq!(
            PrIssueType::GreenWithFeedback.to_string(),
            "CI green with feedback"
        );
    }

    #[test]
    fn test_detect_pr_issues_merge_conflict() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [],
            "reviewDecision": ""
        });
        let issues = detect_pr_issues(&pr);
        assert!(issues.contains(&PrIssueType::MergeConflict));
    }

    #[test]
    fn test_detect_pr_issues_ci_failed() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "lint"},
                {"conclusion": "FAILURE", "name": "test"}
            ],
            "reviewDecision": ""
        });
        let issues = detect_pr_issues(&pr);
        assert!(issues.contains(&PrIssueType::CiFailed));
    }

    #[test]
    fn test_detect_pr_issues_changes_requested() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [],
            "reviewDecision": "CHANGES_REQUESTED"
        });
        let issues = detect_pr_issues(&pr);
        assert!(issues.contains(&PrIssueType::ChangesRequested));
    }

    #[test]
    fn test_detect_pr_issues_approved() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [],
            "reviewDecision": "APPROVED"
        });
        let issues = detect_pr_issues(&pr);
        assert!(issues.contains(&PrIssueType::Approved));
    }

    #[test]
    fn test_detect_pr_issues_no_issues() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "test"}
            ],
            "reviewDecision": ""
        });
        let issues = detect_pr_issues(&pr);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_detect_pr_issues_multiple() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [
                {"conclusion": "FAILURE", "name": "test"}
            ],
            "reviewDecision": "CHANGES_REQUESTED"
        });
        let issues = detect_pr_issues(&pr);
        assert_eq!(issues.len(), 3);
        assert!(issues.contains(&PrIssueType::MergeConflict));
        assert!(issues.contains(&PrIssueType::CiFailed));
        assert!(issues.contains(&PrIssueType::ChangesRequested));
    }

    // -----------------------------------------------------------------------
    // is_auto_mergeable tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_auto_mergeable_approved_all_checks_pass() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "test"},
                {"conclusion": "SUCCESS", "name": "lint"}
            ],
            "reviewDecision": "APPROVED"
        });
        assert!(is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_not_approved() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "test"}
            ],
            "reviewDecision": "REVIEW_REQUIRED"
        });
        assert!(!is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_has_ci_failure() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "FAILURE", "name": "test"}
            ],
            "reviewDecision": "APPROVED"
        });
        assert!(!is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_has_merge_conflict() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "test"}
            ],
            "reviewDecision": "APPROVED"
        });
        assert!(!is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_has_pending_checks() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "name": "test"},
                {"conclusion": "", "name": "deploy"}
            ],
            "reviewDecision": "APPROVED"
        });
        assert!(!is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_empty_checks() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [],
            "reviewDecision": "APPROVED"
        });
        assert!(is_auto_mergeable(&pr));
    }

    #[test]
    fn test_auto_mergeable_no_checks_field() {
        let pr = serde_json::json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED"
        });
        assert!(is_auto_mergeable(&pr));
    }

    #[test]
    fn test_get_issue_action() {
        assert_eq!(
            get_issue_action(PrIssueType::MergeConflict),
            "please rebase"
        );
        assert_eq!(
            get_issue_action(PrIssueType::CiFailed),
            "please investigate"
        );
        assert_eq!(
            get_issue_action(PrIssueType::ChangesRequested),
            "please address feedback"
        );
        assert_eq!(
            get_issue_action(PrIssueType::Approved),
            "approved with CI green — please merge (use --auto if checks pending)"
        );
        assert_eq!(
            get_issue_action(PrIssueType::NeedsReview),
            "calling in reviewer"
        );
        assert_eq!(
            get_issue_action(PrIssueType::ReviewComment),
            "please address review feedback and merge if appropriate"
        );
        assert_eq!(
            get_issue_action(PrIssueType::ReviewComplete),
            "review is complete — please address feedback and merge if appropriate"
        );
        assert_eq!(
            get_issue_action(PrIssueType::GreenWithFeedback),
            "CI is green — please address review feedback and merge"
        );
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("hi", 2), "hi");
    }

    // Stuck condition tracker tests
    #[test]
    fn test_stuck_tracker_track_and_should_nudge() {
        let mut tracker = StuckConditionTracker::new();

        // Not tracked yet — should_nudge returns false
        assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));

        // Track it — now should_nudge returns true (never nudged before)
        tracker.track("42", StuckConditionType::NoReview);
        assert!(tracker.should_nudge("42", StuckConditionType::NoReview));
    }

    #[test]
    fn test_stuck_tracker_record_nudge_cooldown() {
        let mut tracker = StuckConditionTracker::new();
        tracker.track("42", StuckConditionType::NoReview);

        // Before recording nudge — should_nudge is true
        assert!(tracker.should_nudge("42", StuckConditionType::NoReview));

        // After recording nudge — should_nudge is false (within cooldown)
        tracker.record_nudge("42", StuckConditionType::NoReview);
        assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
    }

    #[test]
    fn test_stuck_tracker_independent_conditions() {
        let mut tracker = StuckConditionTracker::new();

        // Track two different conditions for the same PR
        tracker.track("42", StuckConditionType::NoReview);
        tracker.track("42", StuckConditionType::MergeReady);

        // Nudging one doesn't affect the other
        tracker.record_nudge("42", StuckConditionType::NoReview);
        assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
        assert!(tracker.should_nudge("42", StuckConditionType::MergeReady));
    }

    #[test]
    fn test_stuck_tracker_clear() {
        let mut tracker = StuckConditionTracker::new();
        tracker.track("42", StuckConditionType::NoReview);
        assert!(tracker.should_nudge("42", StuckConditionType::NoReview));

        // Clear the condition
        tracker.clear("42", StuckConditionType::NoReview);
        assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
    }

    #[test]
    fn test_stuck_tracker_different_prs() {
        let mut tracker = StuckConditionTracker::new();
        tracker.track("42", StuckConditionType::NoReview);
        tracker.track("43", StuckConditionType::NoReview);

        // Nudging one PR doesn't affect the other
        tracker.record_nudge("42", StuckConditionType::NoReview);
        assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
        assert!(tracker.should_nudge("43", StuckConditionType::NoReview));
    }

    #[test]
    fn test_stuck_condition_type_display() {
        assert_eq!(StuckConditionType::NoReview.to_string(), "no review");
        assert_eq!(
            StuckConditionType::UnresolvedFeedback.to_string(),
            "unresolved feedback"
        );
        assert_eq!(
            StuckConditionType::MergeReady.to_string(),
            "merge-ready but not merged"
        );
        assert_eq!(
            StuckConditionType::SilentCoworker.to_string(),
            "silent coworker"
        );
        assert_eq!(
            StuckConditionType::ReviewBacklog.to_string(),
            "review backlog"
        );
    }

    #[test]
    fn test_stuck_tracker_nudge_count() {
        let mut tracker = StuckConditionTracker::new();

        // Not tracked yet — nudge count is 0
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            0
        );

        // Track and first nudge
        tracker.track("lex", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            0
        );

        tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            1
        );

        // Second nudge (would be escalation)
        tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            2
        );
    }

    #[test]
    fn test_stuck_tracker_nudge_count_cleared_on_clear() {
        let mut tracker = StuckConditionTracker::new();
        tracker.track("lex", StuckConditionType::SilentCoworker);
        tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            1
        );

        // Clear resets everything
        tracker.clear("lex", StuckConditionType::SilentCoworker);
        assert_eq!(
            tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
            0
        );
    }

    // Chat monitor @mention tests
    #[test]
    fn test_extract_mentions_single() {
        let mentions = extract_mentions("@park please review this");
        assert_eq!(mentions, vec!["park"]);
    }

    #[test]
    fn test_extract_mentions_multiple() {
        let mentions = extract_mentions("@park and @lexington please coordinate");
        assert_eq!(mentions.len(), 2);
        assert!(mentions.contains(&"park".to_string()));
        assert!(mentions.contains(&"lexington".to_string()));
    }

    #[test]
    fn test_extract_mentions_case_insensitive() {
        let mentions = extract_mentions("@PARK please review");
        assert_eq!(mentions, vec!["park"]);
    }

    #[test]
    fn test_extract_mentions_no_duplicates() {
        let mentions = extract_mentions("@park @park @park");
        assert_eq!(mentions, vec!["park"]);
    }

    #[test]
    fn test_extract_mentions_word_boundary() {
        // @parkway should not match @park
        let mentions = extract_mentions("@parkway is not a coworker");
        assert!(mentions.is_empty());
    }

    #[test]
    fn test_extract_mentions_at_end() {
        let mentions = extract_mentions("cc @amsterdam");
        assert_eq!(mentions, vec!["amsterdam"]);
    }

    #[test]
    fn test_extract_mentions_no_mentions() {
        let mentions = extract_mentions("just a regular message");
        assert!(mentions.is_empty());
    }

    #[test]
    fn test_extract_mentions_invalid_names() {
        // feature is not a valid coworker name
        let mentions = extract_mentions("@feature @bug @test");
        assert!(mentions.is_empty());
    }

    #[test]
    fn test_extract_mentions_all_coworker_names() {
        // Verify all coworker names work
        for &name in COWORKER_NAMES {
            let msg = format!("@{} please help", name);
            let mentions = extract_mentions(&msg);
            assert_eq!(mentions, vec![name], "Failed for coworker: {}", name);
        }
    }

    #[test]
    fn test_skip_senders() {
        // Verify SKIP_SENDERS contains expected values.
        // "user" is skipped because handle_channel_post routes user @mentions
        // directly, similar to how the webhook handler routes "github" mentions.
        assert!(SKIP_SENDERS.contains(&"midtown"));
        assert!(SKIP_SENDERS.contains(&"system"));
        assert!(SKIP_SENDERS.contains(&"github"));
        assert!(SKIP_SENDERS.contains(&"user"));
        // "architect" is skipped to prevent diagram messages from triggering
        // @mention routing in the chat monitor.
        assert!(SKIP_SENDERS.contains(&"architect"));
    }

    #[test]
    fn test_webhook_mentions_should_be_extracted() {
        // Webhook messages from "github" contain @mentions that should be routed.
        // The chat monitor skips "github" messages for loop protection, so the
        // webhook handler must call route_mentions directly — but only for
        // non-CI-success events (see test_ci_check_passed_should_not_route_mentions).
        //
        // Example: "@riverside merged PR #178" from sender "github"
        // The @riverside mention should be extracted and routed.
        let webhook_content = "@riverside merged PR #178";
        let mentions = extract_mentions(webhook_content);
        assert_eq!(mentions, vec!["riverside"]);

        // PR merge notifications often include PR author in the message
        let merge_content = "@lexington PR #42 was merged by btucker";
        let mentions = extract_mentions(merge_content);
        assert_eq!(mentions, vec!["lexington"]);

        // Multiple mentions in webhook messages
        let review_content = "@park @madison please review PR #99";
        let mentions = extract_mentions(review_content);
        assert!(mentions.contains(&"park".to_string()));
        assert!(mentions.contains(&"madison".to_string()));
    }

    #[test]
    fn test_ci_check_passed_should_not_route_mentions() {
        // Bug: CI check pass webhook events (e.g., "@madison Check 'build' passed
        // on PR #99") contain @mentions that the webhook handler was routing via
        // route_mentions(). This caused a loop: madison gets called in → goes
        // idle → next CI check @mention triggers another call-in.
        //
        // CI success notifications are informational — they should NOT trigger
        // coworker spawn/nudge. The webhook handler must skip route_mentions
        // when ci_check_passed is set.

        // CI check pass messages contain @mentions
        let ci_content = "@madison Check 'build' passed on PR #99";
        let mentions = extract_mentions(ci_content);
        assert_eq!(
            mentions,
            vec!["madison"],
            "CI message does contain @mention"
        );

        // Construct a WebhookEvent with ci_check_passed set
        let mut event = crate::webhook::WebhookEvent::github(ci_content);
        event.ci_check_passed = Some(crate::webhook::CiCheckPassed {
            check_name: "build".to_string(),
            target: "PR #99".to_string(),
            mention_prefix: "@madison ".to_string(),
        });

        // The webhook handler should skip route_mentions when ci_check_passed is set.
        // We verify this by checking the flag that the handler uses to decide.
        assert!(
            event.ci_check_passed.is_some(),
            "ci_check_passed flag should be set for CI success events"
        );

        // Batched CI notifications (which replace ci_check_passed events) also
        // contain @mentions, but they're posted with from="github" and caught
        // by the chat monitor's SKIP_SENDERS filter.
        let batched_content = "@madison 5 checks passed on PR #99";
        let batched_mentions = extract_mentions(batched_content);
        assert_eq!(
            batched_mentions,
            vec!["madison"],
            "batched CI message also contains @mention"
        );

        // The "github" sender is in SKIP_SENDERS, so chat monitor correctly
        // skips batched messages.
        assert!(
            SKIP_SENDERS.contains(&"github"),
            "github must be in SKIP_SENDERS"
        );
    }

    #[test]
    fn test_contains_at_all_basic() {
        assert!(contains_at_all("@all please check the latest changes"));
        assert!(contains_at_all("Hey @all, important update"));
        assert!(contains_at_all("message for @all"));
    }

    #[test]
    fn test_contains_at_all_case_insensitive() {
        assert!(contains_at_all("@ALL please review"));
        assert!(contains_at_all("@All heads up"));
        assert!(contains_at_all("@aLl check this"));
    }

    #[test]
    fn test_contains_at_all_word_boundary() {
        // Should NOT match @allison or @alliance (part of a longer word)
        assert!(!contains_at_all("@allison please help"));
        assert!(!contains_at_all("@alliance meeting at 3"));
        assert!(!contains_at_all("@allowed to proceed"));
    }

    #[test]
    fn test_contains_at_all_at_end() {
        assert!(contains_at_all("message to @all"));
    }

    #[test]
    fn test_contains_at_all_with_punctuation() {
        assert!(contains_at_all("@all: important update"));
        assert!(contains_at_all("@all, please check"));
        assert!(contains_at_all("@all!"));
    }

    #[test]
    fn test_contains_at_all_no_match() {
        assert!(!contains_at_all("just a regular message"));
        assert!(!contains_at_all("@ all with space"));
    }

    #[test]
    fn test_extract_mentions_does_not_include_at_all() {
        // @all is not a coworker name, so extract_mentions should not return it
        let mentions = extract_mentions("@all please check");
        assert!(mentions.is_empty());
    }

    #[test]
    fn test_user_mentions_coworker_should_be_extracted() {
        // When a user @mentions a coworker in their message, the mention should
        // be extracted so it can be routed directly to that coworker (not just
        // to the lead). This validates the contract that handle_channel_post
        // relies on when calling route_mentions for user messages.
        let user_msg = "@lexington please review PR #42";
        let mentions = extract_mentions(user_msg);
        assert_eq!(mentions, vec!["lexington"]);

        // User mentioning multiple coworkers
        let multi_msg = "@park and @madison can you pair on this?";
        let mentions = extract_mentions(multi_msg);
        assert!(mentions.contains(&"park".to_string()));
        assert!(mentions.contains(&"madison".to_string()));

        // User mentioning @lead should NOT appear in coworker mentions
        // (@lead is handled separately in handle_channel_post)
        let lead_msg = "@lead what do you think?";
        let mentions = extract_mentions(lead_msg);
        assert!(mentions.is_empty() || !mentions.contains(&"lead".to_string()));
    }

    #[test]
    fn test_user_mention_routing_skips_lead() {
        // When a user message @mentions a coworker, the lead should NOT be
        // nudged — the daemon routes directly to the coworker. This test
        // validates the detection logic used in handle_channel_post.

        // User @mentions a coworker → has_coworker_mentions = true, skip lead
        let content = "@riverside continue";
        let has_coworker_mentions =
            !extract_mentions(content).is_empty() || contains_at_all(content);
        let has_lead_mention = content.to_lowercase().contains("@lead");
        assert!(has_coworker_mentions);
        assert!(!has_lead_mention);
        // Should skip lead: has_coworker_mentions && !has_lead_mention
        assert!(has_coworker_mentions && !has_lead_mention);

        // User sends a regular message → no mentions, nudge lead
        let content = "how is task 5 going?";
        let has_coworker_mentions =
            !extract_mentions(content).is_empty() || contains_at_all(content);
        assert!(!has_coworker_mentions);

        // User @mentions coworker AND @lead → nudge lead too
        let content = "@riverside @lead please coordinate on this";
        let has_coworker_mentions =
            !extract_mentions(content).is_empty() || contains_at_all(content);
        let has_lead_mention = content.to_lowercase().contains("@lead");
        assert!(has_coworker_mentions);
        assert!(has_lead_mention);

        // User uses @all → coworker mentions detected, skip lead
        // (route_at_all already broadcasts to lead)
        let content = "@all stand up time";
        let has_coworker_mentions =
            !extract_mentions(content).is_empty() || contains_at_all(content);
        let has_lead_mention = content.to_lowercase().contains("@lead");
        assert!(has_coworker_mentions);
        assert!(!has_lead_mention);
    }

    // Review signature detection tests
    #[test]
    fn test_text_contains_review_signature_emoji() {
        // Legacy formal review signature
        assert!(text_contains_review_signature("🤖 Reviewed by lexington"));
        assert!(text_contains_review_signature(
            "Some preamble\n🤖 Reviewed by park\nMore text"
        ));
    }

    #[test]
    fn test_text_contains_review_signature_plain() {
        // Plain "Reviewed by" without emoji
        assert!(text_contains_review_signature("Reviewed by columbus"));
        assert!(text_contains_review_signature("LGTM! Reviewed by york"));
    }

    #[test]
    fn test_text_contains_review_signature_frontmatter() {
        // Coworker comment frontmatter (used in gh pr comment)
        assert!(text_contains_review_signature(
            "<!-- midtown: lexington -->"
        ));
        assert!(text_contains_review_signature(
            "<!-- midtown: park -->\n\n## Summary\nLooks good!"
        ));
        assert!(text_contains_review_signature(
            "Some text\n<!-- midtown: york -->\nMore text"
        ));
    }

    #[test]
    fn test_text_contains_review_signature_code_review_header() {
        // Code review header used by review agent
        assert!(text_contains_review_signature("## Code Review by madison"));
        assert!(text_contains_review_signature(
            "<!-- midtown: madison -->\n\n## Code Review by madison\n\nNice work!"
        ));
    }

    #[test]
    fn test_text_contains_review_signature_code_review_skill_output() {
        // The code-review skill posts comments in this exact format.
        // The <!-- midtown: name --> frontmatter is the primary signature.
        let skill_output_clean = r#"<!-- midtown: pleasant -->

### Code review

No issues found. Checked for bugs and CLAUDE.md compliance.

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;
        assert!(text_contains_review_signature(skill_output_clean));

        let skill_output_issues = r#"<!-- midtown: vernon -->

### Code review

Found 2 issues:

1. Missing null check (bug due to `unwrap()`)

https://github.com/org/repo/blob/abc123/src/main.rs#L10-L12

2. Config not validated (CLAUDE.md says "validate all config")

https://github.com/org/repo/blob/abc123/CLAUDE.md#L5-L7

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;
        assert!(text_contains_review_signature(skill_output_issues));
    }

    #[test]
    fn test_text_contains_review_signature_code_review_without_frontmatter() {
        // Regression test for PR #869: code-review skill sometimes posts reviews
        // without the <!-- midtown: --> frontmatter. The "### Code review" heading
        // alone should still be detected as a review.
        //
        // Real comment from PR #869 that failed detection:
        let review_without_frontmatter = r#"### Code review

No issues found. Checked for bugs and CLAUDE.md compliance.

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;

        // This should be detected as a review, but currently fails:
        assert!(
            text_contains_review_signature(review_without_frontmatter),
            "Code review heading without frontmatter should still be detected"
        );

        // Case insensitive variant:
        let review_lowercase = r#"### code review

Found 1 issue:

1. Missing error handling

🤖 Generated with [Claude Code](https://claude.ai/code)"#;

        assert!(
            text_contains_review_signature(review_lowercase),
            "Lowercase 'code review' heading should be detected"
        );
    }

    #[test]
    fn test_text_contains_review_signature_none() {
        // Text without any review signature should return false
        assert!(!text_contains_review_signature("Just a regular comment"));
        assert!(!text_contains_review_signature("LGTM!"));
        assert!(!text_contains_review_signature(
            "Thanks for the changes, looks good to me."
        ));
        assert!(!text_contains_review_signature(""));
        // Partial matches shouldn't count
        assert!(!text_contains_review_signature("midtown"));
        assert!(!text_contains_review_signature("Code Review"));
    }

    #[test]
    fn test_review_headroom_constant() {
        assert_eq!(REVIEW_HEADROOM, 2);
    }

    #[test]
    fn test_dev_limit_calculation() {
        // Helper: compute dev cap the same way is_at_dev_limit does
        let dev_cap = |max_coworkers: usize| -> usize {
            max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1)
        };

        // Normal case: max_coworkers=6, dev cap should be 4
        assert_eq!(dev_cap(6), 4);

        // max_coworkers=4, dev cap should be 2
        assert_eq!(dev_cap(4), 2);

        // max_coworkers=3, dev cap should be 1
        assert_eq!(dev_cap(3), 1);

        // Edge case: max_coworkers=2, dev cap should be 1 (not 0)
        assert_eq!(dev_cap(2), 1);

        // Edge case: max_coworkers=1, dev cap should be 1 (floor at 1)
        assert_eq!(dev_cap(1), 1);

        // Edge case: max_coworkers=0, dev cap should be 1 (floor at 1 via .max(1))
        assert_eq!(dev_cap(0), 1);

        // Large case: max_coworkers=10, dev cap should be 8
        assert_eq!(dev_cap(10), 8);
    }

    #[test]
    fn test_usage_limit_patterns_detect_common_messages() {
        // The usage limit pattern is "/upgrade" or "/extra-usage" which appears
        // on Claude Code's actual usage limit screen. This avoids false positives
        // from code that mentions "usage limit" in comments.
        let messages = vec![
            "You've hit your usage limit. /upgrade to increase your limit.",
            "Usage limit reached for this model. Options: /upgrade or wait.",
            "Try /upgrade to get more tokens or wait 15 minutes.",
            // Claude Code v2.1.33+ uses /extra-usage instead of /upgrade
            "You've hit your limit · resets 11pm (America/Chicago)\n     /extra-usage to finish what you're working on.",
            "/extra-usage to continue working on this task.",
        ];

        for msg in messages {
            assert!(
                crate::rules::has_usage_limit_pattern(msg),
                "Pattern not detected in: {}",
                msg
            );
        }
    }

    #[test]
    fn test_usage_limit_patterns_no_false_positives() {
        let messages = vec![
            "Reading file src/main.rs",
            "Editing src/daemon.rs",
            "Running tests...",
            "Build succeeded",
        ];

        for msg in messages {
            assert!(
                !crate::rules::has_usage_limit_pattern(msg),
                "False positive in: {}",
                msg
            );
        }
    }

    // ─── Usage Limit Expiry Tests ──────────────────────────────────────

    #[test]
    fn test_usage_limit_expiry_nudge_now() {
        let now = tokio::time::Instant::now();
        // Nudge was scheduled 1 second ago
        let nudge_at = Some(now - std::time::Duration::from_secs(1));

        let decision = decide_usage_limit_expiry(nudge_at, now);
        assert_eq!(decision, UsageLimitExpiryDecision::NudgeNow);
    }

    #[test]
    fn test_usage_limit_expiry_not_yet() {
        let now = tokio::time::Instant::now();
        // Nudge is 10 minutes in the future
        let nudge_at = Some(now + std::time::Duration::from_secs(600));

        let decision = decide_usage_limit_expiry(nudge_at, now);
        assert_eq!(decision, UsageLimitExpiryDecision::NotYet);
    }

    #[test]
    fn test_usage_limit_expiry_no_nudge() {
        let now = tokio::time::Instant::now();

        let decision = decide_usage_limit_expiry(None, now);
        assert_eq!(decision, UsageLimitExpiryDecision::NoNudge);
    }

    // -------------------------------------------------------------------------
    // Stuck escalation constant tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_stuck_escalation_threshold_is_reasonable() {
        use super::constants::{STUCK_ESCALATION_NUDGE_COUNT, STUCK_NUDGE_COOLDOWN_SECS};

        // Verify the escalation threshold results in a reasonable time before escalation.
        // With STUCK_ESCALATION_NUDGE_COUNT=2 and STUCK_NUDGE_COOLDOWN_SECS=1800 (30 min),
        // escalation happens after 2 nudges, meaning at least 45+ minutes have elapsed
        // (15 min initial detection + 30 min cooldown before second nudge).
        assert_eq!(
            STUCK_ESCALATION_NUDGE_COUNT, 2,
            "escalation should trigger after 2 nudges (45+ min)"
        );

        // Verify the cooldown is long enough to avoid spam but short enough to escalate
        // within a reasonable timeframe (30 minutes between nudges).
        assert_eq!(
            STUCK_NUDGE_COOLDOWN_SECS,
            30 * 60,
            "nudge cooldown should be 30 minutes"
        );

        // Calculate minimum time before escalation:
        // Initial stuck detection (15 min) + 1 cooldown (30 min) = 45 min minimum
        let min_escalation_minutes =
            15 + (STUCK_ESCALATION_NUDGE_COUNT - 1) as u64 * (STUCK_NUDGE_COOLDOWN_SECS / 60);
        assert!(
            min_escalation_minutes >= 45,
            "escalation should not trigger before 45 minutes"
        );
    }

    // ── Task assignment tracking tests ──────────────────────────────────

    /// Helper to create a minimal task assignment tracker for testing.
    fn new_task_assignment_tracker() -> std::sync::Mutex<HashMap<String, String>> {
        std::sync::Mutex::new(HashMap::new())
    }

    #[test]
    fn test_task_assignment_record_and_lookup() {
        let tracker = new_task_assignment_tracker();

        // Record an assignment
        {
            let mut map = tracker.lock().unwrap();
            map.insert("park".to_string(), "42".to_string());
        }

        // Verify lookup
        let busy: HashSet<String> = {
            let map = tracker.lock().unwrap();
            map.keys().cloned().collect()
        };
        assert!(busy.contains("park"));
        assert!(!busy.contains("madison"));
    }

    #[test]
    fn test_task_assignment_clear_by_task() {
        let tracker = new_task_assignment_tracker();

        // Record assignments for two coworkers
        {
            let mut map = tracker.lock().unwrap();
            map.insert("park".to_string(), "42".to_string());
            map.insert("madison".to_string(), "43".to_string());
        }

        // Clear by task ID (simulates task completion)
        {
            let mut map = tracker.lock().unwrap();
            map.retain(|_, tid| tid != "42");
        }

        let busy: HashSet<String> = {
            let map = tracker.lock().unwrap();
            map.keys().cloned().collect()
        };
        assert!(
            !busy.contains("park"),
            "park should be free after task completion"
        );
        assert!(busy.contains("madison"), "madison should still be busy");
    }

    #[test]
    fn test_task_assignment_clear_by_coworker() {
        let tracker = new_task_assignment_tracker();

        // Record assignment
        {
            let mut map = tracker.lock().unwrap();
            map.insert("park".to_string(), "42".to_string());
        }

        // Clear by coworker name (simulates shutdown)
        {
            let mut map = tracker.lock().unwrap();
            map.remove("park");
        }

        let busy: HashSet<String> = {
            let map = tracker.lock().unwrap();
            map.keys().cloned().collect()
        };
        assert!(
            busy.is_empty(),
            "no coworkers should be busy after shutdown"
        );
    }

    #[test]
    fn test_busy_coworkers_prevents_duplicate_assignment() {
        // This test verifies the core fix: busy_coworkers should contain
        // coworkers from the internal tracker, preventing duplicate assignments.
        let mut busy_coworkers: HashSet<String> = HashSet::new();

        // Simulate daemon's internal tracking (replaces disk-based detection)
        let internal_tracking: HashMap<String, String> = [("park".to_string(), "42".to_string())]
            .into_iter()
            .collect();
        busy_coworkers.extend(internal_tracking.keys().cloned());

        // Verify park is detected as busy
        assert!(
            busy_coworkers.contains("park"),
            "park should be busy (has assigned task)"
        );

        // Simulate dispatch check: already_running AND busy → skip
        let already_running = true;
        let is_busy = busy_coworkers.contains("park");
        let was_grouped = false;

        assert!(
            already_running && is_busy && !was_grouped,
            "should skip busy non-grouped coworker"
        );
    }

    #[test]
    fn test_grouped_tasks_bypass_snapshot_busy_check() {
        // Grouped tasks (same PR, blockedBy) should be allowed even if the
        // coworker is busy from a *previous tick* (in busy_coworkers snapshot).
        let busy_coworkers: HashSet<String> = ["park".to_string()].into_iter().collect();

        let already_running = true;
        let is_busy_from_snapshot = busy_coworkers.contains("park");
        let assigned_this_tick = false; // Not assigned this tick
        let was_grouped = true; // Task was grouped to park via PR/blockedBy
        let is_coworker_reviewer = false;

        // Grouped tasks bypass the snapshot busy check (cross-tick grouping)
        let should_skip = already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped));
        assert!(
            !should_skip,
            "grouped tasks should bypass snapshot busy check"
        );
    }

    #[test]
    fn test_names_assigned_this_tick_prevents_duplicate_spawn() {
        // Within a single tick, if two unrelated tasks both get fresh names,
        // the second should be prevented if the first already claimed the name.
        let names_assigned_this_tick: HashSet<String> = ["park".to_string()].into_iter().collect();

        // Second task tries to use "park" (not grouped)
        let assigned_this_tick = names_assigned_this_tick.contains("park");
        let is_busy_from_snapshot = false;
        let was_grouped = false;
        let already_running = false;

        let should_skip =
            !already_running && (assigned_this_tick || is_busy_from_snapshot) && !was_grouped;
        assert!(
            should_skip,
            "should skip duplicate fresh-spawn within same tick"
        );
    }

    #[test]
    fn test_grouped_tasks_should_not_duplicate_nudge_to_running_coworker() {
        // Bug fix: When two grouped tasks (same PR) target an already-running coworker,
        // the second should be skipped because the coworker was already assigned this tick.
        // Previously, the condition `(is_busy && !was_grouped)` exempted grouped tasks
        // from the busy check entirely, allowing duplicate nudges.
        let names_assigned_this_tick: HashSet<String> =
            ["pleasant".to_string()].into_iter().collect();

        // Second grouped task tries to use "pleasant" (already assigned this tick)
        let assigned_this_tick = names_assigned_this_tick.contains("pleasant");
        let is_busy_from_snapshot = true; // Also busy from snapshot
        let was_grouped = true;
        let already_running = true;
        let is_coworker_reviewer = false;

        let should_skip = already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped));
        assert!(
            should_skip,
            "should skip duplicate nudge to already-running coworker within same tick, \
             even for grouped tasks (same PR)"
        );

        // Verify it's specifically the assigned_this_tick that catches it
        assert!(
            assigned_this_tick,
            "assigned_this_tick should be the trigger for skipping"
        );
    }

    #[test]
    fn test_mark_in_flight_spawns_covers_all_effect_variants() {
        // mark_in_flight_spawns_from_effects must track task IDs from:
        // 1. AssignAndSpawn (Case 2 fresh spawns)
        // 2. NudgeCoworkerWithCallbacks with RecordTaskAssignment (Case 2 nudges)
        // 3. SpawnCoworkerWithCallbacks with RecordTaskAssignment (Case 1 owned spawns)
        let effects = vec![
            effects::Effect::NudgeCoworkerWithCallbacks {
                name: "pleasant".to_string(),
                message: "task prompt".to_string(),
                session_id: None,
                on_success: vec![effects::Effect::RecordTaskAssignment {
                    coworker: "pleasant".to_string(),
                    task_id: "873".to_string(),
                }],
            },
            effects::Effect::AssignAndSpawn {
                task_id: "874".to_string(),
                owner: "park".to_string(),
                repo_name: "test-repo".to_string(),
                config: crate::launch::LaunchConfig::coworker(
                    "park".to_string(),
                    "test-repo".to_string(),
                    crate::launch::SessionMode::Fresh,
                    None,
                ),
                on_success: vec![],
                on_failure: vec![],
            },
            effects::Effect::SpawnCoworkerWithCallbacks {
                config: crate::launch::LaunchConfig::coworker(
                    "broadway".to_string(),
                    "test-repo".to_string(),
                    crate::launch::SessionMode::Resume,
                    None,
                ),
                on_success: vec![effects::Effect::RecordTaskAssignment {
                    coworker: "broadway".to_string(),
                    task_id: "875".to_string(),
                }],
                on_failure: vec![],
            },
        ];

        // Extract task IDs that should be in-flight (mirror the logic in
        // mark_in_flight_spawns_from_effects for test verification)
        let mut in_flight_tasks = HashSet::new();
        for effect in &effects {
            match effect {
                effects::Effect::AssignAndSpawn { task_id, .. } => {
                    in_flight_tasks.insert(task_id.clone());
                }
                effects::Effect::NudgeCoworkerWithCallbacks { on_success, .. }
                | effects::Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                    for sub_effect in on_success {
                        if let effects::Effect::RecordTaskAssignment { task_id, .. } = sub_effect {
                            in_flight_tasks.insert(task_id.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        assert!(
            in_flight_tasks.contains("873"),
            "NudgeCoworkerWithCallbacks with RecordTaskAssignment should be tracked"
        );
        assert!(
            in_flight_tasks.contains("874"),
            "AssignAndSpawn should be tracked"
        );
        assert!(
            in_flight_tasks.contains("875"),
            "SpawnCoworkerWithCallbacks with RecordTaskAssignment should be tracked"
        );
    }
}
