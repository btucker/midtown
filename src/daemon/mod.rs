//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod constants;
pub(crate) mod effects;
pub(crate) mod events;
mod helpers;
pub(crate) mod snapshot;
mod trackers;

use constants::*;
pub use constants::{
    DEFAULT_MAX_COWORKERS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, MAX_CONCURRENT_REVIEWS, PR_NUDGE_COOLDOWN_SECS,
    PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, PR_REVIEW_DELAY_SECS,
};
use effects::Effect;
use helpers::*;
pub use trackers::{
    OrphanTracker, PrIssueTracker, PrIssueType, StuckConditionTracker, StuckConditionType,
};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Mutex, RwLock, broadcast, watch};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::config;
use crate::coworker::CoworkerManager;
use crate::daemon_messages;
use crate::message::{Message, MessageType};
use crate::rpc::{Request, RequestId, Response, RpcError};
use crate::web::{self, WebUpdate};
use crate::webhook::{WebhookConfig, start_webhook_server};
use crate::worktree::WorktreeManager;

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

/// Ensure required Claude Code plugins are installed.
///
/// Reads the required plugins list from config, checks which are already
/// installed, and installs any missing ones. Failures are logged as warnings
/// but don't block daemon startup.
async fn ensure_plugins_installed() {
    let required = crate::config::get_required_plugins();
    if required.is_empty() {
        debug!("No required plugins configured");
        return;
    }

    info!("Checking {} required plugins", required.len());

    // Get list of installed plugins
    let installed = match get_installed_plugins().await {
        Ok(plugins) => plugins,
        Err(e) => {
            warn!("Failed to check installed plugins: {}", e);
            return;
        }
    };

    // Find missing plugins
    let missing: Vec<_> = required
        .iter()
        .filter(|p| !installed.contains(*p))
        .collect();

    if missing.is_empty() {
        info!("All required plugins are installed");
        return;
    }

    info!("Installing {} missing plugins", missing.len());

    // Install missing plugins
    for plugin in missing {
        match install_plugin(plugin).await {
            Ok(()) => info!("Installed plugin: {}", plugin),
            Err(e) => warn!("Failed to install plugin {}: {}", plugin, e),
        }
    }
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

/// Install a plugin by name.
async fn install_plugin(name: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("claude")
        .args(["plugin", "add", name])
        .output()
        .await
        .map_err(|e| format!("Failed to run claude plugin add: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.to_string());
    }

    Ok(())
}

/// Shared daemon state.
pub(crate) struct DaemonState {
    coworkers: CoworkerManager,
    channel: Channel,
    socket_path: PathBuf,
    /// Consolidated per-coworker lifecycle state (phase + last activity).
    /// Bundles what was previously `coworker_phases` and `last_coworker_activity`
    /// into a single map. Entries are created on spawn and cleared on shutdown.
    coworker_lifecycles: RwLock<HashMap<String, crate::rules::CoworkerLifecycle>>,
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
    /// Persistent GitHub state (PR reviewer assignments, etc.)
    github_state: Mutex<crate::github_state::GitHubState>,
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
    /// Persistent reminder state (one-shot condition-based notifications)
    reminder_state: std::sync::Mutex<crate::reminders::ReminderState>,
    /// Hash of the last PR poll response body, used to skip re-processing when data hasn't changed.
    /// This doesn't reduce API calls, but avoids redundant lock acquisition and issue detection
    /// when the PR state hasn't changed between poll cycles.
    last_pr_poll_hash: Mutex<u64>,
    /// In-memory cache of PR numbers with confirmed Claude reviews.
    /// Mirrors `GitHubState.reviewed_prs` for fast lookup without locking github_state.
    /// Review status is monotonic — once cached, never removed (except for closed PRs).
    reviewed_prs_cache: std::sync::RwLock<HashSet<u64>>,
    /// Cached result from the latest `poll_prs_for_issues` call.
    /// Contains `headRefName` from each open PR, allowing `get_coworkers_with_open_prs`
    /// to reuse poll data instead of making a separate `gh pr list` call.
    cached_open_pr_branches: std::sync::RwLock<Vec<String>>,
    /// Cached coworker names from recently merged PRs.
    cached_merged_pr_coworkers: std::sync::RwLock<HashSet<String>>,
    /// Tracks stuck conditions that warrant nudging the lead (no review, unresolved feedback, etc.)
    stuck_tracker: Mutex<StuckConditionTracker>,
    /// Per-coworker pane content hash and last-changed timestamp (for stuck detection).
    /// Maps coworker name → (last_hash, last_changed_at).
    coworker_pane_hashes: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Cached GitHub repo full names (owner/repo) by repo path.
    /// Repo names never change during a daemon session, so we cache indefinitely.
    repo_name_cache: std::sync::RwLock<HashMap<PathBuf, String>>,
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

    #[allow(clippy::too_many_arguments)]
    fn new(
        socket_path: PathBuf,
        coworkers: CoworkerManager,
        repo_name: String,
        all_repo_paths: Vec<PathBuf>,
        channel: Channel,
        web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
        max_coworkers: usize,
        push_manager: Option<std::sync::Arc<crate::push::PushManager>>,
        default_branch: String,
    ) -> crate::Result<Self> {
        // Load persistent GitHub state
        let github_state =
            crate::github_state::load_state_for_repo(&repo_name).unwrap_or_else(|e| {
                warn!("Failed to load github-state.json: {}, using defaults", e);
                crate::github_state::GitHubState::default()
            });

        // Seed the in-memory review cache from persistent state
        let reviewed_prs_cache = github_state.reviewed_prs.clone();

        // Load persistent reminder state
        let reminder_path = crate::paths::reminders_file_for_repo(&repo_name);
        let reminder_state =
            crate::reminders::ReminderState::load(&reminder_path).unwrap_or_else(|e| {
                warn!("Failed to load reminders.json: {}, using defaults", e);
                crate::reminders::ReminderState::default()
            });

        Ok(Self {
            coworkers,
            channel,
            socket_path,
            coworker_lifecycles: RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::new()),
            repo_name,
            default_branch,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            orphan_tracker: std::sync::RwLock::new(OrphanTracker::new()),
            github_state: Mutex::new(github_state),
            web_updates_tx,
            max_coworkers,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            lead_typing: std::sync::Mutex::new(trackers::LeadTypingState::default()),
            reminder_state: std::sync::Mutex::new(reminder_state),
            last_pr_poll_hash: Mutex::new(0),
            reviewed_prs_cache: std::sync::RwLock::new(reviewed_prs_cache),
            cached_open_pr_branches: std::sync::RwLock::new(Vec::new()),
            cached_merged_pr_coworkers: std::sync::RwLock::new(HashSet::new()),
            stuck_tracker: Mutex::new(StuckConditionTracker::new()),
            coworker_pane_hashes: std::sync::Mutex::new(HashMap::new()),
            repo_name_cache: std::sync::RwLock::new(HashMap::new()),
        })
    }

    /// Spawn a coworker and initialize its lifecycle state.
    ///
    /// Wraps `CoworkerManager::spawn_with_name` and inserts a fresh
    /// `CoworkerLifecycle` entry on success, ensuring stale timestamps
    /// from any previous incarnation are replaced.
    async fn spawn_coworker(
        &self,
        name: &str,
        unique: bool,
        prompt: Option<&str>,
        isolated: bool,
    ) -> crate::Result<()> {
        self.coworkers
            .spawn_with_name(name, unique, prompt, isolated)?;
        let mut lc = self.coworker_lifecycles.write().await;
        lc.insert(
            name.to_string(),
            crate::rules::CoworkerLifecycle::new_spawn(),
        );
        Ok(())
    }

    /// Send a message to the channel and broadcast it to WebSocket clients.
    fn send_and_broadcast(&self, message: &Message) -> crate::Result<()> {
        self.channel.send(message)?;
        if let Some(ref tx) = self.web_updates_tx {
            web::broadcast_channel_message(tx, message);
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

    /// Broadcast a coworker status change to WebSocket clients.
    fn broadcast_coworker_update(&self, name: &str, status: &str, current_task: Option<&str>) {
        if let Some(ref tx) = self.web_updates_tx {
            web::broadcast_coworker_status(tx, name, status, current_task);
        }
    }
}

/// Acquire an exclusive lock on the PID file.
///
/// This enforces singleton behavior - only one daemon can run per repository.
/// Load additional WorktreeManagers for multi-repo projects.
///
/// Reads the project config to find additional repos (beyond the primary/workdir)
/// and creates a WorktreeManager for each. Failures are logged but don't prevent
/// the daemon from starting - the primary repo always works.
fn load_additional_worktree_managers(
    project_name: &str,
    config: &DaemonConfig,
) -> Vec<WorktreeManager> {
    let full_config = match crate::config::load_full_project_config(project_name) {
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

    // Initialize logging
    let filter = if config.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Ensure required plugins are installed (non-blocking, logs warnings on failure)
    ensure_plugins_installed().await;

    // Switch gh CLI to the configured GitHub user (fail loudly if switch fails)
    if let Some(ref github_user) = config.github_user {
        info!("Switching gh CLI auth to user: {}", github_user);
        let status = std::process::Command::new("gh")
            .args(["auth", "switch", "--user", github_user])
            .status()
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Failed to run `gh auth switch --user {}`: {}",
                    github_user, e
                ),
            })?;
        if !status.success() {
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "gh auth switch --user {} failed (exit code: {}). Is the user logged in? Run `gh auth login` first.",
                    github_user,
                    status.code().unwrap_or(-1)
                ),
            });
        }
        info!("Successfully switched gh auth to user: {}", github_user);
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

    // Derive project name: explicit --project flag > config.toml [project].name > repo name.
    // This must match what the CLI uses (resolve_project_name) so the Lead session
    // and daemon agree on the task list ID and tmux session name.
    let project_name = config
        .project_name
        .clone()
        .or_else(|| {
            crate::config::load_full_project_config(&repo_name)
                .and_then(|c| c.project.name().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| repo_name.clone());

    // Create channel for the repo
    let channel = Channel::for_repo(&repo_name)?;
    info!("Channel: {}", channel.base_dir().display());

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

    // Capture lead-specific values for health monitoring in the main loop.
    // These are cloned here because session_name is moved into CoworkerManager.
    let lead_session_name = session_name.clone();
    let lead_workdir = config.workdir.clone();
    let lead_project_name = project_name.clone();
    let lead_additional_dirs: Vec<PathBuf> = {
        if let Some(full_config) = crate::config::load_full_project_config(&project_name) {
            full_config
                .project
                .repos()
                .into_iter()
                .map(PathBuf::from)
                .filter(|p| *p != config.workdir)
                .collect()
        } else {
            Vec::new()
        }
    };

    let worktree_manager =
        WorktreeManager::new(config.workdir.clone()).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to initialize worktree manager: {}", e),
        })?;

    // For multi-repo projects, create worktree managers for additional repos
    let additional_worktree_managers = load_additional_worktree_managers(&project_name, &config);
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

    // Build list of all repo paths for multi-repo PR fetching
    // (needed by both the web server and daemon state)
    let all_repo_paths = {
        let mut paths = vec![config.workdir.clone()];
        if let Some(full_config) = crate::config::load_full_project_config(&project_name) {
            for repo in full_config.project.repos() {
                let path = PathBuf::from(repo);
                if path != config.workdir {
                    paths.push(path);
                }
            }
        }
        paths
    };

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
                tokio::spawn(webhook_forwarder_watchdog(
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
        channel,
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

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    // Set up idle check interval
    let mut idle_check_interval = interval(IDLE_CHECK_INTERVAL);

    // Set up lead typing indicator check interval
    let mut lead_typing_interval = interval(LEAD_TYPING_CHECK_INTERVAL);

    // Set up lead health check interval (recreates lead window if killed)
    let mut lead_health_interval = interval(LEAD_HEALTH_CHECK_INTERVAL);

    // Start PR polling background task
    let (pr_poll_shutdown_tx, pr_poll_shutdown_rx) = watch::channel(false);
    {
        let state = Arc::clone(&state);
        let interval_secs = config.pr_poll_interval_secs;
        tokio::spawn(async move {
            pr_poll_task(state, interval_secs, pr_poll_shutdown_rx).await;
        });
        info!(
            "PR polling started (interval: {}s)",
            config.pr_poll_interval_secs
        );
    }

    // Start chat monitor background task if enabled
    let (chat_monitor_shutdown_tx, chat_monitor_shutdown_rx) = watch::channel(false);
    if config.chat_monitor_enabled {
        let state = Arc::clone(&state);
        let channel_path = state.channel.channel_file_path().to_path_buf();
        tokio::spawn(async move {
            chat_monitor_loop(state, channel_path, chat_monitor_shutdown_rx).await;
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

    // Timer for periodic channel rotation
    let mut channel_rotation_interval = interval(CHANNEL_ROTATION_CHECK_INTERVAL);
    // Skip the first tick (which fires immediately)
    channel_rotation_interval.tick().await;

    // Nudge any coworkers discovered from tmux to continue their tasks.
    // This runs once at startup after the daemon has fully initialized.
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            nudge_discovered_coworkers(&state).await;
        });
    }

    // Main accept loop
    loop {
        let shutdown_rx = shutdown_tx.subscribe();
        let state = Arc::clone(&state);

        tokio::select! {
            // Accept new connections
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        debug!("New connection");
                        tokio::spawn(handle_connection(stream, shutdown_rx, state));
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }

            // Forward webhook messages to channel and nudge PR owners on comments
            Some(webhook_event) = async {
                match webhook_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!("Received webhook message: {}", webhook_event.message.content);
                if let Err(e) = state.send_and_broadcast(&webhook_event.message) {
                    error!("Failed to forward webhook message to channel: {}", e);
                }

                // Nudge PR owner when someone else comments on their PR
                if let Some(activity) = webhook_event.pr_activity {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        handle_pr_comment_nudge(&state, activity).await;
                    });
                }

                // Schedule immediate (after delay) reviewer spawn for new PRs
                if let Some(pr_number) = webhook_event.needs_review {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        handle_webhook_review_spawn(&state, pr_number).await;
                    });
                }

                // Nudge lead to pull main when a PR merges
                if let Some(pr_number) = webhook_event.merged_pr {
                    let nudge_msg = Message::text(
                        "system",
                        format!(
                            "@lead PR #{} merged into {}. Run `git pull` to stay current.",
                            pr_number, state.default_branch
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&nudge_msg) {
                        warn!("Failed to post merge nudge for PR #{}: {}", pr_number, e);
                    }
                    route_mentions(&state, &nudge_msg).await;
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

                // Cache review status immediately from webhook data (avoids API calls)
                if let Some(pr_number) = webhook_event.reviewed_pr {
                    debug!(
                        "Webhook: caching review status for PR #{} (review comment detected)",
                        pr_number
                    );
                    let mut cache = state.reviewed_prs_cache.write().unwrap();
                    cache.insert(pr_number);
                }

                // Route @mentions in webhook messages directly (chat monitor skips
                // "github" sender for loop protection, so we handle it here)
                route_mentions(&state, &webhook_event.message).await;
            }

            // Process user channel posts through the daemon (handles nudge, etc.)
            Some(mobile_post) = async {
                match mobile_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let content = &mobile_post.content;
                handle_channel_post(
                    RequestId::Null,
                    "user",
                    content,
                    &state,
                ).await;
            }

            // Periodically check for idle coworkers and shut them down
            _ = idle_check_interval.tick() => {
                // Sync internal state with actual tmux windows first
                if let Err(e) = state.coworkers.sync_with_tmux() {
                    warn!("Failed to sync coworker state with tmux: {}", e);
                }
                // event → snapshot → evaluate → execute
                let snap = snapshot::collect_world_snapshot(&state).await;
                let tick_effects = events::evaluate_tick(
                    &events::DaemonEvent::IdleCheckTick,
                    &snap,
                    &state,
                ).await;
                effects::execute_effects(tick_effects, &state).await;

            }

            // Check lead pane activity for typing indicator
            _ = lead_typing_interval.tick() => {
                check_lead_typing(&state).await;
            }

            // Check if lead window is still alive; recreate if killed
            _ = lead_health_interval.tick() => {
                let session = lead_session_name.clone();
                let workdir = lead_workdir.clone();
                let project = lead_project_name.clone();
                let additional = lead_additional_dirs.clone();
                tokio::task::spawn_blocking(move || {
                    check_and_respawn_lead(&session, &workdir, &project, &additional);
                }).await.ok();
            }

            // Periodic orphan check, duplicate detection, and worktree cleanup
            _ = orphan_check_interval.tick() => {
                // event → snapshot → evaluate → execute
                let snap = snapshot::collect_world_snapshot(&state).await;
                let tick_effects = events::evaluate_tick(
                    &events::DaemonEvent::OrphanCheckTick,
                    &snap,
                    &state,
                ).await;
                effects::execute_effects(tick_effects, &state).await;
                // cleanup_orphaned_worktrees is not yet effect-based
                cleanup_orphaned_worktrees(&state);
            }

            // Periodic channel log rotation
            _ = channel_rotation_interval.tick() => {
                if state.channel.needs_rotation(CHANNEL_ROTATION_MAX_AGE_HOURS) {
                    info!("Channel rotation triggered (oldest message > {}h)", CHANNEL_ROTATION_MAX_AGE_HOURS);
                    match state.channel.rotate(CHANNEL_ROTATION_RETAIN_MINUTES) {
                        Ok(archived) => {
                            info!("Channel rotated: {} messages archived", archived);
                            let msg = Message::system(
                                format!("Channel log rotated: {} old messages archived", archived)
                            );
                            if let Err(e) = state.send_and_broadcast(&msg) {
                                warn!("Failed to send rotation notification: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Channel rotation failed: {}", e);
                        }
                    }
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
        }
    }

    // Signal webhook forwarder watchdog to stop
    info!("Stopping webhook forwarder watchdog...");
    let _ = forwarder_shutdown_tx.send(true);

    // Signal PR poll task to stop
    info!("Stopping PR poll task...");
    let _ = pr_poll_shutdown_tx.send(true);

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

/// Check if the lead's tmux pane has changed and broadcast typing status.
///
/// Captures the lead's Claude Code pane (`lead.0`), hashes the content, and
/// compares against the previous hash. If content changed, the lead is working.
/// Uses a grace period so brief pauses (reading, thinking) don't prematurely
/// clear the indicator. Only broadcasts when the working state transitions.
async fn check_lead_typing(state: &DaemonState) {
    let tx = match state.web_updates_tx {
        Some(ref tx) => tx,
        None => return,
    };

    let session = format!("{}{}", crate::tmux::SESSION_PREFIX, state.repo_name);
    let target = format!("{}:lead.0", session);

    let content =
        match tokio::task::spawn_blocking(move || crate::tmux::capture_pane(&target)).await {
            Ok(Some(text)) => text,
            _ => return,
        };

    // Hash the pane content for cheap comparison
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    let new_hash = hasher.finish();

    let now = Instant::now();

    // Single lock for all lead typing state
    let (is_working, prev_working) = {
        let mut lt = state.lead_typing.lock().unwrap();
        let pane_changed = lt.pane_hash != 0 && new_hash != lt.pane_hash;
        lt.pane_hash = new_hash;

        if pane_changed {
            lt.last_activity = Some(now);
        }

        let is_working = determine_lead_working(
            pane_changed,
            lt.last_activity,
            now,
            LEAD_TYPING_GRACE_PERIOD,
        );

        let prev = lt.working;
        lt.working = is_working;
        (is_working, prev)
    };

    if is_working != prev_working {
        web::broadcast_lead_typing(tx, is_working);
    }
}

/// Check if the lead tmux window is still alive and respawn it if not.
///
/// This runs on a blocking thread since it calls tmux commands.
/// If the tmux session still exists but the lead window is gone, recreates
/// the lead window using `spawn_lead` (which handles --resume fallback).
fn check_and_respawn_lead(
    session: &str,
    workdir: &Path,
    project_name: &str,
    additional_dirs: &[PathBuf],
) {
    // First check if the tmux session itself exists. If the entire session
    // is gone (e.g., user killed it), don't try to recreate — that's intentional.
    let session_check = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output();
    match session_check {
        Ok(o) if o.status.success() => {}
        _ => return, // session gone entirely, don't interfere
    }

    // Session exists — check if the lead window is present
    match crate::tmux::window_exists(session, "lead") {
        Ok(true) => {} // lead is alive, nothing to do
        Ok(false) => {
            warn!("Lead window missing in session {}, respawning...", session);
            match crate::tmux::spawn_lead(
                session,
                &workdir.to_string_lossy(),
                project_name,
                additional_dirs,
            ) {
                Ok(()) => info!("Successfully respawned lead window"),
                Err(e) => error!("Failed to respawn lead window: {}", e),
            }
        }
        Err(e) => {
            warn!("Failed to check lead window status: {}", e);
        }
    }
}

/// Pure decision function: is the lead still working?
///
/// Returns `true` if the pane just changed, or if the last activity was within
/// the grace period. Returns `false` only after sustained inactivity.
fn determine_lead_working(
    pane_changed: bool,
    last_activity: Option<Instant>,
    now: Instant,
    grace_period: Duration,
) -> bool {
    if pane_changed {
        return true;
    }
    match last_activity {
        Some(last) => now.duration_since(last) < grace_period,
        None => false,
    }
}

/// Check for idle coworkers and send them on a break after the idle timeout.
///
/// A coworker is considered idle if they have no tasks in "in_progress" status
/// with their name as owner. After 30 seconds of continuous idle, they are
/// automatically sent on a break.
///
/// IMPORTANT: Coworkers with open PRs or active review assignments are NEVER
/// sent on a break, regardless of idle time. This ensures they can respond to PR
/// feedback, merge their work, or complete their review.
///
/// Also enforces a minimum lifetime check - coworkers must be alive for at least
/// 5 minutes before they can be sent on a break. This prevents spawn storms where
/// coworkers are rapidly sent on breaks.
async fn check_and_shutdown_idle_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    debug!(
        "Idle shutdown check: active={}, busy=[{}], open_prs=[{}], reviewers=[{}], unblocked_deps=[{}]",
        snap.active_coworkers.len(),
        snap.busy_coworkers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.coworkers_with_open_prs
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.active_reviewers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.coworkers_with_unblocked_deps
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    );

    // Pure decision: who should be shut down?
    let to_shutdown = {
        let mut phases = state.coworker_lifecycles.write().await;
        crate::rules::decide_idle_shutdowns(
            &snap.coworker_snapshots,
            &snap.busy_coworkers,
            &snap.coworkers_with_open_prs,
            &snap.active_reviewers,
            &snap.coworkers_with_unblocked_deps,
            &mut phases,
            snap.now,
            snap.now_utc,
            IDLE_BREAK_DURATION,
            MINIMUM_COWORKER_LIFETIME,
        )
    };

    let mut effects = Vec::new();

    // Determine effects for idle coworkers
    for decision in to_shutdown {
        let name = &decision.name;

        // For isolated coworkers (reviewers), verify the review was actually posted
        let (should_shutdown, shutdown_msg) = if decision.is_isolated {
            // Look up the PR this reviewer was assigned to
            let pr_number = {
                let github_state = state.github_state.lock().await;
                github_state.pr_for_reviewer(name)
            };

            match pr_number {
                Some(pr) => {
                    // Check if review was actually posted
                    if pr_has_claude_review(pr, state) {
                        info!(
                            "Sending reviewer {} on a break (review verified for PR #{})",
                            name, pr
                        );
                        (
                            true,
                            daemon_messages::break_review_complete(
                                name,
                                pr,
                                config::get_personality(),
                            ),
                        )
                    } else {
                        warn!(
                            "Reviewer {} is idle but no review found for PR #{} - keeping alive",
                            name, pr
                        );
                        // Don't shutdown - post a warning to the channel so the team knows
                        effects.push(Effect::PostToChannel {
                            sender: "system".to_string(),
                            message: format!(
                                "⚠️ Reviewer {} is idle but hasn't posted review for PR #{} yet",
                                name, pr
                            ),
                        });
                        (false, String::new())
                    }
                }
                None => {
                    // Can't find PR assignment — check if their work already merged
                    if snap.coworkers_with_merged_prs.contains(name) {
                        info!(
                            "Isolated coworker {} has no PR assignment but has merged PR, sending on a break",
                            name
                        );
                        (
                            true,
                            daemon_messages::break_work_merged(name, config::get_personality()),
                        )
                    } else {
                        warn!(
                            "Isolated coworker {} has no PR assignment found, sending on a break",
                            name
                        );
                        (
                            true,
                            daemon_messages::break_no_pr(name, config::get_personality()),
                        )
                    }
                }
            }
        } else if snap.coworkers_with_merged_prs.contains(name) {
            info!("Sending idle coworker {} on a break (PR merged)", name);
            (
                true,
                daemon_messages::break_work_merged(name, config::get_personality()),
            )
        } else {
            info!(
                "Sending idle coworker {} on a break (idle for 30+ seconds)",
                name
            );
            (
                true,
                daemon_messages::break_idle(name, config::get_personality()),
            )
        };

        if !should_shutdown {
            continue;
        }

        // Post system message, broadcast status, and shut down
        effects.push(Effect::PostToChannel {
            sender: "system".to_string(),
            message: shutdown_msg,
        });
        effects.push(Effect::BroadcastCoworkerUpdate {
            name: name.clone(),
            status: "stopped".to_string(),
            current_task: None,
        });
        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
        });
    }

    effects
}

