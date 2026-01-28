//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

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
use crate::coworker::CoworkerManager;
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
}

/// Default interval for restarting the webhook forwarder (5 minutes)
pub const DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS: u64 = 300;

/// Default port for the webhook server (obscure to avoid conflicts)
pub const DEFAULT_WEBHOOK_PORT: u16 = 47022;

/// Default interval for polling PRs (1 minute)
pub const DEFAULT_PR_POLL_INTERVAL_SECS: u64 = 60;

/// Minimum time between nudging the same PR issue (10 minutes)
pub const PR_NUDGE_COOLDOWN_SECS: u64 = 600;

/// Types of actionable PR issues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrIssueType {
    /// PR has merge conflicts
    MergeConflict,
    /// CI checks failed
    CiFailed,
    /// Review requested changes
    ChangesRequested,
    /// PR is approved and ready to merge
    Approved,
    /// PR needs code review (no Claude review comment yet)
    NeedsReview,
    /// PR has review comments from non-owners
    ReviewComment,
}

impl std::fmt::Display for PrIssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrIssueType::MergeConflict => write!(f, "merge conflict"),
            PrIssueType::CiFailed => write!(f, "CI failed"),
            PrIssueType::ChangesRequested => write!(f, "changes requested"),
            PrIssueType::Approved => write!(f, "approved"),
            PrIssueType::NeedsReview => write!(f, "needs review"),
            PrIssueType::ReviewComment => write!(f, "review comment"),
        }
    }
}

/// Tracks which PR issues have been nudged to avoid spamming
#[derive(Debug, Default)]
pub struct PrIssueTracker {
    /// Map of (pr_number, issue_type) -> last_nudge_time
    nudged: HashMap<(u64, PrIssueType), Instant>,
}

impl PrIssueTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a PR has been recently tracked for any issue
    pub fn is_recently_tracked(&self, pr_number: u64) -> bool {
        self.nudged.keys().any(|(num, _)| {
            *num == pr_number
                && self
                    .nudged
                    .get(&(*num, PrIssueType::NeedsReview))
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(PR_NUDGE_COOLDOWN_SECS))
        })
    }

    /// Check if we should nudge for this issue (not nudged recently)
    pub fn should_nudge(&self, pr_number: u64, issue_type: PrIssueType) -> bool {
        match self.nudged.get(&(pr_number, issue_type)) {
            Some(last_nudge) => last_nudge.elapsed() >= Duration::from_secs(PR_NUDGE_COOLDOWN_SECS),
            None => true,
        }
    }

    /// Record that we nudged for this issue
    pub fn record_nudge(&mut self, pr_number: u64, issue_type: PrIssueType) {
        self.nudged.insert((pr_number, issue_type), Instant::now());
    }

    /// Clean up old entries (older than cooldown period)
    pub fn cleanup(&mut self) {
        let cutoff = Duration::from_secs(PR_NUDGE_COOLDOWN_SECS);
        self.nudged
            .retain(|_, last_nudge| last_nudge.elapsed() < cutoff);
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        // Check env vars for webhook config (can override or disable with MIDTOWN_WEBHOOK_PORT=0)
        let webhook_port = std::env::var("MIDTOWN_WEBHOOK_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(|p| if p == 0 { None } else { Some(p) })
            .unwrap_or(Some(DEFAULT_WEBHOOK_PORT));
        let webhook_secret = std::env::var("MIDTOWN_WEBHOOK_SECRET").ok();
        let webhook_restart_interval_secs = std::env::var("MIDTOWN_WEBHOOK_RESTART_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS);

        let pr_poll_interval_secs = std::env::var("MIDTOWN_PR_POLL_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PR_POLL_INTERVAL_SECS);

        // Chat monitor is enabled by default, disable with MIDTOWN_CHAT_MONITOR=0
        let chat_monitor_enabled = std::env::var("MIDTOWN_CHAT_MONITOR")
            .ok()
            .map(|s| s != "0")
            .unwrap_or(true);

        Self {
            // Use repo-specific socket path to isolate daemons per project
            socket_path: crate::paths::daemon_socket(),
            // Use repo-specific PID file for singleton enforcement
            pid_file_path: crate::paths::daemon_pid_file(),
            workdir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            verbose: false,
            webhook_port,
            webhook_secret,
            webhook_restart_interval_secs,
            pr_poll_interval_secs,
            chat_monitor_enabled,
        }
    }
}

/// How long a coworker must be idle before automatic shutdown (5 minutes)
const IDLE_SHUTDOWN_DURATION: Duration = Duration::from_secs(300);

/// How often to check for idle coworkers (30 seconds)
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Interval for checking orphaned tasks (30 seconds)
const ORPHAN_CHECK_INTERVAL_SECS: u64 = 30;

/// Minimum time a coworker must be alive before auto-shutdown (5 minutes)
/// This prevents spawn storms where coworkers are rapidly spawned and killed.
const MINIMUM_COWORKER_LIFETIME: Duration = Duration::from_secs(300);

/// How long a coworker must be interrupted before nudging them to continue (60 seconds)
const INTERRUPTED_NUDGE_DURATION: Duration = Duration::from_secs(60);

/// Cooldown between orphan recovery spawns (5 seconds)
/// Only spawn one coworker per tick, with a minimum gap between spawns.
const ORPHAN_SPAWN_COOLDOWN: Duration = Duration::from_secs(5);

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

/// Tracks which PRs have been assigned for review to avoid duplicates.
#[derive(Debug, Default)]
pub struct PrReviewTracker {
    /// Map of pr_number -> (assigned_coworker, assignment_time)
    assigned: HashMap<u64, (String, Instant)>,
}

/// How long to wait after PR is opened before auto-reviewing (2 minutes)
/// This gives CI time to start and allows the author to add context.
pub const PR_REVIEW_DELAY_SECS: u64 = 120;

/// How long a review assignment is valid before it can be reassigned (10 minutes)
pub const PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS: u64 = 600;

/// Maximum number of concurrent review assignments (rate limiting)
pub const MAX_CONCURRENT_REVIEWS: usize = 4;

impl PrReviewTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a PR has been assigned for review recently
    pub fn is_assigned(&self, pr_number: u64) -> bool {
        match self.assigned.get(&pr_number) {
            Some((_, assigned_at)) => {
                assigned_at.elapsed() < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS)
            }
            None => false,
        }
    }

    /// Record a review assignment
    pub fn assign(&mut self, pr_number: u64, coworker: &str) {
        self.assigned
            .insert(pr_number, (coworker.to_string(), Instant::now()));
    }

    /// Get the number of active review assignments
    pub fn active_count(&self) -> usize {
        self.assigned
            .values()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS))
            .count()
    }

    /// Clean up stale assignments
    pub fn cleanup(&mut self) {
        let timeout = Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS);
        self.assigned.retain(|_, (_, t)| t.elapsed() < timeout);
    }

    /// Mark a PR as reviewed (remove from tracking)
    pub fn mark_reviewed(&mut self, pr_number: u64) {
        self.assigned.remove(&pr_number);
    }
}

