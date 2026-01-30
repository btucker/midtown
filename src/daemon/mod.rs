//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod constants;
mod helpers;
mod trackers;

use constants::*;
pub use constants::{
    DEFAULT_MAX_COWORKERS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, MAX_CONCURRENT_REVIEWS, PR_NUDGE_COOLDOWN_SECS,
    PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, PR_REVIEW_DELAY_SECS,
};
use helpers::*;
pub use trackers::{PrIssueTracker, PrIssueType, PrReviewTracker};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
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
    /// Tracks message IDs that have already triggered a nudge to Lead (to avoid duplicates)
    nudged_messages: std::sync::RwLock<HashSet<String>>,
    /// Tracks when each coworker became idle (no in_progress tasks)
    idle_since: RwLock<HashMap<String, Instant>>,
    /// Tracks when each coworker was first detected as interrupted
    interrupted_since: RwLock<HashMap<String, Instant>>,
    /// Tracks prompts we've already nudged the lead about (coworker_name -> prompt_fingerprint)
    /// to avoid spamming the same prompt repeatedly
    prompted_nudged: RwLock<HashMap<String, String>>,
    /// Tracker to avoid spamming the same PR issues
    pr_issue_tracker: Mutex<PrIssueTracker>,
    /// Tracker for PRs assigned for review
    pr_review_tracker: Mutex<PrReviewTracker>,
    /// Repository name (primary repo)
    repo_name: String,
    /// Paths to all repos in the project (primary + additional)
    all_repo_paths: Vec<PathBuf>,
    /// Unified cooldown tracker for orphan spawning and task nudge rate limiting.
    cooldowns: std::sync::Mutex<crate::rules::CooldownTracker>,
    /// Tracks orphaned worktrees that have already been warned about (dedup across poll cycles)
    warned_orphans: std::sync::RwLock<HashSet<String>>,
    /// Persistent GitHub state (PR reviewer assignments, etc.)
    github_state: Mutex<crate::github_state::GitHubState>,
    /// Broadcast sender for pushing channel messages to WebSocket clients
    web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
    /// Hash of last captured lead pane content (for typing indicator change detection)
    last_lead_pane_hash: std::sync::Mutex<u64>,
    /// Whether the lead is currently working (for typing indicator dedup)
    lead_working: std::sync::Mutex<bool>,
    /// When the lead's pane last showed activity (for typing grace period)
    last_lead_activity: std::sync::Mutex<Option<Instant>>,
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
}

impl DaemonState {
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
    ) -> crate::Result<Self> {
        // Load persistent GitHub state
        let github_state =
            crate::github_state::load_state_for_repo(&repo_name).unwrap_or_else(|e| {
                warn!("Failed to load github-state.json: {}, using defaults", e);
                crate::github_state::GitHubState::default()
            });

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
            nudged_messages: std::sync::RwLock::new(HashSet::new()),
            idle_since: RwLock::new(HashMap::new()),
            interrupted_since: RwLock::new(HashMap::new()),
            prompted_nudged: RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::new()),
            pr_review_tracker: Mutex::new(PrReviewTracker::new()),
            repo_name,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            warned_orphans: std::sync::RwLock::new(HashSet::new()),
            github_state: Mutex::new(github_state),
            web_updates_tx,
            max_coworkers,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            last_lead_pane_hash: std::sync::Mutex::new(0),
            lead_working: std::sync::Mutex::new(false),
            last_lead_activity: std::sync::Mutex::new(None),
            reminder_state: std::sync::Mutex::new(reminder_state),
        })
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

                // Route @mentions in webhook messages directly (chat monitor skips
                // "github" sender for loop protection, so we handle it here)
                route_mentions(&state, &webhook_event.message);
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
                );
            }

            // Periodically check for idle coworkers and shut them down
            _ = idle_check_interval.tick() => {
                // Sync internal state with actual tmux windows first
                if let Err(e) = state.coworkers.sync_with_tmux() {
                    warn!("Failed to sync coworker state with tmux: {}", e);
                }
                check_and_shutdown_idle_coworkers(&state).await;
                check_and_nudge_interrupted_coworkers(&state).await;
                check_and_nudge_prompted_coworkers(&state).await;
                check_for_usage_limits(&state).await;
                maybe_nudge_usage_limit_expiry(&state).await;
            }

            // Check lead pane activity for typing indicator
            _ = lead_typing_interval.tick() => {
                check_lead_typing(&state).await;
            }

            // Periodic orphan check, duplicate detection, and worktree cleanup
            _ = orphan_check_interval.tick() => {
                check_for_duplicate_task_workers(&state).await;
                check_and_recover_orphans(&state).await;
                spawn_for_pending_tasks(&state);
                cleanup_orphaned_worktrees(&state);
                check_and_fire_reminders(&state);
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

    let prev_hash = {
        let mut h = state.last_lead_pane_hash.lock().unwrap();
        let prev = *h;
        *h = new_hash;
        prev
    };

    let pane_changed = prev_hash != 0 && new_hash != prev_hash;
    let now = Instant::now();

    // Update last-activity timestamp when pane content changes
    if pane_changed {
        *state.last_lead_activity.lock().unwrap() = Some(now);
    }

    // Determine working state using grace period: stay "working" until
    // no pane changes have occurred for LEAD_TYPING_GRACE_PERIOD.
    let is_working = determine_lead_working(
        pane_changed,
        *state.last_lead_activity.lock().unwrap(),
        now,
        LEAD_TYPING_GRACE_PERIOD,
    );

    // Only broadcast on state transitions
    let prev_working = {
        let mut w = state.lead_working.lock().unwrap();
        let prev = *w;
        *w = is_working;
        prev
    };

    if is_working != prev_working {
        web::broadcast_lead_typing(tx, is_working);
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
async fn check_and_shutdown_idle_coworkers(state: &DaemonState) {
    // Get list of active coworkers with their data (need started_at for lifetime check)
    let active_coworkers = state.coworkers.list();

    if active_coworkers.is_empty() {
        return;
    }

    // Get in_progress tasks to determine who is busy
    let busy_coworkers: HashSet<String> =
        get_busy_coworkers(&state.repo_name).into_iter().collect();

    // Get coworkers with open PRs - they should NEVER be sent on a break
    let coworkers_with_open_prs: HashSet<String> =
        get_coworkers_with_open_prs().into_iter().collect();

    // Get coworkers actively assigned to review PRs - they should not be considered idle.
    // Check BOTH in-memory and persistent state. The in-memory tracker can be empty after
    // a daemon restart, and the persistent state survives restarts.
    let active_reviewers = {
        let tracker = state.pr_review_tracker.lock().await;
        let mut reviewers = tracker.active_reviewers();
        // Also include reviewers from persistent state (survives daemon restarts)
        let github_state = state.github_state.lock().await;
        for reviewer_name in github_state.assigned_reviewers() {
            reviewers.insert(reviewer_name.to_string());
        }
        reviewers
    };

    // Build coworker snapshots for the pure decision function
    let snapshots: Vec<crate::rules::CoworkerSnapshot> = active_coworkers
        .iter()
        .map(|cw| crate::rules::CoworkerSnapshot {
            name: cw.name.clone(),
            started_at: cw.started_at,
            isolated_tasks: cw.isolated_tasks,
        })
        .collect();

    // Pure decision: who should be shut down?
    let to_shutdown = {
        let mut idle_since = state.idle_since.write().await;
        crate::rules::decide_idle_shutdowns(
            &snapshots,
            &busy_coworkers,
            &coworkers_with_open_prs,
            &active_reviewers,
            &mut idle_since,
            Instant::now(),
            chrono::Utc::now(),
            IDLE_BREAK_DURATION,
            MINIMUM_COWORKER_LIFETIME,
        )
    };

    // Shutdown idle coworkers (outside the lock)
    for decision in to_shutdown {
        let name = &decision.name;

        // For isolated coworkers (reviewers), verify the review was actually posted
        let (should_shutdown, shutdown_msg) = if decision.is_isolated {
            // Look up the PR this reviewer was assigned to
            // Try in-memory tracker first
            let pr_number = {
                let tracker = state.pr_review_tracker.lock().await;
                tracker.pr_for_coworker(name)
            };
            let pr_number = match pr_number {
                Some(pr) => Some(pr),
                None => {
                    // Fall back to persistent state
                    let github_state = state.github_state.lock().await;
                    github_state.pr_for_reviewer(name)
                }
            };

            match pr_number {
                Some(pr) => {
                    // Check if review was actually posted
                    if pr_has_claude_review(pr) {
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
                        // Don't shutdown - let them continue working
                        // Post a warning to the channel so the team knows
                        let warning_msg = Message::text(
                            "system",
                            format!(
                                "⚠️ Reviewer {} is idle but hasn't posted review for PR #{} yet",
                                name, pr
                            ),
                        );
                        if let Err(e) = state.send_and_broadcast(&warning_msg) {
                            warn!("Failed to post warning message to channel: {}", e);
                        }
                        (false, String::new())
                    }
                }
                None => {
                    // Can't find PR assignment - shut down with warning
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

        // Post system message to channel
        let msg = Message::text("system", shutdown_msg);
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post break message to channel: {}", e);
        }

        // Shutdown the coworker
        state.broadcast_coworker_update(name, "stopped", None);
        if let Err(e) = state.coworkers.shutdown(name) {
            warn!("Failed to send idle coworker {} on a break: {}", name, e);
        }
    }
}

/// Check for coworkers whose Claude Code session is interrupted and nudge them to continue.
///
/// Captures each active coworker's tmux pane content and checks for interruption
/// indicators ("Interrupted" or "What should Claude do instead?"). If the interrupted
/// state persists for 60 seconds, sends a "continue" nudge to unstick them.
async fn check_and_nudge_interrupted_coworkers(state: &DaemonState) {
    let active_coworkers = state.coworkers.list();
    if active_coworkers.is_empty() {
        return;
    }

    let session_name = state.coworkers.session_name();

    // Build coworker snapshots and capture pane contents
    let mut snapshots = Vec::new();
    let mut pane_contents = HashMap::new();
    for cw in &active_coworkers {
        snapshots.push(crate::rules::CoworkerSnapshot {
            name: cw.name.clone(),
            started_at: cw.started_at,
            isolated_tasks: cw.isolated_tasks,
        });
        let target = format!("{}:{}", session_name, &cw.name);
        if let Some(content) = crate::tmux::capture_pane(&target) {
            pane_contents.insert(cw.name.clone(), content);
        }
    }

    // Pure decision: who should be nudged?
    let to_nudge = {
        let mut interrupted_since = state.interrupted_since.write().await;
        crate::rules::decide_interrupt_nudges(
            &snapshots,
            &pane_contents,
            &mut interrupted_since,
            Instant::now(),
            INTERRUPTED_NUDGE_DURATION,
        )
    };

    for nudge in to_nudge {
        let name = &nudge.name;
        info!(
            "Nudging interrupted coworker: {} (interrupted for 60+ seconds)",
            name
        );

        let msg = Message::text(
            "system",
            format!("🔄 Nudging interrupted coworker: {}", name),
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post interrupted nudge message to channel: {}", e);
        }

        if let Err(e) = state.coworkers.nudge(name, "continue") {
            warn!("Failed to nudge interrupted coworker {}: {}", name, e);
        }
    }
}

// Interactive prompt detection moved to crate::rules::detect_interactive_prompt

/// Detect coworkers waiting on interactive prompts (plan approval, permission dialogs, etc.)
/// and nudge the lead so they can provide guidance.
///
/// Unlike interrupted coworkers (who just need a "continue"), prompted coworkers need a
/// *human decision* — so we alert the lead with context about what's being asked.
async fn check_and_nudge_prompted_coworkers(state: &DaemonState) {
    let active_coworkers = state.coworkers.list();
    if active_coworkers.is_empty() {
        return;
    }

    let session_name = state.coworkers.session_name();

    // Build coworker snapshots and capture pane contents
    let mut snapshots = Vec::new();
    let mut pane_contents = HashMap::new();
    for cw in &active_coworkers {
        snapshots.push(crate::rules::CoworkerSnapshot {
            name: cw.name.clone(),
            started_at: cw.started_at,
            isolated_tasks: cw.isolated_tasks,
        });
        let target = format!("{}:{}", session_name, &cw.name);
        if let Some(content) = crate::tmux::capture_pane(&target) {
            pane_contents.insert(cw.name.clone(), content);
        }
    }

    // Pure decision: which coworkers need lead attention?
    let to_nudge = {
        let mut prompted_nudged = state.prompted_nudged.write().await;
        crate::rules::decide_prompt_nudges(&snapshots, &pane_contents, &mut prompted_nudged)
    };

    for nudge in to_nudge {
        let (name, label) = (&nudge.name, &nudge.label);
        info!("Coworker {} is waiting on a {}, nudging lead", name, label);

        let msg = Message::text(
            "system",
            format!(
                "⚠️ @lead {} is waiting on a {} — check their tmux pane and respond",
                name, label
            ),
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post prompt nudge to channel: {}", e);
        }

        // Also nudge lead directly via tmux
        let nudge_text = format!(
            "{} is waiting on a {} — run: tmux select-window -t {}:{}",
            name, label, session_name, name
        );
        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
            warn!(
                "Failed to nudge lead about prompted coworker {}: {}",
                name, e
            );
        }
    }
}

// Usage limit patterns and parse_usage_limit_duration moved to crate::rules

/// Check all active coworkers' tmux panes for usage/rate limit messages.
/// If detected, schedule a nudge for when the limit expires.
///
/// Usage limits are account-wide, so when one coworker hits it, all of them
/// will be stuck. We detect it from any coworker, parse the expiry, and
/// schedule a single nudge time for everyone.
async fn check_for_usage_limits(state: &DaemonState) {
    // If we already have a nudge scheduled, don't re-detect
    let nudge_already_scheduled = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        nudge_at.is_some()
    };

    if nudge_already_scheduled {
        return;
    }

    let active_coworkers = state.coworkers.list();
    if active_coworkers.is_empty() {
        return;
    }

    let session_name = state.coworkers.session_name();

    // Gather pane contents
    let mut pane_contents: Vec<(String, String)> = Vec::new();
    for cw in &active_coworkers {
        let target = format!("{}:{}", session_name, cw.name);
        if let Some(content) = crate::tmux::capture_pane(&target) {
            pane_contents.push((cw.name.clone(), content));
        }
    }

    // Pure decision: detect usage limit
    let decision =
        crate::rules::decide_usage_limit_detection(&pane_contents, nudge_already_scheduled);

    let detected_coworker = match decision {
        crate::rules::UsageLimitDecision::Detected { coworker } => coworker,
        _ => return,
    };

    // Find the pane content for the detected coworker to parse duration
    let pane_content = pane_contents
        .iter()
        .find(|(name, _)| *name == detected_coworker)
        .map(|(_, content)| content.as_str())
        .unwrap_or("");

    let wait_duration = crate::rules::parse_usage_limit_duration(pane_content);
    let nudge_time = tokio::time::Instant::now() + wait_duration + USAGE_LIMIT_NUDGE_BUFFER;

    // Store the scheduled nudge time
    {
        let mut nudge_at = state.usage_limit_nudge_at.lock().await;
        *nudge_at = Some(nudge_time);
    }

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

    let msg = Message::text(
        "system",
        format!(
            "⏳ Usage limit detected (via {}). All coworkers will be nudged in ~{} when it resets.",
            detected_coworker, human_duration
        ),
    );
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!("Failed to post usage limit message to channel: {}", e);
    }
}

/// Check if a scheduled usage limit nudge is due, and if so, nudge all active coworkers.
async fn maybe_nudge_usage_limit_expiry(state: &DaemonState) {
    // Pure decision: should we nudge?
    let nudge_at_value = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        *nudge_at
    };
    let decision =
        crate::rules::decide_usage_limit_expiry(nudge_at_value, tokio::time::Instant::now());

    if decision != crate::rules::UsageLimitExpiryDecision::NudgeNow {
        return;
    }

    // Clear the scheduled nudge
    {
        let mut nudge_at = state.usage_limit_nudge_at.lock().await;
        *nudge_at = None;
    }

    let active_coworkers = state.coworkers.list();
    if active_coworkers.is_empty() {
        return;
    }

    info!(
        "Usage limit expired — nudging {} active coworkers",
        active_coworkers.len()
    );

    let msg = Message::text(
        "system",
        format!(
            "🔔 Usage limit expired — nudging {} coworkers to resume work",
            active_coworkers.len()
        ),
    );
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!(
            "Failed to post usage limit expiry message to channel: {}",
            e
        );
    }

    for cw in &active_coworkers {
        if let Err(e) = state.coworkers.nudge(&cw.name, "continue") {
            warn!(
                "Failed to nudge coworker {} after usage limit expiry: {}",
                cw.name, e
            );
        }
    }
}