/// Check for coworkers whose Claude Code session is interrupted and nudge them to continue.
///
/// Captures each active coworker's tmux pane content and checks for interruption
/// indicators ("Interrupted" or "What should Claude do instead?"). If the interrupted
/// state persists for 60 seconds, sends a "continue" nudge to unstick them.
async fn check_and_nudge_interrupted_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Pure decision: who should be nudged?
    let to_nudge = {
        let mut phases = state.coworker_lifecycles.write().await;
        crate::rules::decide_interrupt_nudges(
            &snap.coworker_snapshots,
            &snap.pane_contents,
            &mut phases,
            snap.now,
            INTERRUPTED_NUDGE_DURATION,
        )
    };

    let mut effects = Vec::new();
    for nudge in to_nudge {
        let name = &nudge.name;
        info!(
            "Nudging interrupted coworker: {} (interrupted for 60+ seconds)",
            name
        );

        effects.push(Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!("🔄 Nudging interrupted coworker: {}", name),
        });
        effects.push(Effect::NudgeCoworker {
            name: name.clone(),
            message: "continue".to_string(),
        });
    }

    effects
}

// Interactive prompt detection moved to crate::rules::detect_interactive_prompt

/// Detect coworkers waiting on interactive prompts (plan approval, permission dialogs, etc.)
/// and nudge the lead so they can provide guidance.
///
/// Unlike interrupted coworkers (who just need a "continue"), prompted coworkers need a
/// *human decision* — so we alert the lead with context about what's being asked.
async fn check_and_nudge_prompted_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Pure decision: which coworkers need lead attention?
    let to_nudge = {
        let mut phases = state.coworker_lifecycles.write().await;
        crate::rules::decide_prompt_nudges(
            &snap.coworker_snapshots,
            &snap.pane_contents,
            &mut phases,
        )
    };

    let mut effects = Vec::new();
    for nudge in to_nudge {
        let (name, label) = (&nudge.name, &nudge.label);
        info!("Coworker {} is waiting on a {}, nudging lead", name, label);

        effects.push(Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!(
                "⚠️ @lead {} is waiting on a {} — check their tmux pane and respond",
                name, label
            ),
        });
        effects.push(Effect::NudgeLead {
            message: format!(
                "{} is waiting on a {} — run: tmux select-window -t {}:{}",
                name, label, snap.session_name, name
            ),
        });
    }

    effects
}

/// Detect coworkers whose tmux pane content has not changed for `COWORKER_STUCK_DURATION`,
/// kill them, and respawn with their current task prompt.
///
/// Uses the same pane-hashing approach as lead typing detection. Each tick we hash
/// every coworker's captured pane content and compare to the previous hash. If the
/// hash has been unchanged for 5 minutes, the coworker is assumed stuck (hung process,
/// infinite loop, etc.) and is restarted.
fn check_and_restart_stuck_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    use effects::Effect;
    use std::hash::{Hash, Hasher};

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    let now = snap.now;
    let mut effects = Vec::new();
    let mut hashes = state.coworker_pane_hashes.lock().unwrap();

    for (name, content) in &snap.pane_contents {
        // Hash the pane content for cheap comparison
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let new_hash = hasher.finish();

        let entry = hashes.entry(name.clone()).or_insert((new_hash, now));

        if entry.0 != new_hash {
            // Pane changed — update hash and timestamp
            entry.0 = new_hash;
            entry.1 = now;
            continue;
        }

        // Hash unchanged — check if stuck long enough
        if now.duration_since(entry.1) < COWORKER_STUCK_DURATION {
            continue;
        }

        // Find the coworker's in-progress task
        let task = snap
            .in_progress_tasks
            .iter()
            .find(|(_id, _subject, owner)| owner.eq_ignore_ascii_case(name));

        let Some((task_id, task_subject, _owner)) = task else {
            debug!(
                "Coworker {} pane stuck but no in-progress task found — skipping",
                name
            );
            continue;
        };

        info!(
            "Coworker {} pane unchanged for {}s — restarting for task #{}",
            name,
            COWORKER_STUCK_DURATION.as_secs(),
            task_id
        );

        let prompt = format!(
            "You've been assigned task #{}: {}. Your previous session appeared stuck so you were restarted. Check your git status and continue where you left off.",
            task_id, task_subject
        );

        // Shutdown existing session, then spawn fresh
        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
        });
        effects.push(Effect::SpawnCoworker {
            name: name.clone(),
            prompt,
            isolated: false,
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Restarted stuck coworker {} (pane unchanged for {}s) — resuming task #{}",
                name,
                COWORKER_STUCK_DURATION.as_secs(),
                task_id
            ),
        });

        // Reset the hash tracker so we don't immediately re-trigger
        entry.1 = now;
    }

    // Clean up entries for coworkers no longer in the snapshot
    hashes.retain(|name, _| snap.pane_contents.contains_key(name));

    effects
}

// Usage limit patterns and parse_usage_limit_duration moved to crate::rules

/// Check all active coworkers' tmux panes for usage/rate limit messages.
/// If detected, schedule a nudge for when the limit expires.
///
/// Usage limits are account-wide, so when one coworker hits it, all of them
/// will be stuck. We detect it from any coworker, parse the expiry, and
/// schedule a single nudge time for everyone.
fn check_for_usage_limits(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    // If we already have a nudge scheduled, don't re-detect
    if snap.usage_limit_nudge_scheduled {
        return vec![];
    }

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Pure decision: detect usage limit
    let decision = crate::rules::decide_usage_limit_detection(&snap.pane_contents);

    let detected_coworker = match decision {
        crate::rules::UsageLimitDecision::Detected { coworker } => coworker,
        _ => return vec![],
    };

    // Find the pane content for the detected coworker to parse duration
    let pane_content = snap
        .pane_contents
        .get(&detected_coworker)
        .map(|s| s.as_str())
        .unwrap_or("");

    let wait_duration = crate::rules::parse_usage_limit_duration(pane_content);
    let nudge_time = tokio::time::Instant::now() + wait_duration + USAGE_LIMIT_NUDGE_BUFFER;

    let human_duration = if wait_duration.as_secs() >= 3600 {
        format!(
            "{}h {}m",
            wait_duration.as_secs() / 3600,
            (wait_duration.as_secs() % 3600) / 60
        )
    } else {
        format!("{}m", wait_duration.as_secs() / 60)
    };

    info!(
        "Usage limit detected via coworker {} — scheduling nudge in {} + 30s buffer",
        detected_coworker, human_duration
    );

    vec![
        Effect::SetUsageLimitNudge { at: nudge_time },
        Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!(
                "⏳ Usage limit detected (via {}). All coworkers will be nudged in ~{} when it resets.",
                detected_coworker, human_duration
            ),
        },
    ]
}

/// Check if a scheduled usage limit nudge is due, and if so, nudge all active coworkers.
fn maybe_nudge_usage_limit_expiry(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    // Pure decision: should we nudge?
    let decision = crate::rules::decide_usage_limit_expiry(
        snap.usage_limit_nudge_at,
        tokio::time::Instant::now(),
    );

    if decision != crate::rules::UsageLimitExpiryDecision::NudgeNow {
        return vec![];
    }

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    info!(
        "Usage limit expired — nudging {} active coworkers",
        snap.active_coworkers.len()
    );

    let mut effects = vec![
        Effect::ClearUsageLimitNudge,
        Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!(
                "🔔 Usage limit expired — nudging {} coworkers to resume work",
                snap.active_coworkers.len()
            ),
        },
    ];

    for cw in &snap.active_coworkers {
        effects.push(Effect::NudgeCoworker {
            name: cw.name.clone(),
            message: "continue".to_string(),
        });
    }

    effects
}