/// Shared daemon state.
struct DaemonState {
    coworkers: CoworkerManager,
    channel: Channel,
    socket_path: PathBuf,
    /// Tracks message IDs that have already triggered a nudge to Lead (to avoid duplicates)
    nudged_messages: std::sync::RwLock<HashSet<String>>,
    /// Tracks when each coworker became idle (no in_progress tasks)
    idle_since: RwLock<HashMap<String, Instant>>,
    /// Tracks when each coworker was first detected as interrupted
    interrupted_since: RwLock<HashMap<String, Instant>>,
    /// Tracker to avoid spamming the same PR issues
    pr_issue_tracker: Mutex<PrIssueTracker>,
    /// Tracker for PRs assigned for review
    pr_review_tracker: Mutex<PrReviewTracker>,
    /// Repository name
    repo_name: String,
    /// Last time a coworker was spawned for orphan recovery (rate limiting)
    last_orphan_spawn: Mutex<Option<Instant>>,
    /// Tracks when each pending task was last nudged (task_id -> last nudge time)
    pending_task_nudge_cooldowns: std::sync::Mutex<HashMap<String, Instant>>,
    /// Persistent GitHub state (PR reviewer assignments, etc.)
    github_state: Mutex<crate::github_state::GitHubState>,
    /// Broadcast sender for pushing channel messages to WebSocket clients
    web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
}

impl DaemonState {
    fn new(
        socket_path: PathBuf,
        workdir: PathBuf,
        channel: Channel,
        web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
    ) -> crate::Result<Self> {
        // Derive the tmux session name using git-aware repo detection
        let repo_name = crate::paths::detect_repo_name().unwrap_or_else(|| {
            workdir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "default".to_string())
        });
        let session_name = format!("midtown-{}", repo_name);