/// Get list of coworker names who have in_progress tasks.
///
/// Takes the repo name explicitly to avoid relying on git detection,
/// which may fail in daemon background processes.
fn get_busy_coworkers(repo_name: &str) -> Vec<String> {
    crate::tasks::get_busy_coworkers_for_repo(repo_name)
}

/// Get list of coworker names who have open PRs.
///
/// A coworker is considered to have an open PR if the PR's branch name
/// starts with the coworker's name (e.g., "lexington/fix-auth").
/// Coworkers with open PRs should NEVER be sent on a break.
fn get_coworkers_with_open_prs() -> Vec<String> {
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
                                // Skip messages from protected senders (loop protection)
                                if SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(&msg.from)) {
                                    continue;
                                }
                                // Route any @mentions in the message
                                route_mentions(&state, &msg);
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
fn route_mentions(state: &DaemonState, msg: &Message) {
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
                match state
                    .coworkers
                    .spawn_with_name(n, true, Some(m.as_str()), false)
                {
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
        let mut review_tracker = state.pr_review_tracker.lock().await;
        review_tracker.cleanup_preserving(&active_coworker_names);
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

    // Clean up persistent reviewer assignments for PRs that are no longer open
    {
        let open_pr_numbers: Vec<u64> = prs
            .iter()
            .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
            .collect();
        let mut github_state = state.github_state.lock().await;
        github_state.cleanup_closed_prs(&open_pr_numbers);
        github_state.cleanup_expired_preserving(&active_coworker_names);
        if let Err(e) = crate::github_state::save_state_for_repo(&state.repo_name, &github_state) {
            warn!("Failed to save github-state.json after cleanup: {}", e);
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

            // Format the nudge message
            let message = format!(
                "PR #{} ({}) - {}: {}",
                pr_number,
                truncate_str(title, 40),
                issue_type,
                get_issue_action(issue_type)
            );

            // Decide action using pure decision function
            use crate::rules::{PrAction, decide_pr_issue_action};
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
                        .coworkers
                        .spawn_with_name(o, true, Some(msg.as_str()), false)
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

    Ok(())
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
        let tracker = state.pr_review_tracker.lock().await;
        tracker.active_count()
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
        if pr_has_claude_review(pr_number) {
            debug!("PR #{} already has a Claude review", pr_number);

            // Before cleaning up the assignment, check if the reviewer is still running.
            // If so, leave the assignment in place so the idle shutdown path can
            // properly send them off with break_review_complete() instead of break_no_pr().
            let reviewer_still_running = {
                let tracker = state.pr_review_tracker.lock().await;
                if let Some(reviewer_name) = tracker.get_reviewer(pr_number) {
                    state.coworkers.get(reviewer_name).is_some()
                } else {
                    // Check persistent state too
                    let github_state = state.github_state.lock().await;
                    if let Some(reviewer_name) = github_state.get_reviewer(pr_number) {
                        state.coworkers.get(reviewer_name).is_some()
                    } else {
                        false
                    }
                }
            };

            if reviewer_still_running {
                debug!(
                    "PR #{} has Claude review but reviewer is still running — keeping assignment",
                    pr_number
                );
            } else {
                // Free the tracker slot — the review completed and the reviewer is gone
                {
                    let mut tracker = state.pr_review_tracker.lock().await;
                    if tracker.is_assigned(pr_number) {
                        debug!(
                            "PR #{} review completed, freeing tracker slot (in-memory)",
                            pr_number
                        );
                        tracker.mark_reviewed(pr_number);
                    }
                }
                // Also clean up persistent state
                {
                    let mut github_state = state.github_state.lock().await;
                    if github_state.is_assigned(pr_number) {
                        debug!(
                            "PR #{} review completed, freeing tracker slot (persistent)",
                            pr_number
                        );
                        github_state.remove_assignment(pr_number);
                        if let Err(e) = crate::github_state::save_state_for_repo(
                            &state.repo_name,
                            &github_state,
                        ) {
                            warn!("Failed to save github-state.json: {}", e);
                        }
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
                            match state.coworkers.spawn_with_name(
                                o,
                                true,
                                Some(msg.as_str()),
                                false,
                            ) {
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

        // Check if already assigned for review (check both in-memory and persistent state).
        // This runs AFTER review detection so completed reviews are always detected,
        // but prevents spawning duplicate reviewers for PRs already under review.
        {
            let tracker = state.pr_review_tracker.lock().await;
            if tracker.is_assigned(pr_number) {
                debug!("PR #{} already assigned for review (in-memory)", pr_number);
                continue;
            }
        }
        {
            let github_state = state.github_state.lock().await;
            if github_state.is_assigned(pr_number) {
                debug!("PR #{} already assigned for review (persistent)", pr_number);
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

                // Record the assignment (in-memory)
                {
                    let mut tracker = state.pr_review_tracker.lock().await;
                    tracker.assign(pr_number, &new_coworker);
                }

                // Persist the assignment to github-state.json
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

                // Set tmux tab to show review status immediately
                let review_status = format!("reviewing PR #{}", pr_number);
                if let Err(e) = state
                    .coworkers
                    .update_status_display(&new_coworker, Some(&review_status))
                {
                    warn!("Failed to set review status for {}: {}", new_coworker, e);
                }

                // Post to channel
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
/// Checks both formal reviews (`.reviews[].body`) and comments (`.comments[].body`)
/// since coworkers use comments for reviews (they share one GitHub user and can't
/// approve their own PRs).
fn pr_has_claude_review(pr_number: u64) -> bool {
    // Check formal reviews first
    let reviews_output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "reviews",
            "-q",
            ".reviews[].body",
        ])
        .output();

    if let Ok(output) = reviews_output
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if text_contains_review_signature(&stdout) {
            return true;
        }
    }

    // Check comments (where coworkers post their reviews)
    let comments_output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "comments",
            "-q",
            ".comments[].body",
        ])
        .output();

    match comments_output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            text_contains_review_signature(&stdout)
        }
        _ => {
            debug!("Failed to check comments for PR #{}", pr_number);
            // Assume no review on error (will try again later)
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
                        let response = handle_request(&line, &state);
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
fn handle_request(line: &str, state: &DaemonState) -> Response {
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
                Some(msg) => handle_channel_post(request.id, from, msg, state),
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

    // Nudge the Lead with the question
    let nudge_message = format!("{} is asking: {}", name, question);
    if let Err(e) = state.coworkers.nudge("Lead", &nudge_message) {
        // Log but don't fail - Lead might not be in a tmux window
        debug!("Failed to nudge Lead: {}", e);
    }

    // Update coworker status to show they're waiting
    if let Err(e) = state
        .coworkers
        .update_status_display(name, Some("waiting for feedback"))
    {
        debug!("Failed to update coworker status: {}", e);
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
fn handle_channel_post(id: RequestId, from: &str, message: &str, state: &DaemonState) -> Response {
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
                route_mentions(state, &msg);

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
                // Use message ID to avoid duplicate nudges
                let should_nudge = {
                    let nudged = state.nudged_messages.read().unwrap();
                    !nudged.contains(&msg.id)
                };

                if should_nudge {
                    // Record that we're nudging for this message
                    {
                        let mut nudged = state.nudged_messages.write().unwrap();
                        nudged.insert(msg.id.clone());
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
fn check_and_fire_reminders(state: &DaemonState) {
    let open_pr_coworkers = get_coworkers_with_open_prs();

    let mut reminder_state = state.reminder_state.lock().unwrap();
    let mut fired_any = false;

    for reminder in &mut reminder_state.reminders {
        if reminder.fired {
            continue;
        }
        if crate::reminders::evaluate_trigger(&reminder.trigger, &open_pr_coworkers) {
            info!(
                "Reminder {} fired (trigger: {}): {}",
                reminder.id, reminder.trigger, reminder.message
            );
            let msg = Message::system(format!(
                "\u{23f0} Reminder ({}): {}",
                reminder.trigger, reminder.message
            ));
            if let Err(e) = state.send_and_broadcast(&msg) {
                error!("Failed to post reminder to channel: {}", e);
            }
            reminder.fired = true;
            fired_any = true;
        }
    }

    if fired_any {
        let path = crate::paths::reminders_file_for_repo(&state.repo_name);
        if let Err(e) = reminder_state.save(&path) {
            error!("Failed to save reminders after firing: {}", e);
        }
    }
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
    // Get reviewer assignments from PrReviewTracker (best-effort via try_lock)
    let reviewer_assignments: HashMap<u64, (String, Instant)> = state
        .pr_review_tracker
        .try_lock()
        .map(|tracker| tracker.active_assignments())
        .unwrap_or_default();

    // Fetch PRs from all repos in the project
    let is_multi_repo = state.all_repo_paths.len() > 1;
    let mut prs = Vec::new();
    let mut merged_prs = Vec::new();
    for repo_path in &state.all_repo_paths {
        // Only include repo label when the project has multiple repos
        let repo_label = if is_multi_repo {
            repo_path.file_name().and_then(|s| s.to_str())
        } else {
            None
        };
        prs.extend(fetch_kanban_prs(
            &reviewer_assignments,
            repo_path,
            repo_label,
        ));
        merged_prs.extend(fetch_kanban_merged_prs(repo_path, repo_label));
    }

    // Build repo metadata for TUI status lines
    let repos: Vec<serde_json::Value> = state
        .all_repo_paths
        .iter()
        .map(|repo_path| {
            let label = repo_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            // Get the full owner/name from gh
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
            serde_json::json!({
                "label": label,
                "full_name": full_name,
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "prs": prs,
            "merged_prs": merged_prs,
            "repos": repos,
        }),
    )
}

/// Fetch open PRs with rich data for the kanban board.
///
/// Called once per repo with the repo's path. The `repo_label` is `Some(name)`
/// only for multi-repo projects (to display a repo badge on kanban cards).
fn fetch_kanban_prs(
    reviewer_assignments: &HashMap<u64, (String, Instant)>,
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,createdAt,body,statusCheckRollup,comments",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                prs.iter()
                    .filter_map(|pr| {
                        let number = pr.get("number").and_then(|v| v.as_u64())?;

                        let title = pr
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        let github_author = pr
                            .get("author")
                            .and_then(|v| v.get("login"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                        let author = extract_coworker_from_pr_body(body).unwrap_or(github_author);

                        let created_at = pr.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");

                        // Parse CI status
                        let ci_status = kanban_ci_status(
                            pr.get("statusCheckRollup")
                                .and_then(|v| v.as_array())
                                .map(|a| a.as_slice())
                                .unwrap_or(&[]),
                        );

                        // Extract reviewer from comments
                        let (comment_reviewer, reviewed_at) = extract_reviewer_from_pr_comments(
                            pr.get("comments")
                                .and_then(|v| v.as_array())
                                .map(|a| a.as_slice())
                                .unwrap_or(&[]),
                        );

                        // Use comment reviewer, or fall back to assigned reviewer
                        let (reviewer, reviewer_assigned_at) = if let Some(reviewer) =
                            comment_reviewer
                        {
                            (Some(reviewer), reviewed_at)
                        } else if let Some((name, instant)) = reviewer_assignments.get(&number) {
                            // Convert Instant to approximate DateTime
                            let elapsed = instant.elapsed();
                            let assigned_at = chrono::Utc::now()
                                - chrono::Duration::seconds(elapsed.as_secs() as i64);
                            (Some(name.clone()), Some(assigned_at.to_rfc3339()))
                        } else {
                            (None, None)
                        };

                        Some(serde_json::json!({
                            "number": number,
                            "title": title,
                            "author": author,
                            "created_at": created_at,
                            "ci_status": ci_status,
                            "reviewer": reviewer,
                            "reviewed_at": reviewer_assigned_at,
                            "repo": repo_label,
                        }))
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => {
            debug!("Failed to fetch kanban PRs from gh CLI");
            Vec::new()
        }
    }
}

/// Fetch recently merged PRs for the kanban Done column.
///
/// Called once per repo. The `repo_label` is `Some(name)` only for multi-repo projects.
fn fetch_kanban_merged_prs(
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> Vec<serde_json::Value> {
    let output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "number,title,mergedAt",
            "--limit",
            "10",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            serde_json::from_str::<Vec<serde_json::Value>>(&stdout)
                .unwrap_or_default()
                .into_iter()
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
        }
        _ => {
            debug!("Failed to fetch merged PRs from gh CLI");
            Vec::new()
        }
    }
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
                .coworkers
                .spawn_with_name(o, true, Some(msg.as_str()), false)
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
async fn check_and_recover_orphans(state: &DaemonState) {
    // Check cooldown - skip if we spawned too recently
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("orphan_spawn", "global", ORPHAN_SPAWN_COOLDOWN) {
            debug!("Orphan recovery cooldown active");
            return;
        }
    }

    // Get in_progress tasks with their owners
    let in_progress = get_in_progress_tasks_with_owners();

    if in_progress.is_empty() {
        return;
    }

    // Get list of currently running coworkers (excludes Stopping/Stopped)
    let active_names: std::collections::HashSet<String> = state
        .coworkers
        .list_running()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // Decide which orphan (if any) to recover using pure decision function
    let recovery =
        crate::rules::decide_orphan_recovery(&in_progress, &active_names, state.is_at_dev_limit());

    let Some(recovery) = recovery else {
        return;
    };

    info!(
        "Detected orphaned task #{} owned by {} - attempting recovery",
        recovery.task_id, recovery.owner
    );

    let prompt = format!(
        "Resume task #{}: {}. You were working on this task before your session was interrupted. Check your git status and continue where you left off.",
        recovery.task_id, recovery.task_subject
    );

    match state
        .coworkers
        .spawn_with_name(&recovery.owner, true, Some(&prompt), false)
    {
        Ok(_) => {
            info!("Respawned coworker {} successfully", recovery.owner);
            state.broadcast_coworker_update(&recovery.owner, "running", None);

            // Update cooldown for rate limiting
            {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record("orphan_spawn", "global");
            }

            let recovery_msg = Message::text(
                "midtown",
                format!(
                    "♻️ Recovered coworker {} for orphaned task #{}",
                    recovery.owner, recovery.task_id
                ),
            );
            if let Err(e) = state.send_and_broadcast(&recovery_msg) {
                warn!("Failed to post recovery message: {}", e);
            }
        }
        Err(e) => {
            warn!(
                "Could not respawn {} for orphaned task #{}: {} - resetting task to pending",
                recovery.owner, recovery.task_id, e
            );

            if let Err(reset_err) =
                crate::tasks::reset_task_to_pending_for_repo(&recovery.task_id, &state.repo_name)
            {
                warn!(
                    "Failed to reset orphaned task #{} to pending: {}",
                    recovery.task_id, reset_err
                );
            } else {
                info!(
                    "Reset orphaned task #{} to pending (original owner {} could not be respawned)",
                    recovery.task_id, recovery.owner
                );

                let msg = Message::text(
                    "midtown",
                    format!(
                        "🔄 Task #{} reset to pending - {} could not be called back in",
                        recovery.task_id, recovery.owner
                    ),
                );
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post task reset message: {}", e);
                }
            }
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
    let in_progress = get_in_progress_tasks_with_owners();

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
async fn check_for_duplicate_task_workers(state: &DaemonState) {
    // Get in_progress tasks with their owners
    let in_progress = get_in_progress_tasks_with_owners();

    if in_progress.is_empty() {
        return;
    }

    // Build a map of task_id -> list of owners
    let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _subject, owner) in &in_progress {
        // Skip empty owners or Lead
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }
        task_workers
            .entry(task_id.clone())
            .or_default()
            .push(owner.clone());
    }

    // Get all active coworkers with their start times
    let active_coworkers = state.coworkers.list();
    let coworker_start_times: HashMap<String, chrono::DateTime<chrono::Utc>> = active_coworkers
        .iter()
        .map(|cw| (cw.name.to_lowercase(), cw.started_at))
        .collect();

    // Find tasks with multiple workers and determine who to kill
    for (task_id, workers) in task_workers {
        if workers.len() <= 1 {
            continue;
        }

        // Get the task subject for logging
        let task_subject = in_progress
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
                let start_time = coworker_start_times.get(&name.to_lowercase()).copied();
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

            // Send the duplicate on a break
            state.broadcast_coworker_update(&duplicate, "stopped", None);
            if let Err(e) = state.coworkers.shutdown(&duplicate) {
                warn!(
                    "Failed to send duplicate worker {} on a break: {}",
                    duplicate, e
                );
                continue;
            }

            // Post to channel about the kill
            let msg = Message::text(
                "midtown",
                format!(
                    "🔪 Killed duplicate worker {} on task #{} ({}) - {} started earlier",
                    duplicate, task_id, task_subject, keeper
                ),
            );
            if let Err(e) = state.send_and_broadcast(&msg) {
                warn!("Failed to post duplicate kill message: {}", e);
            }
        }
    }
}

/// Get list of in_progress tasks with their owners and subjects.
fn get_in_progress_tasks_with_owners() -> Vec<(String, String, String)> {
    crate::tasks::get_in_progress_tasks_with_subjects()
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

    // Prune warned_orphans: remove entries for worktrees that are no longer
    // flagged (they were cleaned up or manually deleted). This ensures that
    // if a reused coworker name becomes orphaned again, it will trigger a
    // new warning.
    {
        let mut warned = state.warned_orphans.write().unwrap();
        warned.retain(|name| flagged.contains(name));
    }

    // Filter out worktrees we've already warned about (dedup across poll cycles)
    let already_warned = state.warned_orphans.read().unwrap();
    let new_flags: Vec<_> = flagged
        .into_iter()
        .filter(|name| !already_warned.contains(name))
        .collect();
    drop(already_warned);

    if new_flags.is_empty() {
        return;
    }

    // Record these as warned
    {
        let mut warned = state.warned_orphans.write().unwrap();
        for name in &new_flags {
            warned.insert(name.clone());
        }
    }

    // Notify @lead about orphaned worktrees with unmerged commits
    let names_list = new_flags.join(", ");
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
fn spawn_for_pending_tasks(state: &DaemonState) {
    // Get list of currently running coworkers (excludes Stopping/Stopped)
    let active_coworkers = state.coworkers.list_running();
    let active_names: std::collections::HashSet<String> = active_coworkers
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // Build set of busy coworkers (those with in_progress tasks) for idle detection
    let busy_coworkers: std::collections::HashSet<String> =
        crate::tasks::get_busy_coworkers_for_repo(&state.repo_name)
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();

    // Build set of idle coworkers: active, non-busy, non-isolated (not reviewers)
    // Shuffle to distribute tasks across idle coworkers instead of always picking the first.
    let mut idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|cw| !cw.isolated_tasks) // Skip reviewers
        .filter(|cw| !busy_coworkers.contains(&cw.name.to_lowercase()))
        .map(|cw| cw.name.clone())
        .collect();
    fastrand::shuffle(&mut idle_coworkers);

    // Case 1: Pending tasks with owners assigned but coworker not running
    let pending_with_owners = crate::tasks::get_pending_tasks_with_owners();
    for (task_id, task_subject, owner) in pending_with_owners {
        // Check nudge cooldown for this task
        let task_key = format!("pending-{}", task_id);
        let on_nudge_cooldown = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check("task_nudge", &task_key, Duration::from_secs(300))
        };

        // Decide action using pure decision function
        let action = crate::rules::decide_pending_task_action(
            &task_id.to_string(),
            &task_subject,
            &owner,
            &active_names,
            state.is_at_dev_limit(),
            on_nudge_cooldown,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
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
                let prompt = format!("You've been assigned task #{}: {}. Get started!", tid, subj);
                match state
                    .coworkers
                    .spawn_with_name(o, true, Some(&prompt), false)
                {
                    Ok(_) => {
                        info!("Spawned coworker {} for pending task #{}", o, tid);
                        state.broadcast_coworker_update(o, "running", None);
                        let msg = Message::text(
                            "midtown",
                            daemon_messages::called_in_pending_task(
                                o,
                                &tid.to_string(),
                                config::get_personality(),
                            ),
                        );
                        if let Err(e) = state.send_and_broadcast(&msg) {
                            warn!("Failed to post call-in message: {}", e);
                        }
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
    let pending_unowned = crate::tasks::get_pending_tasks_without_owners();
    // Read all tasks once for relationship lookups (blockedBy, PR owner search)
    let all_tasks = crate::tasks::read_tasks();
    // Track PR# → coworker and task_id → coworker assignments made during this loop iteration.
    // This prevents assigning different coworkers to sub-tasks of the same PR review
    // when multiple sub-tasks are processed in the same tick.
    let mut pr_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut task_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for task in pending_unowned {
        // Check dev coworkers limit before spawning (reserve slots for reviewers)
        if state.is_at_dev_limit() {
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
            if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(&task) {
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
                    crate::tasks::find_pr_owner_in_tasks(&pr_num, &all_tasks)
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
            if let Some(owner) = crate::tasks::find_owner_via_blocked_by(&task, &all_tasks) {
                info!(
                    "Task #{} blocked by owned task - assigning to {}",
                    task.id, owner
                );
                break 'resolve Some(owner);
            }

            None
        };

        // Step 1b: If no grouping found, prefer an idle coworker over spawning a new one.
        // An idle coworker is already running but has no in_progress tasks and isn't
        // a reviewer — assigning them work avoids the cost of spawning a new session.
        let coworker_name = if let Some(name) = grouped_name {
            name
        } else if let Some(idle_name) = idle_coworkers
            .iter()
            .find(|name| {
                // Skip coworkers already assigned work in this tick
                !task_coworker_map
                    .values()
                    .any(|v| v.eq_ignore_ascii_case(name))
            })
            .cloned()
        {
            idle_name
        } else {
            // No idle coworkers available - allocate a new coworker name
            let Some(name) = state.coworkers.next_available_name() else {
                debug!("No available coworker slots for unowned task #{}", task.id);
                break;
            };
            name
        };

        // Check if this coworker is already running (idle reuse) vs needs spawning
        let already_running = active_names.contains(&coworker_name.to_lowercase());

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
        if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(&task) {
            pr_coworker_map.insert(pr_num, coworker_name.clone());
        }

        // Build the prompt message
        let prompt = format!(
            "You've been assigned task #{}: {}. Get started!",
            task.id, task.subject
        );

        if already_running {
            // Step 3a: Coworker is already running (idle reuse) — nudge instead of spawn
            match state.coworkers.nudge(&coworker_name, &prompt) {
                Ok(()) => {
                    info!(
                        "Nudged idle coworker {} with pre-assigned task #{}",
                        coworker_name, task.id
                    );
                    let msg = Message::text(
                        "midtown",
                        daemon_messages::called_in_assigned_task(
                            &coworker_name,
                            &task.id.to_string(),
                            &task.subject,
                            config::get_personality(),
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&msg) {
                        warn!("Failed to post assignment message: {}", e);
                    }
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
            // spawn_with_name handles waiting and sending the prompt internally
            // Use shared task list (not isolated) for pre-assigned task spawns
            match state
                .coworkers
                .spawn_with_name(&coworker_name, false, Some(&prompt), false)
            {
                Ok(_) => {
                    info!(
                        "Spawned coworker {} for pre-assigned task #{}",
                        coworker_name, task.id
                    );
                    state.broadcast_coworker_update(&coworker_name, "running", None);

                    // Post to channel
                    let msg = Message::text(
                        "midtown",
                        daemon_messages::called_in_assigned_task(
                            &coworker_name,
                            &task.id.to_string(),
                            &task.subject,
                            config::get_personality(),
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&msg) {
                        warn!("Failed to post assignment message: {}", e);
                    }
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
}

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// These functions extract the decision logic from the async check-and-act
// functions so it can be tested without mocking tmux, `gh`, or async state.
// Phase 3 (PRs 5-8) will refactor the async functions to call these,
// at which point the #[cfg(test)] gates can be removed.
#[cfg(test)]
mod decisions {
    use std::time::Instant;

    use super::{IDLE_BREAK_DURATION, INTERRUPTED_NUDGE_DURATION, MINIMUM_COWORKER_LIFETIME};
    use crate::rules::detect_interactive_prompt;

    /// Snapshot of a coworker's state for decision-making (no async, no side effects).
    #[derive(Debug, Clone)]
    #[allow(dead_code)] // name used in tests and will be used by async callers in Phase 3
    pub struct CoworkerSnapshot {
        pub name: String,
        pub started_at: chrono::DateTime<chrono::Utc>,
        pub isolated_tasks: bool,
    }

    /// Decision output for idle shutdown checks.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IdleDecision {
        /// Coworker should be shut down (idle too long or isolated+idle).
        Shutdown,
        /// Coworker is protected (busy, has open PR, reviewing, too young).
        Protected,
        /// Coworker just became idle — start tracking.
        StartedIdleTracking,
        /// Coworker is idle but hasn't hit the timeout yet.
        StillWaiting,
    }

    /// Decide whether a single coworker should be shut down for idleness.
    ///
    /// Pure function: takes only data, returns a decision with no side effects.
    pub fn decide_idle_shutdown(
        coworker: &CoworkerSnapshot,
        is_busy: bool,
        has_open_pr: bool,
        is_reviewing: bool,
        idle_since: Option<Instant>,
        now: Instant,
        now_utc: chrono::DateTime<chrono::Utc>,
    ) -> IdleDecision {
        // Check minimum lifetime
        let lifetime = now_utc.signed_duration_since(coworker.started_at);
        if lifetime < chrono::Duration::from_std(MINIMUM_COWORKER_LIFETIME).unwrap_or_default() {
            return IdleDecision::Protected;
        }

        // Busy / open PR / reviewing → protected
        if is_busy || has_open_pr || is_reviewing {
            return IdleDecision::Protected;
        }

        // Isolated coworkers (reviewers) get shut down immediately when idle
        if coworker.isolated_tasks {
            return IdleDecision::Shutdown;
        }

        // Normal idle timeout
        match idle_since {
            Some(since) if now.duration_since(since) >= IDLE_BREAK_DURATION => {
                IdleDecision::Shutdown
            }
            Some(_) => IdleDecision::StillWaiting,
            None => IdleDecision::StartedIdleTracking,
        }
    }

    /// Decision output for interrupt nudge checks.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum InterruptDecision {
        /// Coworker has been interrupted long enough — nudge them.
        Nudge,
        /// Coworker just became interrupted — start tracking.
        StartedTracking,
        /// Coworker is interrupted but hasn't hit the timeout yet.
        StillWaiting,
        /// Coworker is not interrupted — clear any tracking.
        NotInterrupted,
    }

    /// Decide whether a coworker should be nudged for being interrupted.
    pub fn decide_interrupt_nudge(
        pane_content: &str,
        interrupted_since: Option<Instant>,
        now: Instant,
    ) -> InterruptDecision {
        let is_interrupted = pane_content.contains("Interrupted")
            || pane_content.contains("What should Claude do instead?");

        if !is_interrupted {
            return InterruptDecision::NotInterrupted;
        }

        match interrupted_since {
            Some(since) if now.duration_since(since) >= INTERRUPTED_NUDGE_DURATION => {
                InterruptDecision::Nudge
            }
            Some(_) => InterruptDecision::StillWaiting,
            None => InterruptDecision::StartedTracking,
        }
    }

    /// Decision output for prompt detection checks.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PromptDecision {
        /// New prompt detected — nudge the lead.
        NudgeLead { label: String },
        /// Same prompt as before — already nudged, skip.
        AlreadyNudged,
        /// No prompt detected — clear tracking.
        NoPrompt,
    }

    /// Decide whether a coworker's prompt should trigger a lead nudge.
    pub fn decide_prompt_nudge(
        coworker_name: &str,
        pane_content: &str,
        previous_fingerprint: Option<&str>,
    ) -> PromptDecision {
        // Skip the lead
        if coworker_name == "lead" {
            return PromptDecision::NoPrompt;
        }

        match detect_interactive_prompt(pane_content) {
            Some(label) => {
                let fingerprint = label.to_string();
                if previous_fingerprint == Some(label) {
                    PromptDecision::AlreadyNudged
                } else {
                    PromptDecision::NudgeLead { label: fingerprint }
                }
            }
            None => PromptDecision::NoPrompt,
        }
    }

    // ─── Usage Limit Decision ──────────────────────────────────────────

    /// Decision output for usage limit detection.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum UsageLimitDecision {
        /// Usage limit detected in pane — schedule a nudge.
        Detected { coworker: String },
        /// Nudge is already scheduled — skip re-detection.
        AlreadyScheduled,
        /// No usage limit found in any pane.
        NoneDetected,
    }

    /// Decide whether pane contents indicate a usage limit.
    ///
    /// Scans pane contents for known usage/rate limit patterns. If a nudge is
    /// already scheduled (`nudge_already_scheduled`), skips re-detection.
    pub fn decide_usage_limit_detection(
        pane_contents: &[(String, String)], // (coworker_name, pane_content)
        nudge_already_scheduled: bool,
    ) -> UsageLimitDecision {
        if nudge_already_scheduled {
            return UsageLimitDecision::AlreadyScheduled;
        }

        for (name, content) in pane_contents {
            let has_limit = crate::rules::has_usage_limit_pattern(content);

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
    pub enum UsageLimitExpiryDecision {
        /// Nudge time has arrived — nudge all coworkers.
        NudgeNow,
        /// Nudge is scheduled but not yet due.
        NotYet,
        /// No nudge is scheduled.
        NoNudge,
    }

    /// Decide whether a scheduled usage limit nudge should fire.
    pub fn decide_usage_limit_expiry(
        nudge_at: Option<tokio::time::Instant>,
        now: tokio::time::Instant,
    ) -> UsageLimitExpiryDecision {
        match nudge_at {
            Some(at) if now >= at => UsageLimitExpiryDecision::NudgeNow,
            Some(_) => UsageLimitExpiryDecision::NotYet,
            None => UsageLimitExpiryDecision::NoNudge,
        }
    }

    // ─── Orphan Recovery Decision ──────────────────────────────────────

    /// Decision output for orphan recovery.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum OrphanDecision {
        /// Found an orphaned task — should spawn coworker to recover it.
        Recover {
            task_id: String,
            task_subject: String,
            owner: String,
        },
        /// Cooldown is active — skip this tick.
        CooldownActive,
        /// At dev limit — can't spawn more coworkers.
        AtDevLimit,
        /// No orphaned tasks found.
        NoOrphans,
    }

    /// Decide whether any in_progress task needs orphan recovery.
    ///
    /// An orphaned task is one whose owner is not in the active coworker list.
    /// Rate-limited: only one recovery per tick.
    pub fn decide_orphan_recovery(
        in_progress_tasks: &[(String, String, String)], // (task_id, subject, owner)
        active_coworker_names: &std::collections::HashSet<String>,
        last_orphan_spawn: Option<Instant>,
        now: Instant,
        at_dev_limit: bool,
    ) -> OrphanDecision {
        // Check cooldown
        if let Some(last) = last_orphan_spawn
            && now.duration_since(last) < super::ORPHAN_SPAWN_COOLDOWN
        {
            return OrphanDecision::CooldownActive;
        }

        if in_progress_tasks.is_empty() {
            return OrphanDecision::NoOrphans;
        }

        if at_dev_limit {
            return OrphanDecision::AtDevLimit;
        }

        // Find first orphaned task
        for (task_id, subject, owner) in in_progress_tasks {
            let owner = owner.trim().trim_matches('"');
            if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
                continue;
            }
            if !active_coworker_names.contains(&owner.to_lowercase()) {
                return OrphanDecision::Recover {
                    task_id: task_id.clone(),
                    task_subject: subject.clone(),
                    owner: owner.to_string(),
                };
            }
        }

        OrphanDecision::NoOrphans
    }

    // ─── Duplicate Worker Decision ─────────────────────────────────────

    /// Decision output for duplicate task worker detection.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DuplicateWorkerAction {
        pub task_id: String,
        pub keeper: String,
        pub duplicates: Vec<String>,
    }

    /// Decide which workers to kill when multiple workers are on the same task.
    ///
    /// Keeps the earliest-started worker and marks the rest as duplicates.
    pub fn decide_duplicate_workers(
        in_progress_tasks: &[(String, String, String)], // (task_id, subject, owner)
        coworker_start_times: &std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
    ) -> Vec<DuplicateWorkerAction> {
        use std::collections::HashMap;

        // Build task_id -> list of owners
        let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
        for (task_id, _subject, owner) in in_progress_tasks {
            if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
                continue;
            }
            task_workers
                .entry(task_id.clone())
                .or_default()
                .push(owner.clone());
        }

        let mut actions = Vec::new();

        for (task_id, workers) in task_workers {
            if workers.len() <= 1 {
                continue;
            }

            // Sort by start time (earliest first), unknown times go last
            let mut workers_with_times: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> =
                workers
                    .into_iter()
                    .map(|name| {
                        let start_time = coworker_start_times.get(&name.to_lowercase()).copied();
                        (name, start_time)
                    })
                    .collect();

            workers_with_times.sort_by(|a, b| match (&a.1, &b.1) {
                (Some(t1), Some(t2)) => t1.cmp(t2),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });

            let keeper = workers_with_times[0].0.clone();
            let duplicates: Vec<String> = workers_with_times
                .into_iter()
                .skip(1)
                .map(|(name, _)| name)
                .collect();

            actions.push(DuplicateWorkerAction {
                task_id,
                keeper,
                duplicates,
            });
        }

        actions
    }

    // ─── Pending Task Spawn Decision ───────────────────────────────────

    /// Action to take for a pending task with an owner.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PendingTaskAction {
        /// Owner is not running — should spawn them.
        Spawn { task_id: String, owner: String },
        /// Owner is running — nudge them about the pending task.
        Nudge { task_id: String, owner: String },
        /// Owner is running but nudge is on cooldown — skip.
        NudgeCooldown { task_id: String, owner: String },
        /// Dev limit reached — can't spawn.
        DevLimitReached { task_id: String, owner: String },
        /// Skip (owner is lead or empty).
        Skip,
    }

    /// Decide what to do for a pending task that has an assigned owner.
    pub fn decide_pending_task_action(
        task_id: &str,
        owner: &str,
        is_owner_active: bool,
        last_nudge_elapsed: Option<std::time::Duration>,
        at_dev_limit: bool,
    ) -> PendingTaskAction {
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            return PendingTaskAction::Skip;
        }

        if is_owner_active {
            // Check nudge cooldown (300s = 5 min)
            let should_nudge = match last_nudge_elapsed {
                Some(elapsed) => elapsed >= std::time::Duration::from_secs(300),
                None => true,
            };
            if should_nudge {
                PendingTaskAction::Nudge {
                    task_id: task_id.to_string(),
                    owner: owner.to_string(),
                }
            } else {
                PendingTaskAction::NudgeCooldown {
                    task_id: task_id.to_string(),
                    owner: owner.to_string(),
                }
            }
        } else if at_dev_limit {
            PendingTaskAction::DevLimitReached {
                task_id: task_id.to_string(),
                owner: owner.to_string(),
            }
        } else {
            PendingTaskAction::Spawn {
                task_id: task_id.to_string(),
                owner: owner.to_string(),
            }
        }
    }

    // ─── Mention Routing Decision ──────────────────────────────────────

    /// Action to take when a coworker is @mentioned.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MentionAction {
        /// Mentioned coworker is not running — spawn them.
        Spawn { name: String },
        /// Mentioned coworker is running — nudge them.
        Nudge { name: String },
        /// Mentioned coworker is the sender — skip self-mention.
        SelfMention,
        /// Dev limit reached — can't spawn inactive coworker.
        DevLimitReached { name: String },
    }

    /// Decide what to do for a single @mention.
    pub fn decide_mention_action(
        mentioned_name: &str,
        sender: &str,
        is_running: bool,
        at_dev_limit: bool,
    ) -> MentionAction {
        if mentioned_name.eq_ignore_ascii_case(sender) {
            return MentionAction::SelfMention;
        }

        if is_running {
            MentionAction::Nudge {
                name: mentioned_name.to_string(),
            }
        } else if at_dev_limit {
            MentionAction::DevLimitReached {
                name: mentioned_name.to_string(),
            }
        } else {
            MentionAction::Spawn {
                name: mentioned_name.to_string(),
            }
        }
    }

    // ─── PR Issue Action Decision ──────────────────────────────────────

    /// Action to take when a PR has an actionable issue.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PrIssueAction {
        /// Owner is active — nudge them.
        NudgeOwner { owner: String },
        /// Owner is inactive — spawn them.
        SpawnOwner { owner: String },
        /// Dev limit reached — can't spawn inactive owner.
        DevLimitReached { owner: String },
        /// No owner — post to channel.
        PostToChannel,
        /// Already nudged recently — skip (cooldown active).
        CooldownActive,
    }

    /// Decide what to do about a PR issue.
    pub fn decide_pr_issue_action(
        owner: &str,
        is_owner_active: bool,
        at_dev_limit: bool,
        cooldown_active: bool,
    ) -> PrIssueAction {
        if cooldown_active {
            return PrIssueAction::CooldownActive;
        }

        if owner.is_empty() {
            return PrIssueAction::PostToChannel;
        }

        if is_owner_active {
            PrIssueAction::NudgeOwner {
                owner: owner.to_string(),
            }
        } else if at_dev_limit {
            PrIssueAction::DevLimitReached {
                owner: owner.to_string(),
            }
        } else {
            PrIssueAction::SpawnOwner {
                owner: owner.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decisions::*;
    use super::*;

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

    // PrReviewTracker tests
    #[test]
    fn test_pr_review_tracker_new() {
        let tracker = PrReviewTracker::new();
        assert_eq!(tracker.active_count(), 0);
        assert!(!tracker.is_assigned(42));
    }

    #[test]
    fn test_pr_review_tracker_assign() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");

        assert!(tracker.is_assigned(42));
        assert!(!tracker.is_assigned(43));
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn test_pr_review_tracker_mark_reviewed() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        assert!(tracker.is_assigned(42));

        tracker.mark_reviewed(42);
        assert!(!tracker.is_assigned(42));
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_pr_review_tracker_multiple_assignments() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        tracker.assign(43, "park");

        assert!(tracker.is_assigned(42));
        assert!(tracker.is_assigned(43));
        assert_eq!(tracker.active_count(), 2);
    }

    #[test]
    fn test_pr_review_tracker_pr_for_coworker() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        tracker.assign(43, "park");

        assert_eq!(tracker.pr_for_coworker("lexington"), Some(42));
        assert_eq!(tracker.pr_for_coworker("park"), Some(43));
        assert_eq!(tracker.pr_for_coworker("york"), None);

        // After marking reviewed, should return None
        tracker.mark_reviewed(42);
        assert_eq!(tracker.pr_for_coworker("lexington"), None);
    }

    #[test]
    fn test_pr_review_tracker_active_reviewers() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        tracker.assign(43, "park");
        tracker.assign(44, "lexington"); // duplicate reviewer

        let reviewers = tracker.active_reviewers();
        assert!(reviewers.contains("lexington"));
        assert!(reviewers.contains("park"));
        // Should deduplicate
        assert_eq!(reviewers.len(), 2);
    }

    #[test]
    fn test_pr_review_tracker_active_reviewers_after_mark_reviewed() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        tracker.assign(43, "park");

        // Mark lexington's review as done
        tracker.mark_reviewed(42);

        let reviewers = tracker.active_reviewers();
        assert!(!reviewers.contains("lexington"));
        assert!(reviewers.contains("park"));
        assert_eq!(reviewers.len(), 1);
    }

    #[test]
    fn test_pr_review_tracker_get_reviewer() {
        let mut tracker = PrReviewTracker::new();
        tracker.assign(42, "lexington");
        tracker.assign(43, "park");

        assert_eq!(tracker.get_reviewer(42), Some("lexington"));
        assert_eq!(tracker.get_reviewer(43), Some("park"));
        assert_eq!(tracker.get_reviewer(99), None);

        // get_reviewer should return the name even after mark_reviewed removes it
        tracker.mark_reviewed(42);
        assert_eq!(tracker.get_reviewer(42), None);
    }

    #[test]
    fn test_pr_review_tracker_get_reviewer_ignores_timeout() {
        let mut tracker = PrReviewTracker::new();
        // Simulate an expired assignment (> 10 minutes old)
        tracker.assign_at(
            42,
            "broadway",
            Instant::now() - Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + 60),
        );

        // is_assigned should return false (expired)
        assert!(!tracker.is_assigned(42));
        // get_reviewer should still return the name (ignores timeout)
        assert_eq!(tracker.get_reviewer(42), Some("broadway"));
    }

    #[test]
    fn test_cleanup_preserves_active_coworkers() {
        let mut tracker = PrReviewTracker::new();

        // Simulate an expired assignment (> 10 minutes old) for an active coworker
        tracker.assign_at(
            42,
            "broadway",
            Instant::now() - Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + 60),
        );
        // And a fresh assignment for another coworker
        tracker.assign(43, "park");

        // broadway is still active (running as a coworker)
        let active: HashSet<String> = ["broadway".to_string()].into_iter().collect();

        // cleanup_preserving should keep broadway's assignment alive
        tracker.cleanup_preserving(&active);

        // broadway's assignment should be preserved because they're still active
        assert_eq!(tracker.pr_for_coworker("broadway"), Some(42));
        assert!(tracker.active_reviewers().contains("broadway"));

        // park's fresh assignment should also still be there
        assert_eq!(tracker.pr_for_coworker("park"), Some(43));
    }

    #[test]
    fn test_cleanup_removes_expired_inactive_coworkers() {
        let mut tracker = PrReviewTracker::new();

        // Simulate an expired assignment for an inactive coworker
        tracker.assign_at(
            42,
            "broadway",
            Instant::now() - Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + 60),
        );

        // broadway is NOT active
        let active: HashSet<String> = HashSet::new();

        tracker.cleanup_preserving(&active);

        // broadway's assignment should be removed
        assert_eq!(tracker.pr_for_coworker("broadway"), None);
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

    // ─── Test Helpers ──────────────────────────────────────────────────────

    use chrono::Utc;

    /// Build a CoworkerSnapshot with sensible defaults for testing.
    fn test_coworker(name: &str) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::hours(1), // 1 hour old by default
            isolated_tasks: false,
        }
    }

    /// Build an isolated (reviewer) CoworkerSnapshot.
    fn test_isolated_coworker(name: &str) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::hours(1),
            isolated_tasks: true,
        }
    }

    /// Build a young coworker (started 30 seconds ago — under MINIMUM_COWORKER_LIFETIME).
    fn test_young_coworker(name: &str) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(30),
            isolated_tasks: false,
        }
    }

    // ─── Idle Shutdown Decision Tests ──────────────────────────────────────

    #[test]
    fn test_idle_shutdown_idle_past_timeout() {
        let cw = test_coworker("park");
        let now = Instant::now();
        // Was idle since 31 seconds ago (past IDLE_BREAK_DURATION of 30s)
        let idle_since = Some(now - Duration::from_secs(31));

        let decision = decide_idle_shutdown(&cw, false, false, false, idle_since, now, Utc::now());
        assert_eq!(decision, IdleDecision::Shutdown);
    }

    #[test]
    fn test_idle_shutdown_busy_coworker_protected() {
        let cw = test_coworker("park");
        let now = Instant::now();

        let decision = decide_idle_shutdown(&cw, true, false, false, None, now, Utc::now());
        assert_eq!(decision, IdleDecision::Protected);
    }

    #[test]
    fn test_idle_shutdown_open_pr_protected() {
        let cw = test_coworker("park");
        let now = Instant::now();
        // Even with long idle time, open PR protects from shutdown
        let idle_since = Some(now - Duration::from_secs(120));

        let decision = decide_idle_shutdown(&cw, false, true, false, idle_since, now, Utc::now());
        assert_eq!(decision, IdleDecision::Protected);
    }

    #[test]
    fn test_idle_shutdown_active_reviewer_protected() {
        let cw = test_coworker("broadway");
        let now = Instant::now();
        let idle_since = Some(now - Duration::from_secs(120));

        let decision = decide_idle_shutdown(&cw, false, false, true, idle_since, now, Utc::now());
        assert_eq!(decision, IdleDecision::Protected);
    }

    #[test]
    fn test_idle_shutdown_young_coworker_protected() {
        let cw = test_young_coworker("park");
        let now = Instant::now();
        // Idle for a long time, but too young (30s < 300s minimum)
        let idle_since = Some(now - Duration::from_secs(60));

        let decision = decide_idle_shutdown(&cw, false, false, false, idle_since, now, Utc::now());
        assert_eq!(decision, IdleDecision::Protected);
    }

    #[test]
    fn test_idle_shutdown_isolated_coworker_immediate() {
        let cw = test_isolated_coworker("broadway");
        let now = Instant::now();
        // Isolated coworkers are shut down immediately when idle — no timer needed
        let decision = decide_idle_shutdown(&cw, false, false, false, None, now, Utc::now());
        assert_eq!(decision, IdleDecision::Shutdown);
    }

    #[test]
    fn test_idle_shutdown_isolated_but_reviewing_protected() {
        // Regression test for PR #344: isolated coworkers (reviewers) must NOT be
        // shut down while they still have an active review assignment.
        let cw = test_isolated_coworker("broadway");
        let now = Instant::now();
        let decision = decide_idle_shutdown(&cw, false, false, true, None, now, Utc::now());
        assert_eq!(decision, IdleDecision::Protected);
    }

    #[test]
    fn test_idle_shutdown_starts_tracking_new_idle() {
        let cw = test_coworker("park");
        let now = Instant::now();
        // No idle_since entry yet → should start tracking
        let decision = decide_idle_shutdown(&cw, false, false, false, None, now, Utc::now());
        assert_eq!(decision, IdleDecision::StartedIdleTracking);
    }

    #[test]
    fn test_idle_shutdown_still_waiting() {
        let cw = test_coworker("park");
        let now = Instant::now();
        // Idle for 10 seconds — not yet past 30s threshold
        let idle_since = Some(now - Duration::from_secs(10));

        let decision = decide_idle_shutdown(&cw, false, false, false, idle_since, now, Utc::now());
        assert_eq!(decision, IdleDecision::StillWaiting);
    }

    // ─── Interrupt Nudge Decision Tests ────────────────────────────────────

    #[test]
    fn test_interrupt_nudge_past_timeout() {
        let now = Instant::now();
        let since = Some(now - Duration::from_secs(61));
        let pane = "some output\nInterrupted\nmore output";

        let decision = decide_interrupt_nudge(pane, since, now);
        assert_eq!(decision, InterruptDecision::Nudge);
    }

    #[test]
    fn test_interrupt_nudge_before_timeout() {
        let now = Instant::now();
        let since = Some(now - Duration::from_secs(30));
        let pane = "some output\nInterrupted\n";

        let decision = decide_interrupt_nudge(pane, since, now);
        assert_eq!(decision, InterruptDecision::StillWaiting);
    }

    #[test]
    fn test_interrupt_nudge_not_interrupted() {
        let now = Instant::now();
        let pane = "Running tests... all good\n$";

        let decision = decide_interrupt_nudge(pane, None, now);
        assert_eq!(decision, InterruptDecision::NotInterrupted);
    }

    #[test]
    fn test_interrupt_nudge_just_became_interrupted() {
        let now = Instant::now();
        let pane = "Working on something\nWhat should Claude do instead?\n";

        let decision = decide_interrupt_nudge(pane, None, now);
        assert_eq!(decision, InterruptDecision::StartedTracking);
    }

    #[test]
    fn test_interrupt_nudge_clears_tracking_when_no_longer_interrupted() {
        let now = Instant::now();
        // Was previously tracked as interrupted, but pane no longer shows it
        let since = Some(now - Duration::from_secs(45));
        let pane = "Back to normal work\n$";

        let decision = decide_interrupt_nudge(pane, since, now);
        assert_eq!(decision, InterruptDecision::NotInterrupted);
    }

    // ─── Prompt Detection Decision Tests ───────────────────────────────────

    #[test]
    fn test_prompt_nudge_new_prompt_detected() {
        let pane = "Some output\nYes, and don't ask again for this project\nMore text";

        let decision = decide_prompt_nudge("park", pane, None);
        assert_eq!(
            decision,
            PromptDecision::NudgeLead {
                label: "plan approval".to_string()
            }
        );
    }

    #[test]
    fn test_prompt_nudge_same_fingerprint_skipped() {
        let pane = "Some output\nAllow once\nMore text";

        // Already nudged for "permission request"
        let decision = decide_prompt_nudge("park", pane, Some("permission request"));
        assert_eq!(decision, PromptDecision::AlreadyNudged);
    }

    #[test]
    fn test_prompt_nudge_different_prompt_type_triggers() {
        let pane = "Some output\nDo you want to proceed?\nMore text";

        // Previously nudged for a different prompt type
        let decision = decide_prompt_nudge("park", pane, Some("plan approval"));
        assert_eq!(
            decision,
            PromptDecision::NudgeLead {
                label: "confirmation prompt".to_string()
            }
        );
    }

    #[test]
    fn test_prompt_nudge_no_prompt_clears() {
        let pane = "Normal work output, no prompts here\n$";

        let decision = decide_prompt_nudge("park", pane, Some("plan approval"));
        assert_eq!(decision, PromptDecision::NoPrompt);
    }

    #[test]
    fn test_prompt_nudge_lead_always_skipped() {
        let pane = "Yes, and don't ask again for this project";

        // Lead should never trigger a prompt nudge
        let decision = decide_prompt_nudge("lead", pane, None);
        assert_eq!(decision, PromptDecision::NoPrompt);
    }

    #[test]
    fn test_prompt_nudge_select_option_detected() {
        let pane = "Choose your path:\nSelect an option\n> Option A\n> Option B";

        let decision = decide_prompt_nudge("broadway", pane, None);
        assert_eq!(
            decision,
            PromptDecision::NudgeLead {
                label: "question prompt".to_string()
            }
        );
    }

    // ─── Usage Limit Detection Tests ───────────────────────────────────

    #[test]
    fn test_usage_limit_detected_in_pane() {
        let panes = vec![
            ("park".to_string(), "Working on task...\n".to_string()),
            (
                "broadway".to_string(),
                "Usage limit reached. Try again in 15 minutes.\n".to_string(),
            ),
        ];

        let decision = decide_usage_limit_detection(&panes, false);
        assert_eq!(
            decision,
            UsageLimitDecision::Detected {
                coworker: "broadway".to_string()
            }
        );
    }

    #[test]
    fn test_usage_limit_already_scheduled() {
        let panes = vec![("park".to_string(), "Usage limit reached.\n".to_string())];

        let decision = decide_usage_limit_detection(&panes, true);
        assert_eq!(decision, UsageLimitDecision::AlreadyScheduled);
    }

    #[test]
    fn test_usage_limit_none_detected() {
        let panes = vec![
            (
                "park".to_string(),
                "Running tests... all pass\n".to_string(),
            ),
            ("broadway".to_string(), "Editing src/main.rs\n".to_string()),
        ];

        let decision = decide_usage_limit_detection(&panes, false);
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

    // ─── Orphan Recovery Tests ─────────────────────────────────────────

    #[test]
    fn test_orphan_recovery_inactive_owner() {
        let tasks = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "park".to_string(),
        )];
        let active: std::collections::HashSet<String> =
            ["broadway".to_string()].into_iter().collect();
        let now = Instant::now();

        let decision = decide_orphan_recovery(&tasks, &active, None, now, false);
        assert_eq!(
            decision,
            OrphanDecision::Recover {
                task_id: "42".to_string(),
                task_subject: "Fix auth bug".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_orphan_recovery_cooldown_active() {
        let tasks = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "park".to_string(),
        )];
        let active: std::collections::HashSet<String> = std::collections::HashSet::new();
        let now = Instant::now();
        // Last spawn was 2 seconds ago (under 5s cooldown)
        let last_spawn = Some(now - Duration::from_secs(2));

        let decision = decide_orphan_recovery(&tasks, &active, last_spawn, now, false);
        assert_eq!(decision, OrphanDecision::CooldownActive);
    }

    #[test]
    fn test_orphan_recovery_active_owner_no_orphan() {
        let tasks = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "park".to_string(),
        )];
        // park IS active, so the task is not orphaned
        let active: std::collections::HashSet<String> = ["park".to_string()].into_iter().collect();
        let now = Instant::now();

        let decision = decide_orphan_recovery(&tasks, &active, None, now, false);
        assert_eq!(decision, OrphanDecision::NoOrphans);
    }

    #[test]
    fn test_orphan_recovery_at_dev_limit() {
        let tasks = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "park".to_string(),
        )];
        let active: std::collections::HashSet<String> = std::collections::HashSet::new();
        let now = Instant::now();

        let decision = decide_orphan_recovery(&tasks, &active, None, now, true);
        assert_eq!(decision, OrphanDecision::AtDevLimit);
    }

    #[test]
    fn test_orphan_recovery_skips_lead_owner() {
        let tasks = vec![(
            "42".to_string(),
            "Lead's task".to_string(),
            "lead".to_string(),
        )];
        let active: std::collections::HashSet<String> = std::collections::HashSet::new();
        let now = Instant::now();

        let decision = decide_orphan_recovery(&tasks, &active, None, now, false);
        assert_eq!(decision, OrphanDecision::NoOrphans);
    }

    // ─── Duplicate Worker Tests ────────────────────────────────────────

    #[test]
    fn test_duplicate_workers_two_on_same_task() {
        use chrono::Utc;

        let now = Utc::now();
        let earlier = now - chrono::Duration::minutes(5);

        let tasks = vec![
            ("42".to_string(), "Fix auth".to_string(), "park".to_string()),
            (
                "42".to_string(),
                "Fix auth".to_string(),
                "broadway".to_string(),
            ),
        ];

        let mut start_times = std::collections::HashMap::new();
        start_times.insert("park".to_string(), earlier);
        start_times.insert("broadway".to_string(), now);

        let actions = decide_duplicate_workers(&tasks, &start_times);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].task_id, "42");
        assert_eq!(actions[0].keeper, "park"); // started earlier
        assert_eq!(actions[0].duplicates, vec!["broadway".to_string()]);
    }

    #[test]
    fn test_duplicate_workers_single_worker_no_action() {
        let tasks = vec![("42".to_string(), "Fix auth".to_string(), "park".to_string())];

        let start_times = std::collections::HashMap::new();

        let actions = decide_duplicate_workers(&tasks, &start_times);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_duplicate_workers_unknown_start_times_sorted_last() {
        use chrono::Utc;

        let now = Utc::now();

        let tasks = vec![
            ("42".to_string(), "Fix auth".to_string(), "park".to_string()),
            (
                "42".to_string(),
                "Fix auth".to_string(),
                "broadway".to_string(),
            ),
        ];

        // Only park has a known start time; broadway is unknown
        let mut start_times = std::collections::HashMap::new();
        start_times.insert("park".to_string(), now);

        let actions = decide_duplicate_workers(&tasks, &start_times);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].keeper, "park"); // known time beats unknown
        assert_eq!(actions[0].duplicates, vec!["broadway".to_string()]);
    }

    #[test]
    fn test_duplicate_workers_different_tasks_no_conflict() {
        use chrono::Utc;

        let tasks = vec![
            ("42".to_string(), "Fix auth".to_string(), "park".to_string()),
            (
                "43".to_string(),
                "Add tests".to_string(),
                "broadway".to_string(),
            ),
        ];

        let mut start_times = std::collections::HashMap::new();
        start_times.insert("park".to_string(), Utc::now());
        start_times.insert("broadway".to_string(), Utc::now());

        let actions = decide_duplicate_workers(&tasks, &start_times);
        assert!(actions.is_empty());
    }

    // ─── Pending Task Spawn Tests ──────────────────────────────────────

    #[test]
    fn test_pending_task_inactive_owner_spawns() {
        let decision = decide_pending_task_action("42", "park", false, None, false);
        assert_eq!(
            decision,
            PendingTaskAction::Spawn {
                task_id: "42".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_pending_task_active_owner_nudges() {
        let decision = decide_pending_task_action("42", "park", true, None, false);
        assert_eq!(
            decision,
            PendingTaskAction::Nudge {
                task_id: "42".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_pending_task_active_owner_nudge_cooldown() {
        // Last nudge was 60 seconds ago — within the 300s cooldown
        let decision =
            decide_pending_task_action("42", "park", true, Some(Duration::from_secs(60)), false);
        assert_eq!(
            decision,
            PendingTaskAction::NudgeCooldown {
                task_id: "42".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_pending_task_active_owner_nudge_after_cooldown() {
        // Last nudge was 301 seconds ago — past the 300s cooldown
        let decision =
            decide_pending_task_action("42", "park", true, Some(Duration::from_secs(301)), false);
        assert_eq!(
            decision,
            PendingTaskAction::Nudge {
                task_id: "42".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_pending_task_dev_limit_reached() {
        let decision = decide_pending_task_action("42", "park", false, None, true);
        assert_eq!(
            decision,
            PendingTaskAction::DevLimitReached {
                task_id: "42".to_string(),
                owner: "park".to_string(),
            }
        );
    }

    #[test]
    fn test_pending_task_lead_owner_skipped() {
        let decision = decide_pending_task_action("42", "lead", false, None, false);
        assert_eq!(decision, PendingTaskAction::Skip);
    }

    #[test]
    fn test_pending_task_empty_owner_skipped() {
        let decision = decide_pending_task_action("42", "", false, None, false);
        assert_eq!(decision, PendingTaskAction::Skip);
    }

    // ─── Mention Routing Tests ─────────────────────────────────────────

    #[test]
    fn test_mention_inactive_coworker_spawns() {
        let action = decide_mention_action("park", "broadway", false, false);
        assert_eq!(
            action,
            MentionAction::Spawn {
                name: "park".to_string()
            }
        );
    }

    #[test]
    fn test_mention_active_coworker_nudges() {
        let action = decide_mention_action("park", "broadway", true, false);
        assert_eq!(
            action,
            MentionAction::Nudge {
                name: "park".to_string()
            }
        );
    }

    #[test]
    fn test_mention_self_skipped() {
        let action = decide_mention_action("park", "park", true, false);
        assert_eq!(action, MentionAction::SelfMention);
    }

    #[test]
    fn test_mention_dev_limit_blocks_spawn() {
        let action = decide_mention_action("park", "broadway", false, true);
        assert_eq!(
            action,
            MentionAction::DevLimitReached {
                name: "park".to_string()
            }
        );
    }

    // ─── PR Issue Action Tests ─────────────────────────────────────────

    #[test]
    fn test_pr_issue_active_owner_nudged() {
        let action = decide_pr_issue_action("park", true, false, false);
        assert_eq!(
            action,
            PrIssueAction::NudgeOwner {
                owner: "park".to_string()
            }
        );
    }

    #[test]
    fn test_pr_issue_inactive_owner_spawned() {
        let action = decide_pr_issue_action("park", false, false, false);
        assert_eq!(
            action,
            PrIssueAction::SpawnOwner {
                owner: "park".to_string()
            }
        );
    }

    #[test]
    fn test_pr_issue_cooldown_skips() {
        let action = decide_pr_issue_action("park", true, false, true);
        assert_eq!(action, PrIssueAction::CooldownActive);
    }

    #[test]
    fn test_pr_issue_no_owner_posts_to_channel() {
        let action = decide_pr_issue_action("", false, false, false);
        assert_eq!(action, PrIssueAction::PostToChannel);
    }

    #[test]
    fn test_pr_issue_inactive_owner_dev_limit() {
        let action = decide_pr_issue_action("park", false, true, false);
        assert_eq!(
            action,
            PrIssueAction::DevLimitReached {
                owner: "park".to_string()
            }
        );
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