/// Get list of coworker names who have open PRs.
///
/// A coworker is considered to have an open PR if the PR's branch name
/// starts with the coworker's name (e.g., "lexington/fix-auth").
/// Coworkers with open PRs should NEVER be sent on a break.
/// Get coworker names that have open PRs (branch name starts with coworker name).
///
/// Uses cached data from the latest `poll_prs_for_issues` call when available,
/// avoiding a separate `gh pr list` API call.
fn get_coworkers_with_open_prs(state: &DaemonState) -> Vec<String> {
    let cached = state.cached_open_pr_branches.read().unwrap();
    if !cached.is_empty() {
        return cached
            .iter()
            .filter_map(|branch| coworker_from_branch(branch))
            .collect();
    }
    drop(cached);

    // Fallback to API call if cache is empty (e.g., first tick before poll runs)
    let output = std::process::Command::new("gh")
        .args(["pr", "list", "--json", "headRefName"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return prs
                    .iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .and_then(coworker_from_branch)
                    })
                    .collect();
            }
            Vec::new()
        }
        _ => {
            debug!("Failed to get PRs from gh CLI for idle check");
            Vec::new()
        }
    }
}

/// How often to re-fetch merged PRs (5 minutes). Merges aren't urgent so
/// polling less frequently saves significant API calls.
const MERGED_PRS_FETCH_INTERVAL_SECS: u64 = 300;

/// Get coworker names that have recently merged PRs (branch name starts with coworker name).
///
/// Uses a time-based cache to reduce API calls. Merged PR status is only refreshed
/// every 5 minutes since merge events aren't time-critical.
fn get_coworkers_with_merged_prs(state: &DaemonState) -> HashSet<String> {
    // Check if we need to refresh (uses CooldownTracker instead of standalone timestamp)
    let needs_refresh = {
        let cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.check(
            "merged_pr_fetch",
            "global",
            Duration::from_secs(MERGED_PRS_FETCH_INTERVAL_SECS),
        )
    };

    if !needs_refresh {
        let cached = state.cached_merged_pr_coworkers.read().unwrap();
        return cached.clone();
    }

    // Fetch from API
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "20",
            "--json",
            "headRefName",
        ])
        .output();

    let result = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                prs.iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .and_then(coworker_from_branch)
                    })
                    .collect()
            } else {
                HashSet::new()
            }
        }
        _ => {
            debug!("Failed to get merged PRs from gh CLI for idle check");
            HashSet::new()
        }
    };

    // Update cache
    {
        let mut cached = state.cached_merged_pr_coworkers.write().unwrap();
        *cached = result.clone();
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("merged_pr_fetch", "global");
    }

    result
}

/// Watchdog task that manages the gh webhook forward process with periodic restarts.
///
/// The `gh webhook forward` command can sometimes stop delivering events without
/// terminating. This watchdog ensures reliability by:
/// 1. Starting the forwarder process
/// 2. Restarting it every `restart_interval_secs` seconds
/// 3. Cleaning up on shutdown signal
async fn webhook_forwarder_watchdog(
    port: u16,
    restart_interval_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Get the GitHub repo name (owner/repo) for webhook forwarding
    let gh_repo = match get_github_repo_name() {
        Some(repo) => repo,
        None => {
            warn!(
                "Could not determine GitHub repo (gh repo view failed). Webhook forwarding disabled."
            );
            warn!("Webhooks will still work if configured manually in GitHub settings.");
            return;
        }
    };

    // Ensure gh-webhook extension is installed
    if !ensure_gh_webhook_extension() {
        warn!("gh-webhook extension not available, webhook forwarding disabled");
        return;
    }

    let url = format!("http://localhost:{}/webhook", port);
    info!(
        "Starting webhook forwarder watchdog (restart every {}s)",
        restart_interval_secs
    );

    let mut current_process: Option<std::process::Child> = None;

    loop {
        // Kill any existing process before starting a new one
        if let Some(mut child) = current_process.take() {
            debug!("Stopping previous webhook forwarder process");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Start new forwarder process
        match start_gh_webhook_forward(&gh_repo, &url) {
            Ok(child) => {
                info!("Started gh webhook forward for {} to {}", gh_repo, url);
                current_process = Some(child);
            }
            Err(e) => {
                warn!("Failed to start gh webhook forward: {}", e);
            }
        }

        // Wait for restart interval or shutdown signal
        let restart_delay =
            tokio::time::sleep(std::time::Duration::from_secs(restart_interval_secs));

        tokio::select! {
            _ = restart_delay => {
                debug!("Webhook forwarder restart interval elapsed, restarting...");
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Webhook forwarder watchdog received shutdown signal");
                    break;
                }
            }
        }
    }

    // Clean up on exit
    if let Some(mut child) = current_process {
        info!("Stopping gh webhook forward...");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Get the GitHub repo name (owner/repo) from the current directory.
fn get_github_repo_name() -> Option<String> {
    std::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ensure the gh-webhook extension is installed.
fn ensure_gh_webhook_extension() -> bool {
    let extension_check = std::process::Command::new("gh")
        .args(["extension", "list"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("webhook"))
        .unwrap_or(false);

    if extension_check {
        return true;
    }

    info!("Installing gh-webhook extension...");
    match std::process::Command::new("gh")
        .args(["extension", "install", "cli/gh-webhook"])
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            warn!("Failed to install gh-webhook extension: {}", e);
            false
        }
    }
}

/// Start the gh webhook forward process.
fn start_gh_webhook_forward(repo: &str, url: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("gh")
        .args([
            "webhook",
            "forward",
            "--events=pull_request,pull_request_review,check_run,status,issue_comment,pull_request_review_comment",
            &format!("--repo={}", repo),
            &format!("--url={}", url),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Background task that polls PRs for actionable issues.
///
/// Checks all open PRs every `interval_secs` seconds for:
/// - Merge conflicts
/// - CI failures
/// - Changes requested
/// - Approved and ready to merge
///
/// Nudges the PR owner (extracted from branch prefix) or an idle coworker.
async fn pr_poll_task(
    state: Arc<DaemonState>,
    interval_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let interval = Duration::from_secs(interval_secs);

    loop {
        // Wait for the interval or shutdown signal
        let delay = tokio::time::sleep(interval);

        tokio::select! {
            _ = delay => {
                // Time to poll PRs
                if let Err(e) = poll_prs_for_issues(&state).await {
                    warn!("PR poll error: {}", e);
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("PR poll task received shutdown signal");
                    break;
                }
            }
        }
    }
}

// ============================================================================
// Chat Monitor - @mention routing
// ============================================================================

/// Background task that monitors the channel for @mentions and routes them.
///
/// Uses `tailf` to watch `channel.jsonl` for new messages in real-time.
/// When a message with @mentions is detected, spawns/nudges the mentioned coworkers.
async fn chat_monitor_loop(
    state: Arc<DaemonState>,
    channel_path: PathBuf,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Start tailing from the end of the file (0 = no initial lines)
    let mut tailer = match tailf::tailf(&channel_path, Some(0)) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to start tailf on channel file: {}", e);
            return;
        }
    };

    info!("Chat monitor watching: {}", channel_path.display());

    loop {
        tokio::select! {
            // New line from tailf
            Some(result) = async { Some(tailer.next().await) } => {
                match result {
                    Ok(Some(bytes)) => {
                        // Convert bytes to string
                        let line = match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(e) => {
                                debug!("Invalid UTF-8 in channel line: {}", e);
                                continue;
                            }
                        };
                        // Parse the line as a Message
                        match serde_json::from_str::<Message>(&line) {
                            Ok(msg) => {
                                // Skip messages from protected senders (loop protection),
                                // but first check for @lead mentions that need nudging.
                                if SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(&msg.from)) {
                                    // System/daemon messages may contain @lead that still
                                    // needs to trigger a nudge (e.g., orphaned worktree
                                    // warnings). Route @lead before skipping.
                                    // Exclude "user" — user messages with @lead are already
                                    // handled in handle_channel_post to avoid double-nudging.
                                    if !msg.from.eq_ignore_ascii_case("user")
                                        && msg.content.to_lowercase().contains("@lead")
                                    {
                                        let nudge_text = format!("{}: {}", msg.from, msg.content);
                                        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
                                            warn!("Failed to nudge lead for @lead in {} message: {}", msg.from, e);
                                        } else {
                                            info!("Nudged lead about @lead mention in {} message", msg.from);
                                        }
                                        state.send_push_notification(
                                            &format!("@lead from {}", msg.from),
                                            &msg.content,
                                            "mention",
                                        );
                                    }
                                    continue;
                                }
                                // Route any @mentions in the message
                                route_mentions(&state, &msg).await;
                            }
                            Err(e) => {
                                debug!("Failed to parse channel message: {} (line: {})", e, line);
                            }
                        }
                    }
                    Ok(None) => {
                        // No new content, continue waiting
                    }
                    Err(e) => {
                        warn!("tailf error: {}", e);
                    }
                }
            }

            // Shutdown signal
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Chat monitor task received shutdown signal");
                    break;
                }
            }
        }
    }
}

/// Extract @mentions from message content and route to coworkers.
///
/// For each valid coworker name mentioned:
/// - If the coworker is not running, spawn them with --resume
/// - Nudge them with the message context
///
/// Also supports @all to broadcast to every active coworker and the lead.
async fn route_mentions(state: &DaemonState, msg: &Message) {
    // Check for @all broadcast first
    if contains_at_all(&msg.content) {
        route_at_all(state, msg);
        return;
    }

    let mentions = extract_mentions(&msg.content);

    if mentions.is_empty() {
        return;
    }

    debug!(
        "Found {} @mention(s) in message from {}: {:?}",
        mentions.len(),
        msg.from,
        mentions
    );

    for name in mentions {
        let is_running = state.coworkers.get(&name).is_some();
        let nudge_text = format!("{} said: {}", msg.from, msg.content);

        // Decide action using pure decision function
        let action = crate::rules::decide_mention_action(
            &name,
            &msg.from,
            is_running,
            state.is_at_dev_limit(),
            &nudge_text,
        );

        match action {
            crate::rules::MentionAction::Nudge {
                name: ref n,
                message: ref m,
            } => {
                if let Err(e) = state.coworkers.nudge(n, m) {
                    warn!("Failed to nudge {} about @mention: {}", n, e);
                } else {
                    info!("Nudged {} about @mention from {}", n, msg.from);
                }
            }
            crate::rules::MentionAction::Spawn {
                name: ref n,
                message: ref m,
            } => {
                info!("Spawning mentioned coworker {} (not currently running)", n);
                match state.spawn_coworker(n, true, Some(m.as_str()), false).await {
                    Ok(_) => {
                        info!("Spawned coworker {} via @mention", n);
                        let spawn_msg = Message::text(
                            "midtown",
                            format!("🚀 Called in {} in response to @mention", n),
                        );
                        if let Err(e) = state.send_and_broadcast(&spawn_msg) {
                            warn!("Failed to post call-in message: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", n, e);
                        let err_msg = Message::text(
                            "midtown",
                            format!("⚠️ Failed to call in {} for @mention: {}", n, e),
                        );
                        let _ = state.send_and_broadcast(&err_msg);
                    }
                }
            }
            crate::rules::MentionAction::Skip { ref reason } => {
                debug!("{}", reason);
                if reason.contains("dev limit") {
                    let err_msg = Message::text(
                        "midtown",
                        format!(
                            "⚠️ Cannot call in {} for @mention: dev coworkers limit reached",
                            name
                        ),
                    );
                    let _ = state.send_and_broadcast(&err_msg);
                }
            }
        }
    }
}

/// Route an @all broadcast: nudge every active coworker and the lead, except the sender.
fn route_at_all(state: &DaemonState, msg: &Message) {
    let active_coworkers = state.coworkers.list();
    let nudge_text = format!("{} said: {}", msg.from, msg.content);

    info!(
        "@all broadcast from {} to {} active coworker(s) + lead",
        msg.from,
        active_coworkers.len()
    );

    // Nudge the lead (unless the lead sent the message)
    if !msg.from.eq_ignore_ascii_case("lead") {
        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
            warn!("Failed to nudge lead for @all: {}", e);
        } else {
            info!("Nudged lead for @all from {}", msg.from);
        }
    }

    // Nudge all active coworkers (except the sender)
    for coworker in &active_coworkers {
        if coworker.name.eq_ignore_ascii_case(&msg.from) {
            continue;
        }

        if let Err(e) = state.coworkers.nudge(&coworker.name, &nudge_text) {
            warn!("Failed to nudge {} for @all: {}", coworker.name, e);
        } else {
            info!("Nudged {} for @all from {}", coworker.name, msg.from);
        }
    }
}

/// Auto-merge a PR using `gh pr merge --squash`.
///
/// Posts a channel message on success or failure.
async fn auto_merge_pr(
    state: &DaemonState,
    pr_number: u64,
    title: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let output = tokio::process::Command::new("gh")
        .args(["pr", "merge", &pr_number.to_string(), "--squash", "--auto"])
        .output()
        .await?;

    if output.status.success() {
        info!("Auto-merge enabled for PR #{} ({})", pr_number, title);
        let msg = Message::new(
            "midtown",
            format!(
                "🤝 Auto-merge enabled for PR #{} ({}) — approved with all checks passing",
                pr_number,
                truncate_str(title, 40)
            ),
            MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge message: {}", e);
        }
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let err_msg = format!("gh pr merge failed for PR #{}: {}", pr_number, stderr);
        warn!("{}", err_msg);
        let msg = Message::new(
            "midtown",
            format!(
                "⚠️ Auto-merge failed for PR #{} ({}) — {}",
                pr_number,
                truncate_str(title, 40),
                truncate_str(stderr.trim(), 80)
            ),
            MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge failure message: {}", e);
        }
        Err(err_msg.into())
    }
}

/// Poll all open PRs and nudge for actionable issues.
async fn poll_prs_for_issues(
    state: &DaemonState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    debug!("Polling PRs for actionable issues...");

    // Get list of active coworkers
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();

    // Run gh pr list command (include createdAt and isDraft for review filtering)
    // Include state field to filter out merged/closed PRs after restart
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state",
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Hash the response to detect changes. If the PR data hasn't changed since the last poll,
    // skip the expensive lock acquisition, issue detection, and nudge logic.
    let response_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stdout.hash(&mut hasher);
        hasher.finish()
    };
    {
        let mut last_hash = state.last_pr_poll_hash.lock().await;
        if *last_hash == response_hash && response_hash != 0 {
            debug!("PR poll: data unchanged, skipping processing");
            return Ok(());
        }
        *last_hash = response_hash;
    }

    let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)?;

    // Cleanup old tracking entries, but preserve assignments for active coworkers
    // so reviewers don't lose their PR tracking while still running
    let active_coworker_names: HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.clone())
        .collect();
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut github_state = state.github_state.lock().await;
        github_state.cleanup_expired_preserving(&active_coworker_names);
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.cleanup(Duration::from_secs(7200)); // 2 hours
    }

    // Filter to only open PRs (defense-in-depth: gh pr list --state open should only return
    // open PRs, but verify via the state field to guard against stale/cached results)
    let prs: Vec<serde_json::Value> = prs
        .into_iter()
        .filter(|pr| {
            let state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("OPEN");
            state == "OPEN"
        })
        .collect();

    // Cache open PR branch names for reuse by get_coworkers_with_open_prs
    {
        let branches: Vec<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        let mut cached = state.cached_open_pr_branches.write().unwrap();
        *cached = branches;
    }

    // Clean up persistent reviewer assignments for PRs that are no longer open
    {
        let open_pr_numbers: Vec<u64> = prs
            .iter()
            .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
            .collect();
        let mut github_state = state.github_state.lock().await;
        github_state.cleanup_closed_prs(&open_pr_numbers);
        github_state.cleanup_expired_preserving(&active_coworker_names);
        // Sync in-memory review cache to persistent state before saving
        {
            let cache = state.reviewed_prs_cache.read().unwrap();
            github_state.reviewed_prs = cache.clone();
        }
        if let Err(e) = crate::github_state::save_state_for_repo(&state.repo_name, &github_state) {
            warn!("Failed to save github-state.json after cleanup: {}", e);
        }
        // Also clean up the in-memory cache for closed PRs
        {
            let mut cache = state.reviewed_prs_cache.write().unwrap();
            let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
            cache.retain(|pr| open_set.contains(pr));
        }
    }

    for pr in &prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Extract owner from branch prefix (e.g., "amsterdam/feature" -> "amsterdam")
        let owner = head_ref.split('/').next().unwrap_or("");

        // Check for actionable issues
        let issues = detect_pr_issues(pr);

        for issue_type in issues {
            // Check if we should nudge for this issue
            let should_nudge = {
                let tracker = state.pr_issue_tracker.lock().await;
                tracker.should_nudge(pr_number, issue_type)
            };

            if !should_nudge {
                continue;
            }

            // For approved PRs with all checks passing, auto-merge instead of nudging
            use crate::rules::{PrAction, decide_pr_issue_action};
            if issue_type == PrIssueType::Approved && is_auto_mergeable(pr) {
                info!(
                    "PR #{} ({}) is approved with all checks passing — auto-merging",
                    pr_number, title
                );
                if let Err(e) = auto_merge_pr(state, pr_number, title).await {
                    warn!("Auto-merge failed for PR #{}: {}", pr_number, e);
                }
                // Always record the cooldown, even on failure, to prevent
                // retrying every poll interval (30s) for persistent failures.
                {
                    let mut tracker = state.pr_issue_tracker.lock().await;
                    tracker.record_nudge(pr_number, issue_type);
                }
                continue;
            }

            // Format the nudge message
            let message = format!(
                "PR #{} ({}) - {}: {}",
                pr_number,
                truncate_str(title, 40),
                issue_type,
                get_issue_action(issue_type)
            );

            // Decide action using pure decision function
            let action =
                decide_pr_issue_action(owner, &active_coworkers, state.is_at_dev_limit(), &message);

            let nudged = match action {
                PrAction::NudgeOwner {
                    owner: ref o,
                    message: ref msg,
                } => match state.coworkers.nudge(o, msg) {
                    Ok(()) => {
                        info!("Nudged {} about PR #{}: {}", o, pr_number, issue_type);
                        true
                    }
                    Err(e) => {
                        warn!("Failed to nudge {}: {}", o, e);
                        false
                    }
                },
                PrAction::SpawnOwner {
                    owner: ref o,
                    message: ref msg,
                } => {
                    info!(
                        "PR #{} owner {} is not active, spawning to address {}",
                        pr_number, o, issue_type
                    );
                    match state
                        .spawn_coworker(o, true, Some(msg.as_str()), false)
                        .await
                    {
                        Ok(_) => {
                            info!(
                                "Spawned {} to address {} on PR #{}",
                                o, issue_type, pr_number
                            );
                            state.broadcast_coworker_update(o, "running", None);
                            let call_msg = Message::text(
                                "midtown",
                                daemon_messages::called_in_pr_issue(
                                    o,
                                    &issue_type.to_string(),
                                    pr_number,
                                    config::get_personality(),
                                ),
                            );
                            if let Err(e) = state.send_and_broadcast(&call_msg) {
                                warn!("Failed to post call-in message: {}", e);
                            }
                            true
                        }
                        Err(e) => {
                            warn!(
                                "Failed to spawn {} for PR #{} {}: {}",
                                o, pr_number, issue_type, e
                            );
                            let channel_message = format!(
                                "PR #{} ({}) owned by {} - {}: {} (call-in failed)",
                                pr_number,
                                truncate_str(title, 40),
                                o,
                                issue_type,
                                get_issue_action(issue_type)
                            );
                            let fallback =
                                Message::new("midtown", channel_message, MessageType::Text);
                            if let Err(e) = state.send_and_broadcast(&fallback) {
                                warn!("Failed to post PR issue to channel: {}", e);
                            }
                            true
                        }
                    }
                }
                PrAction::PostToChannel { message: ref msg } => {
                    let channel_msg = Message::new("midtown", msg.clone(), MessageType::Text);
                    if let Err(e) = state.send_and_broadcast(&channel_msg) {
                        warn!("Failed to post PR issue to channel: {}", e);
                    }
                    info!(
                        "Posted PR #{} issue to channel (no owner): {}",
                        pr_number, issue_type
                    );
                    true
                }
                PrAction::Skip { ref reason } => {
                    debug!("{}", reason);
                    false
                }
            };

            // Record the nudge
            if nudged {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_nudge(pr_number, issue_type);
            }
        }
    }

    // Auto-spawn reviewers for PRs that need review
    spawn_reviewers_for_prs(state, &prs).await;

    // Check for stuck conditions and nudge lead if self-healing has failed
    check_for_stuck_conditions(state, &prs).await;

    Ok(())
}