        // Create worktree manager for coworker isolation
        let worktree_manager = WorktreeManager::new(workdir).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to initialize worktree manager: {}", e),
        })?;

        // Load persistent GitHub state
        let github_state =
            crate::github_state::load_state_for_repo(&repo_name).unwrap_or_else(|e| {
                warn!("Failed to load github-state.json: {}, using defaults", e);
                crate::github_state::GitHubState::default()
            });

        Ok(Self {
            coworkers: CoworkerManager::new(session_name, worktree_manager),
            channel,
            socket_path,
            nudged_messages: std::sync::RwLock::new(HashSet::new()),
            idle_since: RwLock::new(HashMap::new()),
            interrupted_since: RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(PrIssueTracker::new()),
            pr_review_tracker: Mutex::new(PrReviewTracker::new()),
            repo_name,
            last_orphan_spawn: Mutex::new(None),
            pending_task_nudge_cooldowns: std::sync::Mutex::new(HashMap::new()),
            github_state: Mutex::new(github_state),
            web_updates_tx,
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
}

/// Acquire an exclusive lock on the PID file.
///
/// This enforces singleton behavior - only one daemon can run per repository.
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

    // Start webhook server and gh forwarder watchdog if configured
    let mut webhook_rx = None;
    let mut web_updates_tx = None;
    let (forwarder_shutdown_tx, forwarder_shutdown_rx) = watch::channel(false);

    if let Some(port) = config.webhook_port {
        let webhook_config = WebhookConfig {
            port,
            secret: config.webhook_secret.clone(),
            repo: repo_name.clone(),
            web_static_dir: None, // Use default location
        };
        match start_webhook_server(webhook_config).await {
            Ok((rx, updates_tx)) => {
                info!("Webhook server started on port {}", port);
                webhook_rx = Some(rx);
                web_updates_tx = Some(updates_tx);

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
        config.workdir,
        channel,
        web_updates_tx,
    )?);

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    // Set up idle check interval
    let mut idle_check_interval = interval(IDLE_CHECK_INTERVAL);

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

                // Route @mentions in webhook messages directly (chat monitor skips
                // "github" sender for loop protection, so we handle it here)
                route_mentions(&state, &webhook_event.message);
            }

            // Periodically check for idle coworkers and shut them down
            _ = idle_check_interval.tick() => {
                // Sync internal state with actual tmux windows first
                if let Err(e) = state.coworkers.sync_with_tmux() {
                    warn!("Failed to sync coworker state with tmux: {}", e);
                }
                check_and_shutdown_idle_coworkers(&state).await;
                check_and_nudge_interrupted_coworkers(&state).await;
            }

            // Periodic orphan check and duplicate detection
            _ = orphan_check_interval.tick() => {
                check_for_duplicate_task_workers(&state).await;
                check_and_recover_orphans(&state).await;
                spawn_for_pending_tasks(&state);
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

/// Check for idle coworkers and shut them down after the idle timeout.
///
/// A coworker is considered idle if they have no tasks in "in_progress" status
/// with their name as owner. After 5 minutes of continuous idle, they are
/// automatically shut down.
///
/// IMPORTANT: Coworkers with open PRs are NEVER auto-killed, regardless of
/// idle time. This ensures they can respond to PR feedback and merge their work.
///
/// Also enforces a minimum lifetime check - coworkers must be alive for at least
/// 5 minutes before they can be auto-shutdown. This prevents spawn storms where
/// coworkers are rapidly spawned and killed.
async fn check_and_shutdown_idle_coworkers(state: &DaemonState) {
    // Get list of active coworkers with their data (need started_at for lifetime check)
    let active_coworkers = state.coworkers.list();

    if active_coworkers.is_empty() {
        return;
    }

    // Get in_progress tasks to determine who is busy
    let busy_coworkers = get_busy_coworkers(&state.repo_name);

    // Get coworkers with open PRs - they should NEVER be auto-killed
    let coworkers_with_open_prs = get_coworkers_with_open_prs();

    let now = Instant::now();
    let now_utc = chrono::Utc::now();
    let mut to_shutdown = Vec::new();

    {
        let mut idle_since = state.idle_since.write().await;

        for cw in &active_coworkers {
            let coworker = &cw.name;

            // Check minimum lifetime - coworker must be alive for at least 5 minutes
            let lifetime = now_utc.signed_duration_since(cw.started_at);
            if lifetime < chrono::Duration::from_std(MINIMUM_COWORKER_LIFETIME).unwrap_or_default()
            {
                debug!(
                    "Coworker {} is too young for auto-shutdown ({} < {})",
                    coworker,
                    lifetime,
                    MINIMUM_COWORKER_LIFETIME.as_secs()
                );
                // Remove from idle tracking since they're protected
                idle_since.remove(coworker);
                continue;
            }

            let is_busy = busy_coworkers
                .iter()
                .any(|b| b.eq_ignore_ascii_case(coworker));

            // Check if coworker has an open PR (case-insensitive)
            let has_open_pr = coworkers_with_open_prs
                .iter()
                .any(|c| c.eq_ignore_ascii_case(coworker));

            if is_busy || has_open_pr {
                // Coworker is busy or has open PR, remove from idle tracking
                if idle_since.remove(coworker).is_some() {
                    if has_open_pr {
                        debug!(
                            "Coworker {} has open PR, removed from idle tracking",
                            coworker
                        );
                    } else {
                        debug!(
                            "Coworker {} is now busy, removed from idle tracking",
                            coworker
                        );
                    }
                }
            } else {
                // Coworker is idle and has no open PRs
                match idle_since.get(coworker) {
                    Some(since) => {
                        // Check if they've been idle long enough
                        if now.duration_since(*since) >= IDLE_SHUTDOWN_DURATION {
                            to_shutdown.push(coworker.clone());
                        }
                    }
                    None => {
                        // Just became idle, start tracking
                        idle_since.insert(coworker.clone(), now);
                        debug!("Coworker {} is now idle, starting timer", coworker);
                    }
                }
            }
        }

        // Remove shutdown coworkers from tracking
        for name in &to_shutdown {
            idle_since.remove(name);
        }
    }

    // Shutdown idle coworkers (outside the lock)
    for name in to_shutdown {
        info!(
            "Auto-shutting down idle coworker: {} (idle for 5+ minutes)",
            name
        );

        // Post system message to channel
        let msg = Message::text(
            "system",
            format!("⏱️ Auto-shutting down idle coworker: {}", name),
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post shutdown message to channel: {}", e);
        }

        // Shutdown the coworker
        if let Err(e) = state.coworkers.shutdown(&name) {
            warn!("Failed to shutdown idle coworker {}: {}", name, e);
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
    let now = Instant::now();
    let mut to_nudge = Vec::new();

    {
        let mut interrupted_since = state.interrupted_since.write().await;

        for cw in &active_coworkers {
            let coworker = &cw.name;
            let target = format!("{}:{}", session_name, coworker);

            let pane_content = match crate::tmux::capture_pane(&target) {
                Some(content) => content,
                None => {
                    // Can't capture pane, remove from tracking
                    interrupted_since.remove(coworker);
                    continue;
                }
            };

            let is_interrupted = pane_content.contains("Interrupted")
                || pane_content.contains("What should Claude do instead?");

            if is_interrupted {
                match interrupted_since.get(coworker) {
                    Some(since) => {
                        if now.duration_since(*since) >= INTERRUPTED_NUDGE_DURATION {
                            to_nudge.push(coworker.clone());
                            // Reset tracking so we don't spam nudges every 30s
                            interrupted_since.remove(coworker);
                        }
                    }
                    None => {
                        interrupted_since.insert(coworker.clone(), now);
                        debug!("Coworker {} appears interrupted, starting timer", coworker);
                    }
                }
            } else {
                // Not interrupted, clear tracking
                if interrupted_since.remove(coworker).is_some() {
                    debug!("Coworker {} is no longer interrupted", coworker);
                }
            }
        }
    }

    for name in to_nudge {
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

        if let Err(e) = state.coworkers.nudge(&name, "continue") {
            warn!("Failed to nudge interrupted coworker {}: {}", name, e);
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
/// Coworkers with open PRs should NEVER be auto-killed.
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

/// Senders to skip when routing mentions (loop protection).
const SKIP_SENDERS: &[&str] = &["midtown", "system", "github"];

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
fn route_mentions(state: &DaemonState, msg: &Message) {
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
        // Skip if mentioned coworker is the sender (don't nudge yourself)
        if name.eq_ignore_ascii_case(&msg.from) {
            continue;
        }

        // Check if coworker is already running
        let is_running = state.coworkers.get(&name).is_some();

        // Build the message to send to the coworker
        let nudge_text = format!("{} said: {}", msg.from, msg.content);

        if !is_running {
            // Spawn the coworker with resume flag and the @mention message as prompt
            info!(
                "Spawning mentioned coworker {} (not currently running)",
                name
            );
            // spawn_with_name handles waiting and sending the prompt internally
            match state
                .coworkers
                .spawn_with_name(&name, true, Some(&nudge_text))
            {
                Ok(_) => {
                    info!("Spawned coworker {} via @mention", name);
                    // Post to channel about the spawn
                    let spawn_msg = Message::text(
                        "midtown",
                        format!("🚀 Spawned {} in response to @mention", name),
                    );
                    if let Err(e) = state.send_and_broadcast(&spawn_msg) {
                        warn!("Failed to post spawn message: {}", e);
                    }
                }
                Err(e) => {
                    warn!("Failed to spawn coworker {}: {}", name, e);
                    // Post error to channel
                    let err_msg = Message::text(
                        "midtown",
                        format!("⚠️ Failed to spawn {} for @mention: {}", name, e),
                    );
                    let _ = state.send_and_broadcast(&err_msg);
                    continue;
                }
            }
        } else {
            // Coworker is already running - just nudge them
            if let Err(e) = state.coworkers.nudge(&name, &nudge_text) {
                warn!("Failed to nudge {} about @mention: {}", name, e);
            } else {
                info!("Nudged {} about @mention from {}", name, msg.from);
            }
        }
    }
}

/// Extract valid coworker @mentions from message content.
///
/// Returns a list of coworker names that were mentioned (lowercase).
/// Uses word boundary detection to avoid false positives.
fn extract_mentions(content: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let content_lower = content.to_lowercase();

    // Look for @name patterns where name is a valid coworker name
    for &name in COWORKER_NAMES {
        let pattern = format!("@{}", name);
        if let Some(idx) = content_lower.find(&pattern) {
            // Check that this is at a word boundary (not part of a larger word)
            let after_idx = idx + pattern.len();
            let at_word_boundary = after_idx >= content.len()
                || !content[after_idx..]
                    .chars()
                    .next()
                    .unwrap_or(' ')
                    .is_alphanumeric();

            if at_word_boundary && !mentions.contains(&name.to_string()) {
                mentions.push(name.to_string());
            }
        }
    }

    mentions
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
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt",
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)?;

    // Cleanup old tracking entries
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut review_tracker = state.pr_review_tracker.lock().await;
        review_tracker.cleanup();
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

            // Determine who to nudge
            let nudged = if active_coworkers.contains(&owner.to_string()) {
                // Owner is an active coworker, nudge them
                match state.coworkers.nudge(owner, &message) {
                    Ok(()) => {
                        info!("Nudged {} about PR #{}: {}", owner, pr_number, issue_type);
                        true
                    }
                    Err(e) => {
                        warn!("Failed to nudge {}: {}", owner, e);
                        false
                    }
                }
            } else if !owner.is_empty() {
                // Owner is not active — spawn them with the issue prompt
                info!(
                    "PR #{} owner {} is not active, spawning to address {}",
                    pr_number, owner, issue_type
                );
                match state.coworkers.spawn_with_name(owner, true, Some(&message)) {
                    Ok(_) => {
                        info!(
                            "Spawned {} to address {} on PR #{}",
                            owner, issue_type, pr_number
                        );
                        let msg = Message::text(
                            "midtown",
                            format!(
                                "🚀 Spawned {} to address {} on PR #{}",
                                owner, issue_type, pr_number
                            ),
                        );
                        if let Err(e) = state.send_and_broadcast(&msg) {
                            warn!("Failed to post spawn message: {}", e);
                        }
                        true
                    }
                    Err(e) => {
                        warn!(
                            "Failed to spawn {} for PR #{} {}: {}",
                            owner, pr_number, issue_type, e
                        );
                        // Fall back to posting to channel (indicate spawn was attempted)
                        let channel_message = format!(
                            "PR #{} ({}) owned by {} - {}: {} (spawn failed)",
                            pr_number,
                            truncate_str(title, 40),
                            owner,
                            issue_type,
                            get_issue_action(issue_type)
                        );
                        let msg = Message::new("midtown", channel_message, MessageType::Text);
                        if let Err(e) = state.send_and_broadcast(&msg) {
                            warn!("Failed to post PR issue to channel: {}", e);
                        }
                        true
                    }
                }
            } else {
                // No owner, post to channel
                let msg = Message::new("midtown", message.clone(), MessageType::Text);
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post PR issue to channel: {}", e);
                }
                info!(
                    "Posted PR #{} issue to channel (no owner): {}",
                    pr_number, issue_type
                );
                true
            };

            // Record the nudge
            if nudged {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_nudge(pr_number, issue_type);
            }
        }
    }

    // Auto-spawn reviewers for PRs that need review
    spawn_reviewers_for_prs(state, &prs, &active_coworkers).await;

    Ok(())
}

/// Spawn reviewers for PRs that need code review.
///
/// This function identifies PRs that:
/// - Are not drafts
/// - Are old enough (past the review delay)
/// - Don't have a Claude review comment yet
/// - Haven't been assigned for review recently
/// - Aren't owned by the potential reviewer (no self-reviews)
///
/// For each eligible PR, it spawns a new coworker (or uses an idle one) and
/// nudges them to run `/code-review:code-review <pr-number>`.
async fn spawn_reviewers_for_prs(
    state: &DaemonState,
    prs: &[serde_json::Value],
    active_coworkers: &[String],
) {
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

        // Check if already assigned for review (check both in-memory and persistent state)
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

        // Check if PR already has a Claude review (expensive, do last)
        if pr_has_claude_review(pr_number) {
            debug!("PR #{} already has a Claude review", pr_number);
            // Free the tracker slot if this PR was assigned — the review completed
            // but mark_reviewed() was never called (it only fires on nudge failure)
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
                    if let Err(e) =
                        crate::github_state::save_state_for_repo(&state.repo_name, &github_state)
                    {
                        warn!("Failed to save github-state.json: {}", e);
                    }
                }
            }
            continue;
        }

        // Get PR owner from branch prefix
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let pr_owner = coworker_from_branch(head_ref);

        // Find a reviewer (not the PR owner for no self-reviews)
        let reviewer = find_available_reviewer(state, active_coworkers, pr_owner.as_deref()).await;

        match reviewer {
            Some(reviewer_name) => {
                let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

                // Record the assignment (in-memory)
                {
                    let mut tracker = state.pr_review_tracker.lock().await;
                    tracker.assign(pr_number, &reviewer_name);
                }

                // Persist the assignment to github-state.json
                {
                    let mut github_state = state.github_state.lock().await;
                    github_state.assign_reviewer(pr_number, &reviewer_name);
                    if let Err(e) =
                        crate::github_state::save_state_for_repo(&state.repo_name, &github_state)
                    {
                        warn!("Failed to save github-state.json: {}", e);
                    }
                }

                // Nudge the reviewer to run /code-review:code-review
                let nudge_msg = format!(
                    "Please review PR #{}: {}. Run: /code-review:code-review {}",
                    pr_number,
                    truncate_str(title, 50),
                    pr_number
                );

                match state.coworkers.nudge(&reviewer_name, &nudge_msg) {
                    Ok(()) => {
                        info!(
                            "Assigned {} to review PR #{}: {}",
                            reviewer_name,
                            pr_number,
                            truncate_str(title, 40)
                        );

                        // Post to channel about the assignment
                        let channel_msg = Message::new(
                            "midtown",
                            format!("🔍 {} assigned to review PR #{}", reviewer_name, pr_number),
                            MessageType::Text,
                        );
                        if let Err(e) = state.send_and_broadcast(&channel_msg) {
                            warn!("Failed to post review assignment to channel: {}", e);
                        }

                        reviews_spawned += 1;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to nudge {} to review PR #{}: {}",
                            reviewer_name, pr_number, e
                        );
                        // Remove the assignment since we couldn't nudge (in-memory)
                        let mut tracker = state.pr_review_tracker.lock().await;
                        tracker.mark_reviewed(pr_number);
                        // Also remove from persistent state
                        drop(tracker);
                        let mut github_state = state.github_state.lock().await;
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
            None => {
                // No available reviewer - try spawning a new coworker
                debug!(
                    "No available reviewer for PR #{}, attempting to spawn new coworker",
                    pr_number
                );

                // Spawn without prompt - we'll handle the async wait and nudge ourselves
                // to avoid blocking the tokio runtime with std::thread::sleep
                match state.coworkers.spawn(false, None) {
                    Ok(new_coworker) => {
                        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

                        // Record the assignment (in-memory)
                        {
                            let mut tracker = state.pr_review_tracker.lock().await;
                            tracker.assign(pr_number, &new_coworker);
                        }

                        // Persist the assignment to github-state.json
                        {
                            let mut github_state = state.github_state.lock().await;
                            github_state.assign_reviewer(pr_number, &new_coworker);
                            if let Err(e) = crate::github_state::save_state_for_repo(
                                &state.repo_name,
                                &github_state,
                            ) {
                                warn!("Failed to save github-state.json: {}", e);
                            }
                        }

                        // Give the new coworker time to start (async sleep)
                        tokio::time::sleep(Duration::from_secs(3)).await;

                        // Nudge the new reviewer to run /code-review:code-review
                        let nudge_msg = format!(
                            "Please review PR #{}: {}. Run: /code-review:code-review {}",
                            pr_number,
                            truncate_str(title, 50),
                            pr_number
                        );

                        match state.coworkers.nudge(&new_coworker, &nudge_msg) {
                            Ok(()) => {
                                info!(
                                    "Spawned {} to review PR #{}: {}",
                                    new_coworker,
                                    pr_number,
                                    truncate_str(title, 40)
                                );

                                // Post to channel about the spawn
                                let channel_msg = Message::new(
                                    "midtown",
                                    format!(
                                        "🔍 Spawned {} to review PR #{}",
                                        new_coworker, pr_number
                                    ),
                                    MessageType::Text,
                                );
                                if let Err(e) = state.send_and_broadcast(&channel_msg) {
                                    warn!("Failed to post spawn message to channel: {}", e);
                                }

                                reviews_spawned += 1;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to nudge newly spawned {} to review PR #{}: {}",
                                    new_coworker, pr_number, e
                                );
                                // Remove the assignment since we couldn't nudge (in-memory)
                                let mut tracker = state.pr_review_tracker.lock().await;
                                tracker.mark_reviewed(pr_number);
                                // Also remove from persistent state
                                drop(tracker);
                                let mut github_state = state.github_state.lock().await;
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
                    Err(e) => {
                        debug!("Could not spawn new reviewer for PR #{}: {}", pr_number, e);
                    }
                }
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

/// Find an available coworker to review a PR.
///
/// Prefers idle coworkers (those with no in_progress tasks).
/// Excludes the PR owner to prevent self-reviews.
///
/// Returns None if no suitable reviewer is available.
async fn find_available_reviewer(
    state: &DaemonState,
    active_coworkers: &[String],
    pr_owner: Option<&str>,
) -> Option<String> {
    if active_coworkers.is_empty() {
        return None;
    }

    // Get coworkers who are busy (have in_progress tasks)
    let busy_coworkers = get_busy_coworkers(&state.repo_name);

    // Get coworkers who have open PRs — they may be awaiting review and
    // available to review other PRs despite having an in_progress task
    let coworkers_with_open_prs: HashSet<String> = get_coworkers_with_open_prs()
        .into_iter()
        .map(|name| name.to_lowercase())
        .collect();

    // Get coworkers who are already assigned to review PRs (combine in-memory and persistent)
    let reviewing_coworkers: HashSet<String> = {
        let tracker = state.pr_review_tracker.lock().await;
        let in_memory: HashSet<String> = tracker
            .assigned
            .values()
            .filter(|(_, t)| t.elapsed() < Duration::from_secs(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS))
            .map(|(name, _)| name.to_lowercase())
            .collect();
        drop(tracker);

        let github_state = state.github_state.lock().await;
        let persistent: HashSet<String> = github_state
            .assigned_reviewers()
            .map(|name| name.to_lowercase())
            .collect();

        in_memory.union(&persistent).cloned().collect()
    };

    // Find available coworkers: either idle, or busy but awaiting review on their own PR
    for coworker in active_coworkers {
        let coworker_lower = coworker.to_lowercase();

        // Skip if this is the PR owner (no self-reviews)
        if let Some(owner) = pr_owner
            && coworker_lower == owner.to_lowercase()
        {
            continue;
        }

        // Skip if busy with a task, UNLESS they have an open PR (likely awaiting review)
        let is_busy = busy_coworkers
            .iter()
            .any(|b| b.eq_ignore_ascii_case(coworker));
        if is_busy {
            if coworkers_with_open_prs.contains(&coworker_lower) {
                debug!(
                    "{} is busy but has an open PR — eligible for review assignment",
                    coworker
                );
            } else {
                continue;
            }
        }

        // Skip if already assigned to review another PR
        if reviewing_coworkers.contains(&coworker_lower) {
            continue;
        }

        return Some(coworker.clone());
    }

    None
}

/// Detect actionable issues for a PR.
fn detect_pr_issues(pr: &serde_json::Value) -> Vec<PrIssueType> {
    let mut issues = Vec::new();

    // Check for merge conflicts
    let mergeable = pr.get("mergeable").and_then(|m| m.as_str()).unwrap_or("");
    if mergeable == "CONFLICTING" {
        issues.push(PrIssueType::MergeConflict);
    }

    // Check for CI failures
    if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
        let has_failure = checks.iter().any(|check| {
            let conclusion = check
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            conclusion == "FAILURE"
        });
        if has_failure {
            issues.push(PrIssueType::CiFailed);
        }
    }

    // Check review decision
    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    match review_decision {
        "CHANGES_REQUESTED" => issues.push(PrIssueType::ChangesRequested),
        "APPROVED" => issues.push(PrIssueType::Approved),
        _ => {}
    }

    issues
}

/// Get action text for a PR issue type.
fn get_issue_action(issue_type: PrIssueType) -> &'static str {
    match issue_type {
        PrIssueType::MergeConflict => "please rebase",
        PrIssueType::CiFailed => "please investigate",
        PrIssueType::ChangesRequested => "please address feedback",
        PrIssueType::Approved => "ready to merge!",
        PrIssueType::NeedsReview => "spawning reviewer",
        PrIssueType::ReviewComment => "please address review feedback and merge if appropriate",
    }
}

/// Truncate a string to max length with ellipsis.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Check if text contains a coworker review signature.
///
/// Coworker reviews are identified by:
/// - The "🤖 Reviewed by" or "Reviewed by" signature (legacy formal reviews)
/// - The "<!-- midtown:" frontmatter (comment-based reviews)
/// - The "## Code Review by" header (comment-based reviews)
fn text_contains_review_signature(text: &str) -> bool {
    text.contains("🤖 Reviewed by")
        || text.contains("Reviewed by")
        || text.contains("<!-- midtown:")
        || text.contains("## Code Review by")
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

/// Get the creation time of a PR to enforce review delay.
///
/// Returns None if the PR age couldn't be determined.
fn get_pr_age_secs(pr: &serde_json::Value) -> Option<u64> {
    let created_at = pr.get("createdAt").and_then(|c| c.as_str())?;
    let created = chrono::DateTime::parse_from_rfc3339(created_at).ok()?;
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(created);
    Some(duration.num_seconds().max(0) as u64)
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

        "coworker.shutdown" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_shutdown(request.id, name, state),
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
    // Pass prompt to spawn() - it handles waiting and nudging internally
    match state.coworkers.spawn(resume, prompt.as_deref()) {
        Ok(name) => {
            info!("Spawned coworker: {}", name);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Spawned coworker: {}", name),
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

/// Handle coworker.shutdown RPC method.
fn handle_coworker_shutdown(id: RequestId, name: &str, state: &DaemonState) -> Response {
    match state.coworkers.shutdown(name) {
        Ok(()) => {
            info!("Shutdown coworker: {}", name);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Shutdown coworker: {}", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to shutdown coworker {}: {}", name, e);
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

/// Known system senders that should not trigger feedback detection.
const SYSTEM_SENDERS: &[&str] = &["Lead", "lead", "github", "system", "GitHub"];

/// Patterns that indicate a message is asking for feedback or help.
/// Checked case-insensitively.
const FEEDBACK_PATTERNS: &[&str] = &[
    "feedback",
    "thoughts?",
    "opinion?",
    "what do you think",
    "help",
    "blocked",
    "stuck",
    "unsure",
    "not sure",
    "question",
    "@lead",
    "lead:",
];

/// Check if a message is asking for feedback or help.
///
/// Returns true if the message:
/// - Contains any feedback pattern keywords
/// - Is directed at Lead (@Lead, Lead:)
/// - Ends with "?" and contains substantive content (not just a status update)
fn is_feedback_request(message: &str) -> bool {
    let lower = message.to_lowercase();

    // Check for explicit feedback patterns
    for pattern in FEEDBACK_PATTERNS {
        if lower.contains(pattern) {
            return true;
        }
    }

    // Check if it ends with "?" and has substantive content
    // Exclude status updates like "claiming task?" or short messages
    if message.trim().ends_with('?') && message.len() > 30 {
        // But exclude if it looks like a status update (starts with /me or common status words)
        if !lower.starts_with("/me ")
            && !lower.contains("claiming")
            && !lower.contains("starting")
            && !lower.contains("working on")
        {
            return true;
        }
    }

    false
}

/// Check if a sender is a coworker (not Lead or system).
fn is_coworker_sender(from: &str) -> bool {
    !SYSTEM_SENDERS.contains(&from)
}

/// Check if a message is directed at a coworker but NOT at the lead.
///
/// Returns true if the message contains `@<coworker_name>` (for any known coworker)
/// but does not contain `@lead`. This is used to skip nudging the lead when
/// coworkers are communicating with each other.
fn is_directed_at_coworker_only(message: &str) -> bool {
    let lower = message.to_lowercase();

    // Check if message mentions @lead
    let mentions_lead = lower.contains("@lead");
    if mentions_lead {
        return false;
    }

    // Check if message mentions any coworker
    COWORKER_NAMES
        .iter()
        .any(|&name| lower.contains(&format!("@{}", name)))
}

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in their tmux tab name.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
fn handle_channel_post(id: RequestId, from: &str, message: &str, state: &DaemonState) -> Response {
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

            // Check for feedback requests from coworkers and nudge Lead
            // Skip if the message is directed at another coworker (not @lead)
            if is_coworker_sender(from)
                && is_feedback_request(&content)
                && !is_directed_at_coworker_only(&content)
            {
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

                    // Truncate question for nudge message (max 100 chars)
                    let question = if content.len() > 100 {
                        format!("{}...", &content[..97])
                    } else {
                        content.clone()
                    };

                    let nudge_msg = format!("{} is asking for feedback: {}", from, question);
                    info!("Nudging Lead about feedback request from {}", from);

                    // Nudge the Lead window
                    if let Err(e) = state.coworkers.nudge_lead(&nudge_msg) {
                        warn!("Failed to nudge Lead about feedback request: {}", e);
                    }
                }
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
                "owner": task.owner,
            })
        })
        .collect()
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

/// Truncate a message for summary display.
fn truncate_message(msg: &str, max_len: usize) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..max_len])
    }
}

// ============================================================================
// Auto-nudge helpers for PR activity
// ============================================================================

/// Known coworker names (Manhattan avenues).
const COWORKER_NAMES: &[&str] = &[
    "lexington",
    "park",
    "madison",
    "broadway",
    "amsterdam",
    "columbus",
    "central",
    "riverside",
    "york",
    "pleasant",
    "vernon",
    "bleecker",
    "houston",
    "canal",
    "spring",
    "prince",
    "mercer",
];

/// Extract PR number from a message content.
///
/// Looks for patterns like "PR #42", "#42", "PR #123".
#[cfg(test)]
fn extract_pr_number(content: &str) -> Option<u64> {
    // Look for "PR #N" pattern first
    if let Some(idx) = content.find("PR #") {
        let after = &content[idx + 4..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse() {
            return Some(num);
        }
    }

    // Look for " #N " pattern (standalone PR reference)
    // This handles messages like "approved PR #42" where we already caught it above
    // but also cases like "on #42:"
    for (i, _) in content.match_indices(" #") {
        let after = &content[i + 2..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty()
            && let Ok(num) = num_str.parse()
        {
            return Some(num);
        }
    }

    None
}

/// Extract coworker name from branch prefix (e.g., "lexington/fix-auth" -> "lexington").
fn coworker_from_branch(branch: &str) -> Option<String> {
    let prefix = branch.split('/').next()?;
    COWORKER_NAMES
        .iter()
        .find(|&&name| name.eq_ignore_ascii_case(prefix))
        .map(|&s| s.to_string())
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

    // Don't nudge the owner about their own comments
    if owner == activity.actor {
        debug!(
            "PR #{} comment is from owner {}, skipping self-nudge",
            pr_number, owner
        );
        return;
    }

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

    // Try to nudge an active coworker, or spawn them if inactive
    let is_active = state.coworkers.get(&owner).is_some();

    if is_active {
        match state.coworkers.nudge(&owner, &nudge_msg) {
            Ok(()) => {
                info!(
                    "Nudged {} about review comment on PR #{} from {}",
                    owner, pr_number, activity.actor
                );
            }
            Err(e) => {
                warn!("Failed to nudge {} about PR #{}: {}", owner, pr_number, e);
                return;
            }
        }
    } else {
        // Owner is not active — spawn them with the review feedback prompt
        info!(
            "PR #{} owner {} is not active, spawning to address review feedback",
            pr_number, owner
        );
        match state
            .coworkers
            .spawn_with_name(&owner, true, Some(&nudge_msg))
        {
            Ok(_) => {
                info!(
                    "Spawned {} to address review feedback on PR #{}",
                    owner, pr_number
                );
                let msg = Message::text(
                    "daemon",
                    format!(
                        "Spawned {} to address review feedback on PR #{}",
                        owner, pr_number
                    ),
                );
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post spawn message: {}", e);
                }
            }
            Err(e) => {
                warn!(
                    "Failed to spawn {} for PR #{} review feedback: {}",
                    owner, pr_number, e
                );
                return;
            }
        }
    }

    // Record the nudge to prevent spamming
    let mut tracker = state.pr_issue_tracker.lock().await;
    tracker.record_nudge(pr_number, PrIssueType::ReviewComment);
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
        let last_spawn = state.last_orphan_spawn.lock().await;
        if let Some(last) = *last_spawn
            && last.elapsed() < ORPHAN_SPAWN_COOLDOWN
        {
            debug!(
                "Orphan recovery cooldown active ({:?} remaining)",
                ORPHAN_SPAWN_COOLDOWN - last.elapsed()
            );
            return;
        }
    }

    // Get in_progress tasks with their owners
    let in_progress = get_in_progress_tasks_with_owners();

    if in_progress.is_empty() {
        return;
    }

    // Get list of currently active coworkers
    let active_names: std::collections::HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // Find orphaned tasks (in_progress with owner not in active list)
    // Rate limit: only recover ONE coworker per tick to prevent spawn storms
    for (task_id, task_subject, owner) in in_progress {
        // Skip if owner is Lead, empty, or just whitespace/quotes
        let owner = owner.trim().trim_matches('"').to_string();
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }

        // Skip if coworker is already active
        if active_names.contains(&owner.to_lowercase()) {
            continue;
        }

        // This is an orphaned task - try to recover the coworker
        info!(
            "Detected orphaned task #{} owned by {} - attempting recovery",
            task_id, owner
        );

        // Build the prompt message to send during spawn
        let prompt = format!(
            "Resume task #{}: {}. You were working on this task before your session was interrupted. Check your git status and continue where you left off.",
            task_id, task_subject
        );

        // spawn_with_name handles waiting and sending the prompt internally
        match state.coworkers.spawn_with_name(&owner, true, Some(&prompt)) {
            Ok(_) => {
                info!("Respawned coworker {} successfully", owner);

                // Update last spawn time for rate limiting
                {
                    let mut last_spawn = state.last_orphan_spawn.lock().await;
                    *last_spawn = Some(Instant::now());
                }

                // Post to channel about the recovery
                let recovery_msg = Message::text(
                    "midtown",
                    format!(
                        "♻️ Recovered coworker {} for orphaned task #{}",
                        owner, task_id
                    ),
                );
                if let Err(e) = state.send_and_broadcast(&recovery_msg) {
                    warn!("Failed to post recovery message: {}", e);
                }

                // Rate limit: only spawn ONE coworker per tick
                break;
            }
            Err(e) => {
                // Could not respawn - reset task to pending so another coworker can claim it
                // This might happen if the worktree doesn't exist after restart
                warn!(
                    "Could not respawn {} for orphaned task #{}: {} - resetting task to pending",
                    owner, task_id, e
                );

                // Reset the task so it can be picked up by another coworker
                if let Err(reset_err) =
                    crate::tasks::reset_task_to_pending_for_repo(&task_id, &state.repo_name)
                {
                    warn!(
                        "Failed to reset orphaned task #{} to pending: {}",
                        task_id, reset_err
                    );
                } else {
                    info!(
                        "Reset orphaned task #{} to pending (original owner {} could not be respawned)",
                        task_id, owner
                    );

                    // Post to channel so team knows the task is available
                    let msg = Message::text(
                        "midtown",
                        format!(
                            "🔄 Task #{} reset to pending - {} could not be respawned",
                            task_id, owner
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&msg) {
                        warn!("Failed to post task reset message: {}", e);
                    }
                }
            }
        }
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

            // Shutdown the duplicate
            if let Err(e) = state.coworkers.shutdown(&duplicate) {
                warn!("Failed to shutdown duplicate worker {}: {}", duplicate, e);
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
/// Handles two cases:
/// 1. Pending tasks with owners - spawn/nudge the assigned coworker if not running
/// 2. Pending tasks without owners - spawn a new coworker, assign the task, and nudge
fn spawn_for_pending_tasks(state: &DaemonState) {
    // Get list of currently active coworkers
    let active_names: std::collections::HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    // Case 1: Pending tasks with owners assigned but coworker not running
    let pending_with_owners = crate::tasks::get_pending_tasks_with_owners();
    for (task_id, task_subject, owner) in pending_with_owners {
        // Skip if owner is Lead or empty
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }

        // If coworker is already active, nudge them about the pending task (with cooldown)
        if active_names.contains(&owner.to_lowercase()) {
            let task_key = format!("pending-{}", task_id);
            let should_nudge = {
                let cooldowns = state.pending_task_nudge_cooldowns.lock().unwrap();
                match cooldowns.get(&task_key) {
                    Some(last_nudge) => last_nudge.elapsed() >= Duration::from_secs(300),
                    None => true,
                }
            };
            if should_nudge {
                let nudge_msg = format!(
                    "You have pending task #{}: {}. Get started!",
                    task_id, task_subject
                );
                if let Err(e) = state.coworkers.nudge(&owner, &nudge_msg) {
                    debug!(
                        "Failed to nudge {} about pending task #{}: {}",
                        owner, task_id, e
                    );
                } else {
                    info!("Nudged {} about pending task #{}", owner, task_id);
                    let mut cooldowns = state.pending_task_nudge_cooldowns.lock().unwrap();
                    cooldowns.insert(task_key, Instant::now());
                }
            }
            continue;
        }

        // Coworker is assigned but not running - spawn with prompt
        info!(
            "Pending task #{} is assigned to {} but coworker not running - spawning",
            task_id, owner
        );

        // Build the prompt message to send during spawn
        let prompt = format!(
            "You've been assigned task #{}: {}. Get started!",
            task_id, task_subject
        );

        // spawn_with_name handles waiting and sending the prompt internally
        match state.coworkers.spawn_with_name(&owner, true, Some(&prompt)) {
            Ok(_) => {
                info!("Spawned coworker {} for pending task #{}", owner, task_id);

                // Post to channel
                let msg = Message::text(
                    "midtown",
                    format!(
                        "🚀 Spawned coworker {} for pending task #{}",
                        owner, task_id
                    ),
                );
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post spawn message: {}", e);
                }
            }
            Err(e) => {
                debug!(
                    "Could not spawn {} for pending task #{}: {}",
                    owner, task_id, e
                );
            }
        }
    }

    // Case 2: Pending tasks without owners - assign ownership atomically, then spawn
    let pending_unowned = crate::tasks::get_pending_tasks_without_owners();
    for task in pending_unowned {
        // Step 1: Get the next available coworker name (without spawning yet)
        let Some(coworker_name) = state.coworkers.next_available_name() else {
            debug!("No available coworker slots for unowned task #{}", task.id);
            // Stop trying - if we're out of slots we'll try again next tick
            break;
        };

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
            "Assigned task #{} to {} (pre-spawn)",
            task.id, coworker_name
        );

        // Build the prompt message to send during spawn
        let prompt = format!(
            "You've been assigned task #{}: {}. Get started!",
            task.id, task.subject
        );

        // Step 3: Now spawn the coworker with the pre-assigned name and prompt
        // spawn_with_name handles waiting and sending the prompt internally
        match state
            .coworkers
            .spawn_with_name(&coworker_name, false, Some(&prompt))
        {
            Ok(_) => {
                info!(
                    "Spawned coworker {} for pre-assigned task #{}",
                    coworker_name, task.id
                );

                // Post to channel
                let msg = Message::text(
                    "midtown",
                    format!(
                        "🚀 Spawned coworker {} for assigned task #{}: {}",
                        coworker_name, task.id, task.subject
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

#[cfg(test)]
mod tests {
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

    // PR polling tests
    #[test]
    fn test_is_feedback_request_with_keywords() {
        // Explicit feedback keywords
        assert!(is_feedback_request(
            "What do you think about this approach?"
        ));
        assert!(is_feedback_request(
            "I need some feedback on the API design"
        ));
        assert!(is_feedback_request("Thoughts? I'm not sure about this"));
        assert!(is_feedback_request("Could I get your opinion?"));
        assert!(is_feedback_request("I'm blocked on the auth issue"));
        assert!(is_feedback_request(
            "I'm stuck here, not sure how to proceed"
        ));
        assert!(is_feedback_request(
            "I have a question about the architecture"
        ));
        assert!(is_feedback_request("@Lead can you review this?"));
        assert!(is_feedback_request("Lead: what's the best approach here?"));
    }

    #[test]
    fn test_is_feedback_request_with_questions() {
        // Long questions that end with "?" and don't look like status updates should trigger
        assert!(is_feedback_request(
            "Is this the right way to handle the authentication flow in the API layer?"
        ));
        assert!(is_feedback_request(
            "Should we use async/await here or is the sync version fine for this use case?"
        ));
    }

    #[test]
    fn test_is_feedback_request_excludes_status_updates() {
        // Status updates should not trigger
        assert!(!is_feedback_request("/me claiming task 1"));
        assert!(!is_feedback_request("/me working on task 2"));
        assert!(!is_feedback_request("starting task #3"));
        assert!(!is_feedback_request("claiming #5?"));
    }

    #[test]
    fn test_is_feedback_request_excludes_short_questions() {
        // Short questions that are probably just confirmations
        assert!(!is_feedback_request("Ready?"));
        assert!(!is_feedback_request("Done?"));
    }

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
    fn test_is_directed_at_coworker_only() {
        // Messages directed at a coworker (should return true)
        assert!(is_directed_at_coworker_only(
            "@pleasant any progress on task 304?"
        ));
        assert!(is_directed_at_coworker_only(
            "@lexington can you help with this?"
        ));
        assert!(is_directed_at_coworker_only(
            "Hey @amsterdam, what's the status?"
        ));

        // Messages directed at lead (should return false)
        assert!(!is_directed_at_coworker_only("@lead what do you think?"));
        assert!(!is_directed_at_coworker_only("@Lead can you review this?"));

        // Messages mentioning both coworker and lead (should return false)
        assert!(!is_directed_at_coworker_only(
            "@lead and @lexington please check this"
        ));

        // Messages not mentioning anyone (should return false)
        assert!(!is_directed_at_coworker_only(
            "What do you think about this?"
        ));
        assert!(!is_directed_at_coworker_only("I'm stuck on this issue"));

        // Edge cases
        assert!(!is_directed_at_coworker_only("lead: what's next?")); // lead: without @
        assert!(is_directed_at_coworker_only("@york great work!")); // praise to coworker
    }

    #[test]
    fn test_pr_issue_tracker_should_nudge_new() {
        let tracker = PrIssueTracker::new();
        assert!(tracker.should_nudge(42, PrIssueType::MergeConflict));
        assert!(tracker.should_nudge(42, PrIssueType::CiFailed));
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
    fn test_pr_issue_type_display() {
        assert_eq!(PrIssueType::MergeConflict.to_string(), "merge conflict");
        assert_eq!(PrIssueType::CiFailed.to_string(), "CI failed");
        assert_eq!(
            PrIssueType::ChangesRequested.to_string(),
            "changes requested"
        );
        assert_eq!(PrIssueType::Approved.to_string(), "approved");
        assert_eq!(PrIssueType::NeedsReview.to_string(), "needs review");
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
            "spawning reviewer"
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
        // Verify SKIP_SENDERS contains expected values
        assert!(SKIP_SENDERS.contains(&"midtown"));
        assert!(SKIP_SENDERS.contains(&"system"));
        assert!(SKIP_SENDERS.contains(&"github"));
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
}