/// Check for stuck conditions and nudge the lead when the daemon can't self-heal.
///
/// This function runs during each PR poll cycle and checks for:
/// 1. PRs open with no review for too long
/// 2. PRs with unresolved feedback for too long
/// 3. PRs that are approved + CI green but not merging
/// 4. Coworkers who are silent (no channel activity) for too long
/// 5. Review backlog (more PRs need review than slots available)
///
/// Each condition has a cooldown to avoid spamming the lead.
async fn check_for_stuck_conditions(state: &DaemonState, prs: &[serde_json::Value]) {
    let mut tracker = state.stuck_tracker.lock().await;
    tracker.cleanup();

    let now = Instant::now();

    // Track how many nudges we send this cycle (for logging)
    let mut nudge_count = 0;

    // --- Scenario 1: PR open with no review for N minutes ---
    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            continue;
        }

        let review_decision = pr
            .get("reviewDecision")
            .and_then(|r| r.as_str())
            .unwrap_or("");

        let age_secs = get_pr_age_secs(pr).unwrap_or(0);
        let pr_id = pr_number.to_string();

        // No review decision at all and PR is old enough
        if review_decision.is_empty() && age_secs >= STUCK_NO_REVIEW_DURATION.as_secs() {
            // Check if a reviewer is assigned (daemon tried to self-heal)
            let is_assigned = {
                let github_state = state.github_state.lock().await;
                github_state.is_assigned(pr_number)
            };

            tracker.track(&pr_id, StuckConditionType::NoReview);
            if tracker.should_nudge(&pr_id, StuckConditionType::NoReview) {
                let context = if is_assigned {
                    "I assigned a reviewer but no review has been posted yet"
                } else {
                    "I couldn't assign a reviewer"
                };
                let nudge = format!(
                    "@lead PR #{} ({}) has been open for {} minutes with no review — {}",
                    pr_number,
                    truncate_str(title, 40),
                    age_secs / 60,
                    context,
                );
                nudge_lead_stuck(state, &nudge);
                tracker.record_nudge(&pr_id, StuckConditionType::NoReview);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::NoReview);
        }

        // --- Scenario 2: Unresolved feedback (changes requested) for N minutes ---
        if review_decision == "CHANGES_REQUESTED" {
            let first_detected = tracker.track(&pr_id, StuckConditionType::UnresolvedFeedback);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_UNRESOLVED_FEEDBACK_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::UnresolvedFeedback)
            {
                let nudge = format!(
                    "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't pushed new changes",
                    pr_number,
                    truncate_str(title, 40),
                    stuck_duration.as_secs() / 60,
                );
                nudge_lead_stuck(state, &nudge);
                tracker.record_nudge(&pr_id, StuckConditionType::UnresolvedFeedback);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::UnresolvedFeedback);
        }

        // --- Scenario 3: Approved + CI green but not merging ---
        if is_auto_mergeable(pr) {
            let first_detected = tracker.track(&pr_id, StuckConditionType::MergeReady);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_MERGE_READY_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::MergeReady)
            {
                let nudge = format!(
                    "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — auto-merge may have failed",
                    pr_number,
                    truncate_str(title, 40),
                    stuck_duration.as_secs() / 60,
                );
                nudge_lead_stuck(state, &nudge);
                tracker.record_nudge(&pr_id, StuckConditionType::MergeReady);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::MergeReady);
        }
    }

    // --- Scenario 4: Silent coworker (claimed task, no channel activity) ---
    {
        let busy_coworkers = crate::tasks::get_busy_coworkers_for_repo(&state.repo_name);
        let lifecycles = state.coworker_lifecycles.read().await;

        for name in &busy_coworkers {
            let last_activity: Option<Instant> = lifecycles
                .get(name.as_str())
                .and_then(|lc| lc.last_activity);
            let is_silent = match last_activity {
                Some(last) => last.elapsed() >= STUCK_SILENT_COWORKER_DURATION,
                // No activity recorded — coworker hasn't posted to channel yet.
                // They're still initializing (loading plugins, restoring session, etc.).
                // Only start the silence clock after their first channel message.
                None => false,
            };

            if is_silent {
                tracker.track(name, StuckConditionType::SilentCoworker);
                if tracker.should_nudge(name, StuckConditionType::SilentCoworker) {
                    let task_info = crate::tasks::get_in_progress_tasks_with_subjects()
                        .into_iter()
                        .find(|(_, _, owner)| owner.eq_ignore_ascii_case(name))
                        .map(|(id, subject, _)| {
                            format!("task #{} ({})", id, truncate_str(&subject, 30))
                        })
                        .unwrap_or_else(|| "their task".to_string());

                    let prior_nudges =
                        tracker.nudge_count(name, StuckConditionType::SilentCoworker);

                    if prior_nudges == 0 {
                        // First nudge: ask the coworker directly before escalating
                        let nudge_msg = format!(
                            "Status check — you've been quiet on {} for over {} minutes. \
                             Are you stuck or still working?",
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                        );
                        if let Err(e) = state.coworkers.nudge(name, &nudge_msg) {
                            warn!("Failed to nudge silent coworker {}: {}", name, e);
                        }
                        // Post to channel so it's visible
                        let channel_msg = Message::system(format!(
                            "⚠️ Nudging {} — silent on {} for over {} minutes",
                            name,
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                        ));
                        if let Err(e) = state.send_and_broadcast(&channel_msg) {
                            warn!("Failed to post silent coworker nudge to channel: {}", e);
                        }
                    } else {
                        // Escalation: coworker didn't respond, notify lead
                        let nudge = format!(
                            "@lead {} has been silent on {} for over {} minutes \
                             (nudged {} previously with no response)",
                            name,
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                            name,
                        );
                        nudge_lead_stuck(state, &nudge);
                    }
                    tracker.record_nudge(name, StuckConditionType::SilentCoworker);
                    nudge_count += 1;
                }
            } else {
                tracker.clear(name, StuckConditionType::SilentCoworker);
            }
        }
    }

    // --- Scenario 5: Review backlog ---
    {
        let prs_needing_review: usize = prs
            .iter()
            .filter(|pr| {
                let review_decision = pr
                    .get("reviewDecision")
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
                !is_draft && review_decision.is_empty()
            })
            .count();

        let current_review_count = {
            let github_state = state.github_state.lock().await;
            github_state.active_count()
        };

        // Backlog exists when more PRs need review than we can handle
        if prs_needing_review > MAX_CONCURRENT_REVIEWS
            && current_review_count >= MAX_CONCURRENT_REVIEWS
        {
            tracker.track("backlog", StuckConditionType::ReviewBacklog);
            if tracker.should_nudge("backlog", StuckConditionType::ReviewBacklog) {
                let nudge = format!(
                    "@lead {} PRs need review but I'm at the max concurrent review limit ({}/{}) — some PRs may wait longer than usual",
                    prs_needing_review, current_review_count, MAX_CONCURRENT_REVIEWS,
                );
                nudge_lead_stuck(state, &nudge);
                tracker.record_nudge("backlog", StuckConditionType::ReviewBacklog);
                nudge_count += 1;
            }
        } else {
            tracker.clear("backlog", StuckConditionType::ReviewBacklog);
        }
    }

    if nudge_count > 0 {
        info!(
            "Stuck condition check: nudged lead about {} issue(s)",
            nudge_count
        );
    }
}

/// Helper to nudge the lead about a stuck condition and post to channel.
fn nudge_lead_stuck(state: &DaemonState, message: &str) {
    // Post to channel so it's visible in the log
    let msg = Message::system(format!("⚠️ {}", message));
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!("Failed to post stuck condition to channel: {}", e);
    }

    // Nudge the lead directly
    if let Err(e) = state.coworkers.nudge_lead(message) {
        warn!("Failed to nudge lead about stuck condition: {}", e);
    }
}

/// Spawn reviewers for PRs that need code review.
///
/// This function identifies PRs that:
/// - Are not drafts
/// - Are old enough (past the review delay)
/// - Don't have a Claude review comment yet
/// - Haven't been assigned for review recently
///
/// For each eligible PR, spawns a fresh coworker with an isolated task list
/// and nudges them to run `/code-review:code-review <pr-number>`. The isolated
/// task list ensures review sub-tasks don't pollute the shared task list.
async fn spawn_reviewers_for_prs(state: &DaemonState, prs: &[serde_json::Value]) {
    // Check rate limit
    let current_review_count = {
        let github_state = state.github_state.lock().await;
        github_state.active_count()
    };

    if current_review_count >= MAX_CONCURRENT_REVIEWS {
        debug!(
            "At max concurrent reviews ({}/{}), skipping auto-review spawn",
            current_review_count, MAX_CONCURRENT_REVIEWS
        );
        return;
    }

    let reviews_available = MAX_CONCURRENT_REVIEWS - current_review_count;
    let mut reviews_spawned = 0;

    for pr in prs {
        if reviews_spawned >= reviews_available {
            break;
        }

        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        // Skip draft PRs
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            debug!("PR #{} is a draft, skipping auto-review", pr_number);
            continue;
        }

        // Check if PR is old enough (enforce review delay)
        if let Some(age_secs) = get_pr_age_secs(pr)
            && age_secs < PR_REVIEW_DELAY_SECS
        {
            debug!(
                "PR #{} is too new ({}s < {}s), skipping auto-review",
                pr_number, age_secs, PR_REVIEW_DELAY_SECS
            );
            continue;
        }

        // Check if PR already has a Claude review.
        // This runs BEFORE the is_assigned check so that completed reviews
        // are detected even when a reviewer is still tracked as assigned
        // (e.g., after a daemon restart or when the reviewer posted a comment
        // instead of a formal GitHub review).
        if pr_has_claude_review(pr_number, state) {
            debug!("PR #{} already has a Claude review", pr_number);

            // Before cleaning up the assignment, check if the reviewer is still running.
            // If so, leave the assignment in place so the idle shutdown path can
            // properly send them off with break_review_complete() instead of break_no_pr().
            let reviewer_still_running = {
                let github_state = state.github_state.lock().await;
                if let Some(reviewer_name) = github_state.get_reviewer(pr_number) {
                    state.coworkers.get(reviewer_name).is_some()
                } else {
                    false
                }
            };

            if reviewer_still_running {
                debug!(
                    "PR #{} has Claude review but reviewer is still running — keeping assignment",
                    pr_number
                );
            } else {
                // Free the tracker slot — the review completed and the reviewer is gone
                let mut github_state = state.github_state.lock().await;
                if github_state.is_assigned(pr_number) {
                    debug!("PR #{} review completed, freeing tracker slot", pr_number);
                    github_state.remove_assignment(pr_number);
                    if let Err(e) =
                        crate::github_state::save_state_for_repo(&state.repo_name, &github_state)
                    {
                        warn!("Failed to save github-state.json: {}", e);
                    }
                }
            }

            // Nudge the PR author — review is complete but PR is still open,
            // so the author needs to address feedback and/or merge.
            let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
            let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
            let owner = head_ref.split('/').next().unwrap_or("");

            if !owner.is_empty() {
                // Check cooldown to avoid spamming
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge(pr_number, PrIssueType::ReviewComplete)
                };

                if should_nudge {
                    let nudge_msg = format!(
                        "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.",
                        pr_number,
                        truncate_str(title, 40)
                    );

                    let active_coworkers: Vec<String> = state
                        .coworkers
                        .list()
                        .iter()
                        .map(|c| c.name.clone())
                        .collect();

                    // Decide action using pure decision function
                    let action = crate::rules::decide_review_complete_action(
                        owner,
                        &active_coworkers,
                        state.is_at_dev_limit(),
                        &nudge_msg,
                    );

                    match action {
                        crate::rules::PrAction::NudgeOwner {
                            owner: ref o,
                            message: ref msg,
                        } => match state.coworkers.nudge(o, msg) {
                            Ok(()) => {
                                info!("Nudged {} about completed review on PR #{}", o, pr_number);
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to nudge {} about completed review on PR #{}: {}",
                                    o, pr_number, e
                                );
                            }
                        },
                        crate::rules::PrAction::SpawnOwner {
                            owner: ref o,
                            message: ref msg,
                        } => {
                            info!(
                                "PR #{} owner {} is idle/on a break, spawning to address completed review",
                                pr_number, o
                            );
                            match state
                                .spawn_coworker(o, true, Some(msg.as_str()), false)
                                .await
                            {
                                Ok(_) => {
                                    info!(
                                        "Spawned {} to address completed review on PR #{}",
                                        o, pr_number
                                    );
                                    state.broadcast_coworker_update(o, "running", None);
                                    let call_msg = Message::text(
                                        "midtown",
                                        daemon_messages::called_in_review_feedback(
                                            o,
                                            pr_number,
                                            config::get_personality(),
                                        ),
                                    );
                                    if let Err(e) = state.send_and_broadcast(&call_msg) {
                                        warn!("Failed to post call-in message: {}", e);
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to spawn {} for PR #{} review complete: {}",
                                        o, pr_number, e
                                    );
                                    let channel_message = format!(
                                        "PR #{} ({}) owned by {} - review complete: {} (call-in failed)",
                                        pr_number,
                                        truncate_str(title, 40),
                                        o,
                                        get_issue_action(PrIssueType::ReviewComplete)
                                    );
                                    let fallback =
                                        Message::new("midtown", channel_message, MessageType::Text);
                                    if let Err(e) = state.send_and_broadcast(&fallback) {
                                        warn!("Failed to post PR issue to channel: {}", e);
                                    }
                                }
                            }
                        }
                        crate::rules::PrAction::Skip { ref reason } => {
                            debug!("{}", reason);
                        }
                        crate::rules::PrAction::PostToChannel { message: ref msg } => {
                            let channel_msg =
                                Message::new("midtown", msg.clone(), MessageType::Text);
                            if let Err(e) = state.send_and_broadcast(&channel_msg) {
                                warn!("Failed to post review complete to channel: {}", e);
                            }
                        }
                    }

                    // Record the nudge to prevent spamming
                    let mut tracker = state.pr_issue_tracker.lock().await;
                    tracker.record_nudge(pr_number, PrIssueType::ReviewComplete);
                }
            }

            continue;
        }

        // Check if already assigned for review.
        // This runs AFTER review detection so completed reviews are always detected,
        // but prevents spawning duplicate reviewers for PRs already under review.
        {
            let github_state = state.github_state.lock().await;
            if github_state.is_assigned(pr_number) {
                debug!("PR #{} already assigned for review", pr_number);
                continue;
            }
        }

        // Always spawn a fresh coworker for reviews with an isolated task list.
        // This ensures review sub-tasks don't pollute the shared task list and
        // can't be accidentally claimed by other coworkers.
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        debug!(
            "Spawning isolated coworker to review PR #{}: {}",
            pr_number,
            truncate_str(title, 40)
        );

        // Check max coworkers limit before spawning
        if state.is_at_coworker_limit() {
            debug!(
                "Max coworkers limit ({}) reached, cannot spawn reviewer for PR #{}",
                state.max_coworkers, pr_number
            );
            continue;
        }

        // Spawn with isolated task list and the review prompt included at launch.
        // Passing the prompt directly to spawn() is more reliable than the old
        // approach of spawning without a prompt and nudging via tmux send-keys,
        // which could fail if the Enter key didn't register.
        // Isolated review coworkers are sent on a break when they go idle (no 5-minute wait).
        let review_prompt = format!(
            "First, post a /me status update: `midtown channel post \"/me reviewing PR #{}\"` — then run: /code-review:code-review {}\n\n\
             IMPORTANT: You MUST always post a GitHub comment on the PR, even if no issues are found. \
             If the code-review skill finishes without posting a comment (e.g. because no issues scored above the threshold), \
             post a comment yourself using `gh pr comment {} --body` with the \"no issues found\" format from the skill.",
            pr_number, pr_number, pr_number
        );

        match state.coworkers.spawn(false, Some(&review_prompt), true) {
            Ok(new_coworker) => {
                state.broadcast_coworker_update(&new_coworker, "running", None);

                // Record the assignment in persistent state
                {
                    let mut github_state = state.github_state.lock().await;
                    github_state.assign_reviewer(pr_number, &new_coworker);
                    if let Err(e) =
                        crate::github_state::save_state_for_repo(&state.repo_name, &github_state)
                    {
                        warn!("Failed to save github-state.json: {}", e);
                    }
                }

                info!(
                    "Spawned {} to review PR #{}: {}",
                    new_coworker,
                    pr_number,
                    truncate_str(title, 40)
                );

                // Post to channel (the coworker's /me status update will set
                // the tmux tab name via the channel handler).
                let channel_msg = Message::new(
                    "midtown",
                    daemon_messages::called_in_reviewer(
                        &new_coworker,
                        pr_number,
                        config::get_personality(),
                    ),
                    MessageType::Text,
                );
                if let Err(e) = state.send_and_broadcast(&channel_msg) {
                    warn!("Failed to post call-in message to channel: {}", e);
                }

                reviews_spawned += 1;
            }
            Err(e) => {
                debug!("Could not spawn new reviewer for PR #{}: {}", pr_number, e);
            }
        }
    }

    if reviews_spawned > 0 {
        info!(
            "Spawned {} reviewers for PRs needing review",
            reviews_spawned
        );
    }
}

/// Handle webhook-triggered reviewer spawning for a newly opened or ready-for-review PR.
///
/// Waits the review delay, then fetches the PR data and spawns a reviewer if eligible.
/// This bypasses the polling interval so reviewers start sooner after a PR is opened.
async fn handle_webhook_review_spawn(state: &DaemonState, pr_number: u64) {
    info!(
        "Webhook: PR #{} needs review, waiting {}s before spawning reviewer",
        pr_number, PR_REVIEW_DELAY_SECS
    );
    tokio::time::sleep(Duration::from_secs(PR_REVIEW_DELAY_SECS)).await;

    // Fetch this specific PR's data
    let output = match tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state",
        ])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!(
                "Webhook: Failed to fetch PR #{} for review spawn: {}",
                pr_number, e
            );
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Webhook: gh pr view #{} failed: {}", pr_number, stderr);
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pr: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(pr) => pr,
        Err(e) => {
            warn!("Webhook: Failed to parse PR #{} JSON: {}", pr_number, e);
            return;
        }
    };

    // Check the PR is still open
    let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("");
    if pr_state != "OPEN" {
        debug!(
            "Webhook: PR #{} is no longer open (state={}), skipping review",
            pr_number, pr_state
        );
        return;
    }

    // Reuse the existing spawn logic (handles draft check, assignment dedup, etc.)
    spawn_reviewers_for_prs(state, &[pr]).await;
}

/// Check if a PR has a review comment from a Claude coworker.
///
/// First checks the in-memory cache (populated from persistent state on startup).
/// If not cached, makes API calls to check formal reviews and comments, then
/// caches positive results permanently (review status is monotonic).
///
/// Checks both formal reviews (`.reviews[].body`) and comments (`.comments[].body`)
/// since coworkers use comments for reviews (they share one GitHub user and can't
/// approve their own PRs).
fn pr_has_claude_review(pr_number: u64, state: &DaemonState) -> bool {
    // Fast path: check in-memory cache
    {
        let cache = state.reviewed_prs_cache.read().unwrap();
        if cache.contains(&pr_number) {
            debug!(
                "PR #{} has cached Claude review (skipping API call)",
                pr_number
            );
            return true;
        }
    }

    // Slow path: check via API calls
    let has_review = pr_has_claude_review_uncached(pr_number);

    // Cache positive results (review status is monotonic)
    if has_review {
        let mut cache = state.reviewed_prs_cache.write().unwrap();
        cache.insert(pr_number);
    }

    has_review
}

/// Uncached check for Claude review on a PR (makes GitHub API calls).
///
/// Fetches both reviews and comments in a single API call to reduce GitHub API usage.
fn pr_has_claude_review_uncached(pr_number: u64) -> bool {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "reviews,comments",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let json: serde_json::Value = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    debug!("Failed to parse review JSON for PR #{}: {}", pr_number, e);
                    return false;
                }
            };

            // Check formal reviews
            if let Some(reviews) = json.get("reviews").and_then(|v| v.as_array()) {
                for review in reviews {
                    if let Some(body) = review.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            // Check comments (where coworkers post their reviews)
            if let Some(comments) = json.get("comments").and_then(|v| v.as_array()) {
                for comment in comments {
                    if let Some(body) = comment.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            false
        }
        _ => {
            debug!("Failed to fetch reviews/comments for PR #{}", pr_number);
            false
        }
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    state: Arc<DaemonState>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();

        tokio::select! {
            // Read next request line
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        debug!("Client disconnected");
                        break;
                    }
                    Ok(_) => {
                        let response = handle_request(&line, &state).await;
                        let response_json = match serde_json::to_string(&response) {
                            Ok(json) => json,
                            Err(e) => {
                                error!("Failed to serialize response: {}", e);
                                continue;
                            }
                        };

                        if let Err(e) = writer.write_all(response_json.as_bytes()).await {
                            warn!("Failed to write response: {}", e);
                            break;
                        }
                        if let Err(e) = writer.write_all(b"\n").await {
                            warn!("Failed to write newline: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Read error: {}", e);
                        break;
                    }
                }
            }

            // Handle shutdown signal
            _ = shutdown_rx.recv() => {
                debug!("Connection handler received shutdown signal");
                break;
            }
        }
    }
}

/// Process a JSON-RPC request and return a response.
async fn handle_request(line: &str, state: &DaemonState) -> Response {
    // Parse the request
    let request: Request = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(e) => {
            warn!("Failed to parse request: {}", e);
            return Response::error(RequestId::Null, RpcError::parse_error());
        }
    };

    debug!("Received request: method={}", request.method);

    // Dispatch based on method
    match request.method.as_str() {
        "ping" => Response::success(request.id, serde_json::json!("pong")),

        "version" => Response::success(
            request.id,
            serde_json::json!({
                "name": "midtown",
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),

        "shutdown" => {
            info!("Shutdown requested via RPC");
            Response::success(request.id, serde_json::json!({"status": "shutting_down"}))
        }

        "coworker.spawn" => {
            let params = request.params.as_ref();
            let resume = params
                .and_then(|p| p.get("resume"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let prompt = params
                .and_then(|p| p.get("prompt"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            handle_coworker_spawn(request.id, state, resume, prompt)
        }

        "coworker.break" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_break(request.id, name, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.nudge" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");

            match (name, message) {
                (Some(name), Some(message)) => {
                    handle_coworker_nudge(request.id, from, name, message, state)
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.asking" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let question = params
                .and_then(|p| p.get("question"))
                .and_then(|v| v.as_str());

            match (name, question) {
                (Some(name), Some(question)) => {
                    handle_coworker_asking(request.id, name, question, state)
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "status" => handle_status(request.id, state),

        "kanban.data" => handle_kanban_data(request.id, state),

        "channel.post" => {
            let params = request.params.as_ref();
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");

            match message {
                Some(msg) => handle_channel_post(request.id, from, msg, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "channel.read" => {
            let all = request
                .params
                .as_ref()
                .and_then(|p| p.get("all"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            handle_channel_read(request.id, all, state)
        }

        "reminder.create" => {
            let params = request.params.as_ref();
            let trigger = params
                .and_then(|p| p.get("trigger"))
                .and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());

            match (trigger, message) {
                (Some("all-work-merged"), Some(msg)) => {
                    handle_reminder_create(request.id, msg, state)
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "reminder.list" => handle_reminder_list(request.id, state),

        "reminder.cancel" => {
            let id = request
                .params
                .as_ref()
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str());

            match id {
                Some(id) => handle_reminder_cancel(request.id, id, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = spawn_for_pending_tasks(&snap, state).await;
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

/// Handle coworker.spawn RPC method.
fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
) -> Response {
    // Check dev coworkers limit (reserve slots for reviewers)
    if state.is_at_dev_limit() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "Dev coworkers limit ({}) reached (reserving {} slots for reviewers). Adjust with MIDTOWN_MAX_COWORKERS or max_coworkers in config.toml",
                    state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
                    REVIEW_HEADROOM
                ),
            ),
        );
    }

    // Pass prompt to spawn() - it handles waiting and nudging internally
    // Use shared task list (not isolated) for manual spawns
    match state.coworkers.spawn(resume, prompt.as_deref(), false) {
        Ok(name) => {
            info!("Spawned coworker: {}", name);
            state.broadcast_coworker_update(&name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Called in coworker: {}", name),
                    "coworkers": [{
                        "name": name,
                        "status": "running",
                        "current_task": null,
                        "started_at": chrono::Utc::now().to_rfc3339(),
                    }]
                }),
            )
        }
        Err(e) => {
            error!("Failed to spawn coworker: {}", e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.break RPC method.
fn handle_coworker_break(id: RequestId, name: &str, state: &DaemonState) -> Response {
    state.broadcast_coworker_update(name, "stopped", None);
    match state.coworkers.shutdown(name) {
        Ok(()) => {
            info!("Sent coworker on a break: {}", name);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Sent {} on a break", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to send coworker {} on a break: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.list RPC method.
fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(_task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    Some((owner.to_lowercase(), subject))
                }
            })
            .collect();

    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "coworkers": coworkers,
        }),
    )
}

/// Handle coworker.nudge RPC method.
///
/// Sends the nudge directly to the coworker's tmux window without posting to the channel,
/// to avoid the chat monitor seeing the @mention and creating a duplicate nudge.
fn handle_coworker_nudge(
    id: RequestId,
    _from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    match state.coworkers.nudge(name, message) {
        Ok(()) => {
            info!("Nudged coworker {}: {}", name, message);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Nudged coworker: {}", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to nudge coworker {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.asking RPC method.
///
/// Called when a coworker uses AskUserQuestion tool. This:
/// 1. Posts the question to the channel
/// 2. Nudges the Lead with the question
/// 3. Marks the coworker as waiting for feedback
fn handle_coworker_asking(
    id: RequestId,
    name: &str,
    question: &str,
    state: &DaemonState,
) -> Response {
    // Post question to channel
    let msg = Message::text(name, format!("Question for Lead: {}", question));
    if let Err(e) = state.send_and_broadcast(&msg) {
        error!("Failed to post question to channel: {}", e);
    }

    // Mark the coworker as waiting for feedback in tmux tab.
    // This is a direct call since the question is posted as a text message,
    // not a /me action, so the channel handler won't pick it up.
    if let Err(e) = state
        .coworkers
        .update_status_display(name, Some("waiting for feedback"))
    {
        debug!("Failed to update tmux tab for {}: {}", name, e);
    }

    // Nudge the Lead with the question
    let nudge_message = format!("{} is asking: {}", name, question);
    if let Err(e) = state.coworkers.nudge("Lead", &nudge_message) {
        // Log but don't fail - Lead might not be in a tmux window
        debug!("Failed to nudge Lead: {}", e);
    }

    info!("Coworker {} asking: {}", name, question);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Notified Lead about question from {}", name),
        }),
    )
}

/// Remove shell escaping artifacts from channel messages.
///
/// When Claude Code posts messages via its Bash tool, the LLM often escapes `!`
/// as `\!` (to avoid bash history expansion). Since the Bash tool runs in
/// non-interactive mode where history expansion is disabled, the backslash passes
/// through literally. This function cleans up such artifacts.
fn unescape_shell_artifacts(s: &str) -> String {
    s.replace("\\!", "!")
}

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in their tmux tab name.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
async fn handle_channel_post(
    id: RequestId,
    from: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Clean up shell escaping artifacts (e.g. "\!" from bash history expansion escaping)
    let message = unescape_shell_artifacts(message);

    // Check for /me prefix (IRC-style action)
    let (content, msg_type) = if let Some(action) = message.strip_prefix("/me ") {
        (action.to_string(), MessageType::Action)
    } else {
        (message.to_string(), MessageType::Text)
    };

    let msg = Message::new(from, content.clone(), msg_type.clone());

    match state.send_and_broadcast(&msg) {
        Ok(()) => {
            info!("Channel post from {}: {}", from, message);

            // Track last activity time for coworker (used for silent coworker detection)
            if is_coworker_sender(from) {
                let mut lifecycles = state.coworker_lifecycles.write().await;
                lifecycles
                    .entry(from.to_string())
                    .or_insert_with(|| crate::rules::CoworkerLifecycle {
                        phase: None,
                        last_activity: None,
                    })
                    .last_activity = Some(Instant::now());
            }

            // Update tmux tab for coworkers when they post /me actions
            if msg_type == MessageType::Action {
                // Update the coworker's tmux tab to show their status
                if let Err(e) = state.coworkers.update_status_display(from, Some(&content)) {
                    debug!("Failed to update tmux tab for {}: {}", from, e);
                }
            }

            // Nudge lead when user messages arrive (from web UI or TUI input)
            if from == "user" {
                // Check if user is @mentioning specific coworkers or @all
                let has_coworker_mentions =
                    !extract_mentions(&content).is_empty() || contains_at_all(&content);
                let has_lead_mention = content.to_lowercase().contains("@lead");

                // Route @mentions in user messages directly to coworkers
                route_mentions(state, &msg).await;

                // Only nudge lead if there are no coworker @mentions (regular
                // message for the lead) or if the user also @mentioned the lead.
                // This lets users talk directly to coworkers without the lead
                // acting as a middleman.
                if !has_coworker_mentions || has_lead_mention {
                    let nudge_msg = format!("user: {}", content);
                    info!("Nudging Lead about user message");
                    if let Err(e) = state.coworkers.nudge_lead(&nudge_msg) {
                        warn!("Failed to nudge Lead about user message: {}", e);
                    }
                } else {
                    info!(
                        "Skipping Lead nudge — user message routed directly to mentioned coworker(s)"
                    );
                }
            }

            // Nudge the Lead when a coworker explicitly mentions @lead
            let content_lower = content.to_lowercase();
            if is_coworker_sender(from) && content_lower.contains("@lead") {
                // Use CooldownTracker to avoid duplicate nudges (expires after 1 hour)
                let should_nudge = {
                    let cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.check("lead_mention", &msg.id, Duration::from_secs(3600))
                };

                if should_nudge {
                    // Record that we're nudging for this message
                    {
                        let mut cooldowns = state.cooldowns.lock().unwrap();
                        cooldowns.record("lead_mention", &msg.id);
                    }

                    // Truncate message for nudge (max 100 chars)
                    let summary = if content.len() > 100 {
                        format!("{}...", &content[..97])
                    } else {
                        content.clone()
                    };

                    let nudge_msg = format!("{} mentioned @lead: {}", from, summary);
                    info!("Nudging Lead about @lead mention from {}", from);

                    // Nudge the Lead window
                    if let Err(e) = state.coworkers.nudge_lead(&nudge_msg) {
                        warn!("Failed to nudge Lead about @lead mention: {}", e);
                    }

                    // Send push notification to mobile PWA
                    state.send_push_notification(
                        &format!("@lead from {}", from),
                        &summary,
                        "mention",
                    );
                }
            }

            // Send bell notification and push notification for @user mentions
            if content_lower.contains("@user") && from != "user" {
                info!("Bell notification: @user mentioned by {}", from);
                if let Err(e) = state.coworkers.notify_user() {
                    warn!("Failed to send bell notification for @user mention: {}", e);
                }
                let summary = if content.len() > 100 {
                    format!("{}...", &content[..97])
                } else {
                    content.clone()
                };
                state.send_push_notification(&format!("@user from {}", from), &summary, "mention");
            }

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": "Message posted to channel",
                }),
            )
        }
        Err(e) => {
            error!("Failed to post to channel: {}", e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle channel.read RPC method.
fn handle_channel_read(id: RequestId, all: bool, state: &DaemonState) -> Response {
    let messages = if all {
        // Read all messages
        match state.channel.read_all() {
            Ok(msgs) => msgs,
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else {
        // Read recent messages (last 20)
        match state.channel.read_all() {
            Ok(msgs) => msgs.into_iter().rev().take(20).rev().collect(),
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    };

    let messages_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "from": m.from,
                "message": m.content,
                "timestamp": m.timestamp.to_rfc3339(),
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "messages": messages_json,
        }),
    )
}

/// Handle reminder.create RPC method.
fn handle_reminder_create(id: RequestId, message: &str, state: &DaemonState) -> Response {
    let mut reminder_state = state.reminder_state.lock().unwrap();
    let reminder_id = reminder_state.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        message.to_string(),
    );

    let path = crate::paths::reminders_file_for_repo(&state.repo_name);
    if let Err(e) = reminder_state.save(&path) {
        error!("Failed to save reminders: {}", e);
    }

    let confirmation = format!(
        "Reminder set (id: {}): I'll notify you when all tasks are completed and all PRs are merged. Message: \"{}\"",
        reminder_id, message
    );
    info!("{}", confirmation);
    Response::success(id, serde_json::json!({ "message": confirmation }))
}

/// Handle reminder.list RPC method.
fn handle_reminder_list(id: RequestId, state: &DaemonState) -> Response {
    let reminder_state = state.reminder_state.lock().unwrap();
    let active = reminder_state.active();

    if active.is_empty() {
        return Response::success(id, serde_json::json!({ "message": "No active reminders." }));
    }

    let lines: Vec<String> = active
        .iter()
        .map(|r| {
            format!(
                "  {} [{}] \"{}\" (created {})",
                r.id,
                r.trigger,
                r.message,
                r.created_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect();

    let output = format!("Active reminders:\n{}", lines.join("\n"));
    Response::success(id, serde_json::json!({ "message": output }))
}

/// Handle reminder.cancel RPC method.
fn handle_reminder_cancel(id: RequestId, reminder_id: &str, state: &DaemonState) -> Response {
    let mut reminder_state = state.reminder_state.lock().unwrap();
    if reminder_state.cancel(reminder_id) {
        let path = crate::paths::reminders_file_for_repo(&state.repo_name);
        if let Err(e) = reminder_state.save(&path) {
            error!("Failed to save reminders: {}", e);
        }
        let msg = format!("Reminder {} cancelled.", reminder_id);
        info!("{}", msg);
        Response::success(id, serde_json::json!({ "message": msg }))
    } else {
        Response::error(
            id,
            RpcError::new(-32602, format!("Reminder '{}' not found", reminder_id)),
        )
    }
}

/// Check all active reminders and fire any whose conditions are met.
fn check_and_fire_reminders(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    // Convert snapshot HashSet to Vec for evaluate_trigger compatibility
    let open_pr_coworkers: Vec<String> = snap.coworkers_with_open_prs.iter().cloned().collect();

    let mut reminder_state = state.reminder_state.lock().unwrap();
    let mut fired_any = false;
    let mut effects = Vec::new();

    for reminder in &mut reminder_state.reminders {
        if reminder.fired {
            continue;
        }
        if crate::reminders::evaluate_trigger(&reminder.trigger, &open_pr_coworkers) {
            info!(
                "Reminder {} fired (trigger: {}): {}",
                reminder.id, reminder.trigger, reminder.message
            );
            effects.push(Effect::PostToChannel {
                sender: "system".to_string(),
                message: format!(
                    "\u{23f0} Reminder ({}): {}",
                    reminder.trigger, reminder.message
                ),
            });
            reminder.fired = true;
            fired_any = true;
        }
    }

    if fired_any {
        let path = crate::paths::reminders_file_for_repo(&snap.repo_name);
        if let Err(e) = reminder_state.save(&path) {
            error!("Failed to save reminders after firing: {}", e);
        }
    }

    effects
}

/// Handle status RPC method.
fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
    // This is the source of truth for what each coworker is working on
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(_task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    Some((owner.to_lowercase(), subject))
                }
            })
            .collect();

    // Get coworkers with their details, looking up current task from task storage
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect();

    // Get open PRs from GitHub via gh CLI
    let pull_requests = get_open_prs();

    // Get all tasks from Claude Code task storage (all statuses)
    let tasks = get_all_tasks();
    let pending_count = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .count();

    // Get recently merged PRs
    let merged_prs = get_merged_prs();

    // Get recent channel activity
    let recent_activity = get_recent_channel_activity();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "max_coworkers": state.max_coworkers,
            "max_dev_coworkers": state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
            "pending_tasks": pending_count,
            "socket_path": state.socket_path.to_string_lossy(),
            "coworkers": coworkers,
            "tasks": tasks,
            "pull_requests": pull_requests,
            "merged_prs": merged_prs,
            "recent_activity": recent_activity,
        }),
    )
}

/// Get open PRs from GitHub using gh CLI.
fn get_open_prs() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,state,isDraft,reviewDecision",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                prs.into_iter()
                    .map(|pr| {
                        let status = format_pr_status(&pr);
                        serde_json::json!({
                            "number": pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
                            "title": pr.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                            "author": pr.get("author").and_then(|a| a.get("login")).and_then(|l| l.as_str()).unwrap_or("unknown"),
                            "status": status,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => {
            debug!("Failed to get PRs from gh CLI");
            Vec::new()
        }
    }
}

/// Format PR status from gh CLI JSON.
fn format_pr_status(pr: &serde_json::Value) -> String {
    let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
    if is_draft {
        return "draft".to_string();
    }

    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");

    match review_decision {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes requested".to_string(),
        "REVIEW_REQUIRED" => "awaiting review".to_string(),
        _ => "open".to_string(),
    }
}

/// Get all tasks from Claude Code task storage with their status.
fn get_all_tasks() -> Vec<serde_json::Value> {
    crate::tasks::read_tasks()
        .into_iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            serde_json::json!({
                "id": task.id,
                "subject": task.subject,
                "status": status,
                "assignee": task.owner,
            })
        })
        .collect()
}

/// Handle kanban.data RPC method - returns PR data for the kanban board.
///
/// Returns open PRs with author, reviewer, CI status, and timestamps,
/// plus recently merged PRs for the Done column.
fn handle_kanban_data(id: RequestId, state: &DaemonState) -> Response {
    // Get reviewer assignments from GitHubState (best-effort via try_lock)
    let reviewer_assignments: HashMap<u64, crate::github_state::PrReviewerAssignment> = state
        .github_state
        .try_lock()
        .map(|gs| gs.active_assignments())
        .unwrap_or_default();

    // Fetch PRs and repo metadata from all repos in the project.
    // We resolve nameWithOwner once per repo and reuse it for both the
    // batched GraphQL PR query and the repo metadata response.
    let is_multi_repo = state.all_repo_paths.len() > 1;
    let mut prs = Vec::new();
    let mut merged_prs = Vec::new();
    let mut repos = Vec::new();
    for repo_path in &state.all_repo_paths {
        let repo_label = if is_multi_repo {
            repo_path.file_name().and_then(|s| s.to_str())
        } else {
            None
        };

        // Resolve owner/name via cache (only hits API on first call per repo path)
        let full_name = state.get_repo_full_name(repo_path);

        let label = repo_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        repos.push(serde_json::json!({
            "label": label,
            "full_name": full_name,
        }));

        let (open, merged) =
            fetch_kanban_all_prs(&reviewer_assignments, &full_name, repo_path, repo_label);
        prs.extend(open);
        merged_prs.extend(merged);
    }

    Response::success(
        id,
        serde_json::json!({
            "prs": prs,
            "merged_prs": merged_prs,
            "repos": repos,
        }),
    )
}

/// GraphQL query that fetches both open and recently merged PRs in a single call.
///
/// This replaces two separate `gh pr list` CLI calls with one GraphQL request,
/// cutting API usage in half for the kanban board.
const KANBAN_GRAPHQL_QUERY: &str = r#"
query($owner: String!, $repo: String!) {
  repository(owner: $owner, name: $repo) {
    openPrs: pullRequests(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number
        title
        author { login }
        createdAt
        body
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup {
                contexts(first: 100) {
                  nodes {
                    __typename
                    ... on CheckRun {
                      status
                      conclusion
                    }
                    ... on StatusContext {
                      state
                    }
                  }
                }
              }
            }
          }
        }
        comments(first: 100) {
          nodes {
            body
            createdAt
          }
        }
      }
    }
    mergedPrs: pullRequests(states: MERGED, first: 10, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        mergedAt
      }
    }
  }
}
"#;

/// Fetch both open and merged PRs for a repo using a single GraphQL call.
///
/// `name_with_owner` should be `"owner/repo"` (e.g. `"anthropics/midtown"`).
/// Returns `(open_prs, merged_prs)` formatted for the kanban board.
/// Falls back to empty vectors on failure.
fn fetch_kanban_all_prs(
    reviewer_assignments: &HashMap<u64, crate::github_state::PrReviewerAssignment>,
    name_with_owner: &str,
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let parts: Vec<&str> = name_with_owner.splitn(2, '/').collect();
    if parts.len() != 2 {
        debug!("Unexpected nameWithOwner format: {}", name_with_owner);
        return (Vec::new(), Vec::new());
    }
    let (owner, repo_name) = (parts[0], parts[1]);

    // Execute the batched GraphQL query
    let graphql_output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={}", owner),
            "-F",
            &format!("repo={}", repo_name),
            "-f",
            &format!("query={}", KANBAN_GRAPHQL_QUERY),
        ])
        .output();

    let data = match graphql_output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(v) => v,
                Err(_) => {
                    debug!("Failed to parse kanban GraphQL response");
                    return (Vec::new(), Vec::new());
                }
            }
        }
        _ => {
            debug!("Failed to execute kanban GraphQL query");
            return (Vec::new(), Vec::new());
        }
    };

    let repository = match data.pointer("/data/repository") {
        Some(r) => r,
        None => {
            debug!("No repository data in kanban GraphQL response");
            return (Vec::new(), Vec::new());
        }
    };

    // Process open PRs
    let open_prs = repository
        .pointer("/openPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;

                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let github_author = pr
                        .pointer("/author/login")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let author = extract_coworker_from_pr_body(body).unwrap_or(github_author);

                    let created_at = pr.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");

                    // Extract CI status from the last commit's statusCheckRollup
                    let check_contexts: Vec<serde_json::Value> = pr
                        .pointer("/commits/nodes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.last())
                        .and_then(|node| node.pointer("/commit/statusCheckRollup/contexts/nodes"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ci_status = kanban_ci_status(&check_contexts);

                    // Extract reviewer from comments
                    let comments: Vec<serde_json::Value> = pr
                        .pointer("/comments/nodes")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let (comment_reviewer, reviewed_at) =
                        extract_reviewer_from_pr_comments(&comments);

                    // Use comment reviewer, or fall back to assigned reviewer.
                    // Track whether the review was actually posted (vs just assigned).
                    let (reviewer, reviewer_assigned_at, review_posted) =
                        if let Some(reviewer) = comment_reviewer {
                            (Some(reviewer), reviewed_at, true)
                        } else if let Some(assignment) = reviewer_assignments.get(&number) {
                            (
                                Some(assignment.reviewer.clone()),
                                Some(assignment.assigned_at.to_rfc3339()),
                                false,
                            )
                        } else {
                            (None, None, false)
                        };

                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "author": author,
                        "created_at": created_at,
                        "ci_status": ci_status,
                        "reviewer": reviewer,
                        "reviewed_at": reviewer_assigned_at,
                        "review_posted": review_posted,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // Process merged PRs
    let merged_prs = repository
        .pointer("/mergedPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;
                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let merged_at = pr
                        .get("mergedAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "merged_at": merged_at,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    (open_prs, merged_prs)
}

/// Extract coworker name from PR body frontmatter (<!-- midtown: name -->).
fn extract_coworker_from_pr_body(body: &str) -> Option<String> {
    let marker = "midtown:";
    let marker_pos = body.find(marker)?;
    let before = &body[..marker_pos];
    if !before.contains("<!--") {
        return None;
    }
    let after_marker = &body[marker_pos + marker.len()..];
    let end = after_marker.find("-->")?;
    let name = after_marker[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract reviewer name and timestamp from PR comments.
fn extract_reviewer_from_pr_comments(
    comments: &[serde_json::Value],
) -> (Option<String>, Option<String>) {
    for comment in comments {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if !body.contains("Code Review") && !body.contains("Code review") {
            continue;
        }

        // Try frontmatter first
        let reviewer = extract_coworker_from_pr_body(body).or_else(|| {
            // Fall back to "Code Review by {name}" header
            for line in body.lines() {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if let Some(rest) = trimmed
                    .strip_prefix("Code Review by ")
                    .or_else(|| trimmed.strip_prefix("Code review by "))
                {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        });

        if let Some(name) = reviewer {
            let created_at = comment
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return (Some(name), created_at);
        }
    }
    (None, None)
}

/// Compute CI status string from statusCheckRollup array.
fn kanban_ci_status(checks: &[serde_json::Value]) -> &'static str {
    if checks.is_empty() {
        return "unknown";
    }

    let mut has_running = false;
    let mut has_failed = false;
    let mut has_passed = false;

    for check in checks {
        let status = check.get("status").and_then(|v| v.as_str());
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        let state = check.get("state").and_then(|v| v.as_str());

        if let Some(status) = status {
            match status {
                "IN_PROGRESS" | "QUEUED" | "WAITING" | "PENDING" => has_running = true,
                "COMPLETED" => match conclusion {
                    Some("SUCCESS") => has_passed = true,
                    Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => has_failed = true,
                    _ => {}
                },
                _ => {}
            }
        }

        if let Some(state) = state {
            match state {
                "PENDING" => has_running = true,
                "SUCCESS" => has_passed = true,
                "FAILURE" | "ERROR" => has_failed = true,
                _ => {}
            }
        }
    }

    if has_failed {
        "failed"
    } else if has_running {
        "running"
    } else if has_passed {
        "passed"
    } else {
        "unknown"
    }
}

/// Get recently merged PRs from GitHub using gh CLI.
fn get_merged_prs() -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "10",
            "--json",
            "number,title,mergedAt",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str::<Vec<serde_json::Value>>(&stdout).unwrap_or_default()
        }
        _ => {
            debug!("Failed to get merged PRs from gh CLI");
            Vec::new()
        }
    }
}

/// Get recent channel activity.
fn get_recent_channel_activity() -> Vec<serde_json::Value> {
    // Try to read from the default channel location
    let channel_file = crate::paths::channel_file_for_repo("default");

    if !channel_file.exists() {
        return Vec::new();
    }

    // Read the last few messages from the channel
    match std::fs::read_to_string(&channel_file) {
        Ok(content) => {
            let messages: Vec<serde_json::Value> = content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            // Get the last 5 messages, most recent last
            messages
                .into_iter()
                .rev()
                .take(5)
                .map(|msg| {
                    serde_json::json!({
                        "timestamp": msg.get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(|t| {
                                // Format timestamp for display (just time portion)
                                if t.len() > 11 {
                                    t[11..16].to_string()
                                } else {
                                    t.to_string()
                                }
                            })
                            .unwrap_or_default(),
                        "from": msg.get("from").and_then(|f| f.as_str()).unwrap_or("unknown"),
                        "summary": truncate_message(
                            msg.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                            60
                        ),
                    })
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Auto-nudge helpers for PR activity
// ============================================================================

/// Add an eyes reaction to a GitHub comment to indicate it was received.
///
/// Uses the GitHub Reactions API via `gh api` to add a 👀 reaction to the
/// comment that triggered a coworker nudge or spawn.
async fn add_eyes_reaction(repo_full_name: &str, comment_node: &crate::webhook::CommentNode) {
    let endpoint = match comment_node {
        crate::webhook::CommentNode::IssueComment(id) => {
            format!("/repos/{}/issues/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::ReviewComment(id) => {
            format!("/repos/{}/pulls/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::Review { .. } => {
            // GitHub API does not support reactions on pull request reviews
            // (only on issue comments and review comments).
            debug!("Skipping eyes reaction: GitHub API does not support reactions on reviews");
            return;
        }
    };

    let result = tokio::process::Command::new("gh")
        .args(["api", &endpoint, "-f", "content=eyes", "--silent"])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            debug!("Added eyes reaction to {}", endpoint);
        }
        Ok(output) => {
            debug!(
                "Failed to add eyes reaction to {}: {}",
                endpoint,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            debug!("Failed to run gh api for eyes reaction: {}", e);
        }
    }
}

/// Async version of `get_pr_owner_coworker` that doesn't block the Tokio runtime.
async fn get_pr_owner_coworker_async(pr_number: u64) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    coworker_from_branch(&branch)
}

/// Handle nudging a PR owner when a comment/review is posted on their PR.
///
/// This is called from the webhook event loop when a `PrActivity` is present.
/// It resolves the PR owner (from webhook data or async lookup), checks cooldowns,
/// and either nudges an active coworker or spawns an inactive one.
async fn handle_pr_comment_nudge(state: &DaemonState, activity: crate::webhook::PrActivity) {
    let pr_number = activity.pr_number;

    // Resolve the PR owner: use webhook data if available, otherwise look up async
    let owner = match activity.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

    // Check cooldown to avoid spamming
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
            debug!(
                "PR #{} review comment nudge on cooldown, skipping",
                pr_number
            );
            return;
        }
    }

    let nudge_msg = format!(
        "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
        pr_number, activity.actor
    );

    // Decide action using pure decision function
    let is_active = state.coworkers.get(&owner).is_some();
    let action = crate::rules::decide_pr_comment_action(
        &owner,
        &activity.actor,
        is_active,
        state.is_at_dev_limit(),
        &nudge_msg,
    );

    let success = match action {
        crate::rules::PrAction::NudgeOwner {
            owner: ref o,
            message: ref msg,
        } => match state.coworkers.nudge(o, msg) {
            Ok(()) => {
                info!(
                    "Nudged {} about review comment on PR #{} from {}",
                    o, pr_number, activity.actor
                );
                true
            }
            Err(e) => {
                warn!("Failed to nudge {} about PR #{}: {}", o, pr_number, e);
                false
            }
        },
        crate::rules::PrAction::SpawnOwner {
            owner: ref o,
            message: ref msg,
        } => {
            info!(
                "PR #{} owner {} is not active, spawning to address review feedback",
                pr_number, o
            );
            match state
                .spawn_coworker(o, true, Some(msg.as_str()), false)
                .await
            {
                Ok(_) => {
                    info!(
                        "Spawned {} to address review feedback on PR #{}",
                        o, pr_number
                    );
                    let call_msg = Message::text(
                        "daemon",
                        format!(
                            "Called in {} to address review feedback on PR #{}",
                            o, pr_number
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&call_msg) {
                        warn!("Failed to post call-in message: {}", e);
                    }
                    true
                }
                Err(e) => {
                    warn!(
                        "Failed to spawn {} for PR #{} review feedback: {}",
                        o, pr_number, e
                    );
                    false
                }
            }
        }
        crate::rules::PrAction::PostToChannel { message: ref msg } => {
            let channel_msg = Message::new("midtown", msg.clone(), MessageType::Text);
            if let Err(e) = state.send_and_broadcast(&channel_msg) {
                warn!("Failed to post PR comment to channel: {}", e);
            }
            true
        }
        crate::rules::PrAction::Skip { ref reason } => {
            debug!("{}", reason);
            false
        }
    };

    // Record the nudge to prevent spamming
    if success {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.record_nudge(pr_number, PrIssueType::ReviewComment);
    }

    // Add eyes reaction to the comment to provide visual feedback that it was received
    if success
        && let (Some(ref node), Some(ref repo)) = (activity.comment_node, activity.repo_full_name)
    {
        add_eyes_reaction(repo, node).await;
    }
}

// ============================================================================
// Orphan task recovery
// ============================================================================

/// Check for orphaned tasks and auto-recover coworkers.
///
/// An orphaned task is one that is `in_progress` but the owning coworker
/// is no longer active (no tmux window). If the coworker's worktree still
/// exists, we respawn them and nudge them to resume work.
///
/// Rate limiting: Only spawns ONE coworker per tick with a cooldown between
/// spawns to prevent window flashing from spawn storms.
async fn check_and_recover_orphans(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    // Check cooldown - skip if we spawned too recently
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("orphan_spawn", "global", ORPHAN_SPAWN_COOLDOWN) {
            debug!("Orphan recovery cooldown active");
            return vec![];
        }
    }

    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Decide which orphan (if any) to recover using pure decision function
    let recovery = crate::rules::decide_orphan_recovery(
        &snap.in_progress_tasks,
        &snap.active_names,
        snap.is_at_dev_limit,
    );

    let Some(recovery) = recovery else {
        return vec![];
    };

    // Check per-coworker spawn failure cooldown to prevent infinite retry loops
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("spawn_failure", &recovery.owner, SPAWN_FAILURE_COOLDOWN) {
            debug!(
                "Spawn failure cooldown active for {} — skipping orphan recovery for task #{}",
                recovery.owner, recovery.task_id
            );
            return vec![];
        }
    }

    info!(
        "Detected orphaned task #{} owned by {} - attempting recovery",
        recovery.task_id, recovery.owner
    );

    let prompt = format!(
        "You've been assigned task #{}: {}. Your previous session was interrupted but your worktree and branch are still intact. Check your git status and get started!",
        recovery.task_id, recovery.task_subject
    );

    // Spawn fresh (no --continue) — the coworker keeps the same name so they
    // retain their worktree and branch. This is the same path as normal task
    // assignment, just reusing the previous coworker name.
    match state
        .spawn_coworker(&recovery.owner, false, Some(&prompt), false)
        .await
    {
        Ok(_) => {
            info!("Respawned coworker {} successfully", recovery.owner);
            vec![
                Effect::BroadcastCoworkerUpdate {
                    name: recovery.owner.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::RecordCooldown {
                    category: "orphan_spawn".to_string(),
                    key: "global".to_string(),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "♻️ Recovered coworker {} for orphaned task #{}",
                        recovery.owner, recovery.task_id
                    ),
                },
            ]
        }
        Err(e) => {
            warn!(
                "Could not respawn {} for orphaned task #{}: {} - resetting task to pending (cooldown {}s)",
                recovery.owner,
                recovery.task_id,
                e,
                SPAWN_FAILURE_COOLDOWN.as_secs()
            );
            vec![
                Effect::RecordCooldown {
                    category: "spawn_failure".to_string(),
                    key: recovery.owner.clone(),
                },
                Effect::ResetTaskToPending {
                    task_id: recovery.task_id.clone(),
                    repo_name: snap.repo_name.clone(),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "🔄 Task #{} reset to pending - {} could not be respawned (backing off for {}s)",
                        recovery.task_id,
                        recovery.owner,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                },
            ]
        }
    }
}

/// Nudge coworkers that were discovered from tmux on daemon startup.
///
/// After a daemon restart, existing coworkers are found in tmux but they may
/// be stuck waiting for input or idle. This function checks if each discovered
/// coworker has an assigned task (in_progress with them as owner) or a reviewer
/// assignment (in github-state.json), and nudges them to continue.
///
/// This runs once at startup, with a short delay to let coworkers settle.
async fn nudge_discovered_coworkers(state: &DaemonState) {
    let discovered = state.coworkers.take_discovered_on_startup();
    if discovered.is_empty() {
        return;
    }

    info!(
        "Checking {} discovered coworker(s) for tasks to resume",
        discovered.len()
    );

    // Small delay to let things settle after daemon startup
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Get in_progress tasks with owners
    let in_progress = crate::tasks::get_in_progress_tasks_with_subjects();

    // Build a map of owner -> (task_id, task_subject)
    let mut owner_tasks: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for (task_id, task_subject, owner) in &in_progress {
        let owner_lower = owner.trim().trim_matches('"').to_lowercase();
        if !owner_lower.is_empty() {
            owner_tasks.insert(owner_lower, (task_id.clone(), task_subject.clone()));
        }
    }

    // Check reviewer assignments from github-state.json
    let reviewer_prs: std::collections::HashMap<String, u64> = {
        let github_state = state.github_state.lock().await;
        discovered
            .iter()
            .filter_map(|name| {
                github_state
                    .pr_for_reviewer(name)
                    .map(|pr| (name.to_lowercase(), pr))
            })
            .collect()
    };

    for name in &discovered {
        let name_lower = name.to_lowercase();

        // Check for an in_progress task owned by this coworker
        if let Some((task_id, task_subject)) = owner_tasks.get(&name_lower) {
            let prompt = format!(
                "Resume task #{}: {}. The daemon was restarted and discovered you still running. Check your git status and continue where you left off.",
                task_id, task_subject
            );

            info!(
                "Nudging discovered coworker {} to resume task #{}",
                name, task_id
            );

            if let Err(e) = state.coworkers.nudge(name, &prompt) {
                warn!("Failed to nudge discovered coworker {}: {}", name, e);
            }

            // Post recovery message to channel
            let msg = Message::text(
                "midtown",
                format!(
                    "♻️ Nudged discovered coworker {} to resume task #{}",
                    name, task_id
                ),
            );
            if let Err(e) = state.send_and_broadcast(&msg) {
                warn!("Failed to post discovery nudge message: {}", e);
            }
        } else if let Some(pr_number) = reviewer_prs.get(&name_lower) {
            // Coworker was assigned to review a PR
            let prompt = format!(
                "Resume reviewing PR #{}. The daemon was restarted and discovered you still running. Continue your code review where you left off.\n\n\
                 IMPORTANT: You MUST always post a GitHub comment on the PR, even if no issues are found. \
                 If the code-review skill finishes without posting a comment, \
                 post a comment yourself using `gh pr comment {} --body` with the \"no issues found\" format from the skill.",
                pr_number, pr_number
            );

            info!(
                "Nudging discovered coworker {} to resume review of PR #{}",
                name, pr_number
            );

            if let Err(e) = state.coworkers.nudge(name, &prompt) {
                warn!("Failed to nudge discovered reviewer {}: {}", name, e);
            }

            let msg = Message::text(
                "midtown",
                format!(
                    "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                    name, pr_number
                ),
            );
            if let Err(e) = state.send_and_broadcast(&msg) {
                warn!("Failed to post discovery nudge message: {}", e);
            }
        } else {
            debug!(
                "Discovered coworker {} has no assigned task or review - skipping nudge",
                name
            );
        }

        // Small delay between nudges to avoid overwhelming tmux
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Detect and kill duplicate task workers.
///
/// When multiple coworkers end up working on the same task (e.g., due to race
/// conditions in task claiming), this function detects the duplicates and kills
/// all but the earliest-started worker. This prevents wasted effort and duplicate PRs.
///
/// The function:
/// 1. Gets all in_progress tasks with their owners
/// 2. Groups tasks by task ID to find duplicates
/// 3. For tasks with multiple workers, keeps the one that started earliest
/// 4. Shuts down the duplicate workers with an explanatory message
fn check_for_duplicate_task_workers(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Build a map of task_id -> list of owners
    let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        // Skip empty owners or Lead
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }
        task_workers
            .entry(task_id.clone())
            .or_default()
            .push(owner.clone());
    }

    let mut effects = Vec::new();

    // Find tasks with multiple workers and determine who to kill
    for (task_id, workers) in task_workers {
        if workers.len() <= 1 {
            continue;
        }

        // Get the task subject for logging
        let task_subject = snap
            .in_progress_tasks
            .iter()
            .find(|(id, _, _)| id == &task_id)
            .map(|(_, s, _)| s.as_str())
            .unwrap_or("unknown");

        info!(
            "Detected {} duplicate workers on task #{} ({}): {:?}",
            workers.len(),
            task_id,
            task_subject,
            workers
        );

        // Sort workers by start time (earliest first)
        // Workers not found in active list go to the end (will be killed)
        let mut workers_with_times: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = workers
            .into_iter()
            .map(|name| {
                let start_time = snap.coworker_start_times.get(&name.to_lowercase()).copied();
                (name, start_time)
            })
            .collect();

        workers_with_times.sort_by(|a, b| {
            match (&a.1, &b.1) {
                (Some(t1), Some(t2)) => t1.cmp(t2),          // Earlier time first
                (Some(_), None) => std::cmp::Ordering::Less, // Known time beats unknown
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        // Keep the first (earliest) worker, kill the rest
        let (keeper, keeper_time) = workers_with_times[0].clone();
        info!(
            "Keeping {} (started {:?}) for task #{}",
            keeper, keeper_time, task_id
        );

        for (duplicate, dup_time) in workers_with_times.into_iter().skip(1) {
            warn!(
                "Killing duplicate worker {} (started {:?}) for task #{} - {} is already working on it",
                duplicate, dup_time, task_id, keeper
            );

            effects.push(Effect::BroadcastCoworkerUpdate {
                name: duplicate.clone(),
                status: "stopped".to_string(),
                current_task: None,
            });
            effects.push(Effect::ShutdownCoworker {
                name: duplicate.clone(),
                message: String::new(),
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "🔪 Killed duplicate worker {} on task #{} ({}) - {} started earlier",
                    duplicate, task_id, task_subject, keeper
                ),
            });
        }
    }

    effects
}

// ============================================================================
// Pending task auto-spawn
// ============================================================================

/// Spawn coworkers for pending tasks.
///
/// Clean up orphaned worktrees that have no active coworker.
///
/// Worktrees with no commits beyond the base branch are deleted.
/// Worktrees with unmerged commits are flagged to the Lead via channel.
fn cleanup_orphaned_worktrees(state: &DaemonState) {
    let flagged = state.coworkers.cleanup_orphaned_worktrees();

    let mut tracker = state.orphan_tracker.write().unwrap();

    // Prune entries for worktrees that are no longer flagged
    tracker.prune(&flagged);

    // Track newly flagged worktrees and collect those due for a warning
    let due_for_warning: Vec<_> = flagged
        .into_iter()
        .filter(|name| {
            tracker.track(name.clone());
            tracker.should_warn(name)
        })
        .collect();

    if due_for_warning.is_empty() {
        return;
    }

    // Record warnings
    for name in &due_for_warning {
        tracker.record_warn(name);
    }
    drop(tracker);

    // Notify @lead about orphaned worktrees with unmerged commits
    let names_list = due_for_warning.join(", ");
    let msg = Message::system(format!(
        "⚠️ @lead Orphaned worktrees with unmerged commits: {}. \
         Please investigate and decide whether to merge or delete these branches.",
        names_list
    ));
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!("Failed to send orphan flag message: {}", e);
    }
}

/// Handles two cases:
/// 1. Pending tasks with owners - spawn/nudge the assigned coworker if not running
/// 2. Pending tasks without owners - spawn a new coworker, assign the task, and nudge
async fn spawn_for_pending_tasks(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    debug!(
        "Task assignment state: active={}",
        snap.running_coworkers.len()
    );

    let mut effects = Vec::new();

    // Case 1: Pending tasks with owners assigned but coworker not running
    let pending_with_owners = &snap.pending_tasks_with_owners;
    for (task_id, task_subject, owner) in pending_with_owners.iter() {
        // Check nudge cooldown for this task
        let task_key = format!("pending-{}", task_id);
        let on_nudge_cooldown = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check("task_nudge", &task_key, Duration::from_secs(300))
        };

        // Decide action using pure decision function
        let action = crate::rules::decide_pending_task_action(
            task_id,
            task_subject,
            owner,
            &snap.active_names,
            snap.is_at_dev_limit,
            on_nudge_cooldown,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                // Nudge inline — cooldown recording depends on nudge success
                let nudge_msg = format!("You have pending task #{}: {}. Get started!", tid, subj);
                if let Err(e) = state.coworkers.nudge(o, &nudge_msg) {
                    debug!("Failed to nudge {} about pending task #{}: {}", o, tid, e);
                } else {
                    info!("Nudged {} about pending task #{}", o, tid);
                    let mut cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.record("task_nudge", &task_key);
                }
            }
            crate::rules::PendingTaskAction::SpawnOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                info!(
                    "Pending task #{} is assigned to {} but coworker not running - spawning",
                    tid, o
                );
                // Spawn inline — post-spawn effects depend on spawn result
                let prompt = format!("You've been assigned task #{}: {}. Get started!", tid, subj);
                match state.spawn_coworker(o, true, Some(&prompt), false).await {
                    Ok(_) => {
                        info!("Spawned coworker {} for pending task #{}", o, tid);
                        effects.push(Effect::BroadcastCoworkerUpdate {
                            name: o.clone(),
                            status: "running".to_string(),
                            current_task: None,
                        });
                        effects.push(Effect::PostToChannel {
                            sender: "midtown".to_string(),
                            message: daemon_messages::called_in_pending_task(
                                o,
                                &tid.to_string(),
                                config::get_personality(),
                            ),
                        });
                    }
                    Err(e) => {
                        debug!("Could not spawn {} for pending task #{}: {}", o, tid, e);
                    }
                }
            }
            crate::rules::PendingTaskAction::Skip { ref reason } => {
                debug!("{}", reason);
            }
        }
    }

    // Case 2: Pending tasks without owners - assign ownership atomically, then spawn
    let pending_unowned = &snap.pending_tasks_without_owners;
    // All tasks from snapshot for relationship lookups (blockedBy, PR owner search)
    let all_tasks = &snap.all_tasks;
    // Track PR# → coworker and task_id → coworker assignments made during this loop iteration.
    // This prevents assigning different coworkers to sub-tasks of the same PR review
    // when multiple sub-tasks are processed in the same tick.
    let mut pr_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut task_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for task in pending_unowned.iter() {
        // Check dev coworkers limit before spawning (reserve slots for reviewers)
        if snap.is_at_dev_limit {
            debug!(
                "Dev coworkers limit reached, deferring unowned task #{}",
                task.id
            );
            break;
        }

        // Step 1: Determine the coworker name by checking multiple grouping strategies.
        // Priority: in-memory PR map → in-memory blockedBy map → disk PR owner →
        //           blockedBy relationship → new coworker name
        let grouped_name: Option<String> = 'resolve: {
            // Strategy A: Extract PR number from subject or description
            if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
                // Check in-memory map first (handles same-tick assignments)
                if let Some(name) = pr_coworker_map.get(&pr_num) {
                    info!(
                        "Task #{} references PR #{} - assigning to in-memory owner {}",
                        task.id, pr_num, name
                    );
                    break 'resolve Some(name.clone());
                }
                // Check disk for previously assigned PR tasks
                if let Some(existing_owner) =
                    crate::tasks::find_pr_owner_in_tasks(&pr_num, all_tasks)
                {
                    info!(
                        "Task #{} references PR #{} - assigning to existing owner {}",
                        task.id, pr_num, existing_owner
                    );
                    break 'resolve Some(existing_owner);
                }
            }

            // Strategy B: Check blockedBy relationships
            // If this task is blocked by a task that was assigned in this loop, use that owner
            for blocked_by_id in &task.blocked_by {
                if let Some(name) = task_coworker_map.get(blocked_by_id) {
                    info!(
                        "Task #{} blocked by #{} - assigning to same owner {}",
                        task.id, blocked_by_id, name
                    );
                    break 'resolve Some(name.clone());
                }
            }
            // Check disk for blockedBy owners
            if let Some(owner) = crate::tasks::find_owner_via_blocked_by(task, all_tasks) {
                info!(
                    "Task #{} blocked by owned task - assigning to {}",
                    task.id, owner
                );
                break 'resolve Some(owner);
            }

            None
        };

        // Step 1b: Use grouped name if found, otherwise allocate a fresh coworker.
        // We always spawn fresh rather than reusing idle coworkers — idle coworkers
        // get shut down by the idle check loop, keeping the lifecycle simple:
        // spawn → work → PR → idle → shutdown.
        let coworker_name = if let Some(name) = grouped_name {
            name
        } else {
            let Some(name) = state.coworkers.next_available_name() else {
                debug!("No available coworker slots for unowned task #{}", task.id);
                break;
            };
            debug!("Task #{}: allocated fresh coworker name {}", task.id, name,);
            name
        };

        // Check if this coworker is already running (grouped to an active coworker)
        let already_running = snap.active_names.contains(&coworker_name.to_lowercase());

        // Step 2: Atomically assign task ownership BEFORE spawning
        // This prevents race conditions where multiple coworkers could claim the same task
        if let Err(e) = crate::tasks::update_task_owner(&task.id, &coworker_name) {
            warn!(
                "Failed to assign task #{} to {}: {}",
                task.id, coworker_name, e
            );
            continue;
        }

        info!(
            "Assigned task #{} to {} (pre-spawn, already_running={})",
            task.id, coworker_name, already_running
        );

        // Record this assignment in in-memory maps for same-tick grouping
        task_coworker_map.insert(task.id.clone(), coworker_name.clone());
        if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
            pr_coworker_map.insert(pr_num, coworker_name.clone());
        }

        // Build the prompt message
        let prompt = format!(
            "You've been assigned task #{}: {}. Get started!",
            task.id, task.subject
        );

        if already_running {
            // Step 3a: Coworker is already running (grouped task) — nudge about new assignment
            // Nudge inline — channel post depends on nudge success
            match state.coworkers.nudge(&coworker_name, &prompt) {
                Ok(()) => {
                    info!(
                        "Nudged running coworker {} with grouped task #{}",
                        coworker_name, task.id
                    );
                    effects.push(Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: daemon_messages::called_in_assigned_task(
                            &coworker_name,
                            &task.id.to_string(),
                            &task.subject,
                            config::get_personality(),
                        ),
                    });
                }
                Err(e) => {
                    warn!(
                        "Failed to nudge idle coworker {} for task #{}: {}",
                        coworker_name, task.id, e
                    );
                }
            }
        } else {
            // Step 3b: Spawn a new coworker with the pre-assigned name and prompt
            // Spawn inline — post-spawn effects depend on spawn result
            match state
                .spawn_coworker(&coworker_name, false, Some(&prompt), false)
                .await
            {
                Ok(_) => {
                    info!(
                        "Spawned coworker {} for pre-assigned task #{}",
                        coworker_name, task.id
                    );
                    effects.push(Effect::BroadcastCoworkerUpdate {
                        name: coworker_name.clone(),
                        status: "running".to_string(),
                        current_task: None,
                    });
                    effects.push(Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: daemon_messages::called_in_assigned_task(
                            &coworker_name,
                            &task.id.to_string(),
                            &task.subject,
                            config::get_personality(),
                        ),
                    });
                }
                Err(e) => {
                    // Spawn failed but task is already assigned - that's okay,
                    // the next daemon tick will see the assigned task and try to spawn again
                    warn!(
                        "Failed to spawn {} for pre-assigned task #{}: {}",
                        coworker_name, task.id, e
                    );
                }
            }
        }
    }

    effects
}

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// Per-coworker decision helpers for unit tests. The batch `decide_*` functions
// in `rules.rs` handle the full coworker set; these single-coworker variants
// make individual test cases easier to write.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{
        UsageLimitDecision, UsageLimitExpiryDecision, decide_usage_limit_detection,
        decide_usage_limit_expiry,
    };

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
        assert_eq!(PrIssueType::ReviewComplete.to_string(), "review complete");
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
        assert_eq!(get_issue_action(PrIssueType::Approved), "ready to merge!");
        assert_eq!(
            get_issue_action(PrIssueType::NeedsReview),
            "calling in reviewer"
        );
        assert_eq!(
            get_issue_action(PrIssueType::ReviewComplete),
            "review is complete — please address feedback and merge if appropriate"
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
    }

    #[test]
    fn test_webhook_mentions_should_be_extracted() {
        // Webhook messages from "github" contain @mentions that should be routed.
        // The chat monitor skips "github" messages for loop protection, so the
        // webhook handler must call route_mentions directly.
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

    // Duplicate task worker detection tests
    #[test]
    fn test_duplicate_worker_sorting_by_start_time() {
        use chrono::{Duration, Utc};

        // Create workers with different start times
        let now = Utc::now();
        let earlier = now - Duration::minutes(5);
        let later = now + Duration::minutes(5);

        let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
            ("later_worker".to_string(), Some(later)),
            ("earlier_worker".to_string(), Some(earlier)),
            ("now_worker".to_string(), Some(now)),
        ];

        // Sort by start time (earliest first) - same logic as check_for_duplicate_task_workers
        workers.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(t1), Some(t2)) => t1.cmp(t2),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        // Earliest worker should be first (the keeper)
        assert_eq!(workers[0].0, "earlier_worker");
        assert_eq!(workers[1].0, "now_worker");
        assert_eq!(workers[2].0, "later_worker");
    }

    #[test]
    fn test_duplicate_worker_sorting_with_unknown_times() {
        use chrono::Utc;

        let now = Utc::now();

        // Workers with some unknown start times
        let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
            ("unknown_worker".to_string(), None),
            ("known_worker".to_string(), Some(now)),
            ("another_unknown".to_string(), None),
        ];

        // Sort by start time - known times beat unknown
        workers.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(t1), Some(t2)) => t1.cmp(t2),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        // Known worker should be first (the keeper), unknowns at the end
        assert_eq!(workers[0].0, "known_worker");
        // Unknown workers are equal, so their order is preserved (stable sort)
        assert!(workers[1].1.is_none());
        assert!(workers[2].1.is_none());
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
    fn test_extract_coworker_from_pr_body() {
        assert_eq!(
            extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
            Some("york".to_string())
        );
        assert_eq!(
            extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
            Some("park".to_string())
        );
        assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
        assert_eq!(extract_coworker_from_pr_body(""), None);
    }

    #[test]
    fn test_extract_reviewer_from_pr_comments() {
        let comments = vec![serde_json::json!({
            "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
            "createdAt": "2026-01-29T10:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("lexington".to_string()));
        assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

        let comments = vec![serde_json::json!({
            "body": "## Code Review by vernon\nLGTM",
            "createdAt": "2026-01-29T11:00:00Z"
        })];
        let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("vernon".to_string()));

        let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
        assert_eq!(reviewer, None);
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
    fn test_kanban_ci_status() {
        assert_eq!(kanban_ci_status(&[]), "unknown");
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})
            ]),
            "passed"
        );
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})
            ]),
            "failed"
        );
        assert_eq!(
            kanban_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
            "running"
        );
    }

    // Interactive prompt detection tests
    #[test]
    fn test_detect_interactive_prompt_plan_approval() {
        let pane = r#"
  ╭──────────────────────────────────────────────────────────╮
  │ Plan: Add authentication endpoint                        │
  │                                                          │
  │  1. Yes, and bypass permissions                          │
  │  2. Yes, clear context and bypass permissions            │
  │  3. No, and tell Claude what to do differently           │
  ╰──────────────────────────────────────────────────────────╯
        "#;
        assert_eq!(
            crate::rules::detect_interactive_prompt(pane),
            Some("plan approval")
        );
    }

    #[test]
    fn test_detect_interactive_prompt_permission_request() {
        let pane = "Claude wants to run: cargo test\n  Allow once  Allow always  Deny";
        assert_eq!(
            crate::rules::detect_interactive_prompt(pane),
            Some("permission request")
        );
    }

    #[test]
    fn test_detect_interactive_prompt_confirmation() {
        let pane = "This will modify 15 files. Would you like to proceed?";
        assert_eq!(
            crate::rules::detect_interactive_prompt(pane),
            Some("confirmation prompt")
        );
    }

    #[test]
    fn test_detect_interactive_prompt_question() {
        let pane = "Which approach do you prefer?\n  Select an option\n  > Option A\n    Option B";
        assert_eq!(
            crate::rules::detect_interactive_prompt(pane),
            Some("question prompt")
        );
    }

    #[test]
    fn test_detect_interactive_prompt_none() {
        // Normal working output — no prompt
        let pane = "Reading file src/main.rs\nEditing src/daemon.rs\n";
        assert_eq!(crate::rules::detect_interactive_prompt(pane), None);
    }

    #[test]
    fn test_detect_interactive_prompt_empty() {
        assert_eq!(crate::rules::detect_interactive_prompt(""), None);
    }

    // Usage limit detection tests
    #[test]
    fn test_parse_usage_limit_duration_minutes() {
        let pane = "You've hit your usage limit. Try again in 15 minutes.";
        assert_eq!(
            crate::rules::parse_usage_limit_duration(pane).as_secs(),
            15 * 60
        );
    }

    #[test]
    fn test_parse_usage_limit_duration_hours() {
        let pane = "Rate limited. Try again in 2 hours.";
        assert_eq!(
            crate::rules::parse_usage_limit_duration(pane).as_secs(),
            2 * 3600
        );
    }

    #[test]
    fn test_parse_usage_limit_duration_seconds() {
        let pane = "Too many requests. Try again in 30 seconds.";
        assert_eq!(crate::rules::parse_usage_limit_duration(pane).as_secs(), 30);
    }

    #[test]
    fn test_parse_usage_limit_duration_after_keyword() {
        let pane = "Limit reached. Available after 10 minutes.";
        assert_eq!(
            crate::rules::parse_usage_limit_duration(pane).as_secs(),
            10 * 60
        );
    }

    #[test]
    fn test_parse_usage_limit_duration_default() {
        // No parseable duration — should default to 15 minutes
        let pane = "Usage limit reached. Please wait.";
        assert_eq!(
            crate::rules::parse_usage_limit_duration(pane).as_secs(),
            15 * 60
        );
    }

    #[test]
    fn test_parse_usage_limit_duration_case_insensitive() {
        let pane = "USAGE LIMIT REACHED. TRY AGAIN IN 20 MINUTES.";
        assert_eq!(
            crate::rules::parse_usage_limit_duration(pane).as_secs(),
            20 * 60
        );
    }

    #[test]
    fn test_usage_limit_patterns_detect_common_messages() {
        let messages = vec![
            "You've hit your usage limit for claude-3-5-sonnet",
            "Usage limit reached for this model",
            "rate_limit_error: too many requests",
            "Your usage limit resets in 15 minutes",
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

    // ─── Usage Limit Detection Tests ───────────────────────────────────

    #[test]
    fn test_usage_limit_detected_in_pane() {
        let mut panes = std::collections::HashMap::new();
        panes.insert("park".to_string(), "Working on task...\n".to_string());
        panes.insert(
            "broadway".to_string(),
            "Usage limit reached. Try again in 15 minutes.\n".to_string(),
        );

        let decision = decide_usage_limit_detection(&panes);
        assert_eq!(
            decision,
            UsageLimitDecision::Detected {
                coworker: "broadway".to_string()
            }
        );
    }

    #[test]
    fn test_usage_limit_none_detected() {
        let mut panes = std::collections::HashMap::new();
        panes.insert(
            "park".to_string(),
            "Running tests... all pass\n".to_string(),
        );
        panes.insert("broadway".to_string(), "Editing src/main.rs\n".to_string());

        let decision = decide_usage_limit_detection(&panes);
        assert_eq!(decision, UsageLimitDecision::NoneDetected);
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

    // Shell artifact unescaping tests
    #[test]
    fn test_unescape_shell_artifacts_exclamation() {
        assert_eq!(
            unescape_shell_artifacts("Game time\\! Let's go"),
            "Game time! Let's go"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_multiple_exclamations() {
        assert_eq!(
            unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
            "Wow! Amazing! Done!"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_no_escapes() {
        assert_eq!(
            unescape_shell_artifacts("Normal message with ! marks"),
            "Normal message with ! marks"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_preserves_other_backslashes() {
        assert_eq!(
            unescape_shell_artifacts("path\\to\\file and \\!"),
            "path\\to\\file and !"
        );
    }

    // ---- Lead typing indicator grace period tests ----

    #[test]
    fn test_determine_lead_working_pane_changed() {
        // When the pane just changed, always working regardless of last_activity
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        assert!(determine_lead_working(true, None, now, grace));
        assert!(determine_lead_working(true, Some(now), now, grace));
    }

    #[test]
    fn test_determine_lead_working_within_grace_period() {
        // Pane hasn't changed, but last activity was recent — still working
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(10);
        assert!(determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }

    #[test]
    fn test_determine_lead_working_grace_period_expired() {
        // Pane hasn't changed and grace period has elapsed — not working
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(31);
        assert!(!determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }

    #[test]
    fn test_determine_lead_working_no_activity_ever() {
        // No pane change and no prior activity — not working
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        assert!(!determine_lead_working(false, None, now, grace));
    }

    #[test]
    fn test_determine_lead_working_exactly_at_grace_boundary() {
        // At exactly the grace period boundary — not working (uses < not <=)
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(30);
        assert!(!determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }
}
