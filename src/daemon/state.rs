//! Daemon state types: DaemonConfig, DaemonState, and PrCoworkerCache.
//!
//! These are the core data structures that hold the daemon's runtime state.
//! Extracted from mod.rs to keep the event loop module thin.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

use super::constants::*;
use super::trackers;
use crate::channel::Channel;
use crate::config;
use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::web::{self, WebUpdate};

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

/// Unified cache for PR-to-coworker mappings.
///
/// Replaces the previous separate fields (`cached_open_pr_branches`,
/// `cached_merged_pr_coworkers`) with a single struct. Merged refresh timing
/// uses the shared `CooldownTracker` rather than a standalone timestamp.
pub(super) struct PrCoworkerCache {
    /// Coworker names extracted from open PR branch names.
    /// Updated every PR poll tick (~30s).
    pub(super) open_pr_owners: HashSet<String>,
    /// Coworker names from recently merged PR branch names.
    /// Updated every `MERGED_PRS_FETCH_INTERVAL_SECS` (5 minutes via CooldownTracker).
    pub(super) merged_pr_owners: HashSet<String>,
    /// Coworker names whose open PR has all CI checks passing.
    /// Used by snapshot to determine PR break eligibility.
    pub(super) ci_passed_pr_owners: HashSet<String>,
}

impl PrCoworkerCache {
    fn new() -> Self {
        Self {
            open_pr_owners: HashSet::new(),
            merged_pr_owners: HashSet::new(),
            ci_passed_pr_owners: HashSet::new(),
        }
    }
}

/// Shared daemon state.
pub(crate) struct DaemonState {
    pub(super) coworkers: CoworkerManager,
    pub(super) channel: Channel,
    pub(super) socket_path: PathBuf,
    /// Consolidated per-coworker lifecycle state (phase + last activity).
    /// Bundles what was previously `coworker_phases` and `last_coworker_activity`
    /// into a single map. Entries are created on spawn and cleared on shutdown.
    pub(super) coworker_lifecycles: RwLock<HashMap<String, crate::rules::CoworkerLifecycle>>,
    /// Tracker to avoid spamming the same PR issues
    pub(super) pr_issue_tracker: Mutex<super::PrIssueTracker>,
    /// Repository name (primary repo)
    pub(super) repo_name: String,
    /// Default branch name (detected at startup, e.g. "main" or "master")
    pub(super) default_branch: String,
    /// Paths to all repos in the project (primary + additional)
    pub(super) all_repo_paths: Vec<PathBuf>,
    /// Unified cooldown tracker for orphan spawning and task nudge rate limiting.
    pub(super) cooldowns: std::sync::Mutex<crate::rules::CooldownTracker>,
    /// Tracks orphaned worktrees — detection time, warning cooldown, and auto-pruning
    pub(super) orphan_tracker: std::sync::RwLock<super::OrphanTracker>,
    /// Persistent GitHub state (PR reviewer assignments, etc.)
    pub(super) github_state: Mutex<crate::github_state::GitHubState>,
    /// Broadcast sender for pushing channel messages to WebSocket clients
    pub(super) web_updates_tx: Option<tokio::sync::broadcast::Sender<WebUpdate>>,
    /// Consolidated lead typing indicator state (pane hash, working flag, last activity).
    pub(super) lead_typing: std::sync::Mutex<trackers::LeadTypingState>,
    /// Maximum number of concurrent coworkers
    pub(super) max_coworkers: usize,
    /// Web Push notification manager for sending notifications to PWA clients
    /// (shared with the webserver to avoid race conditions on subscription storage)
    pub(super) push_manager: Option<std::sync::Arc<crate::push::PushManager>>,
    /// Scheduled time to nudge all coworkers after a usage limit expires.
    /// When a coworker hits an API usage/rate limit, we parse the expiry and store it here.
    /// The main loop checks this and nudges everyone when the time arrives.
    pub(super) usage_limit_nudge_at: Mutex<Option<tokio::time::Instant>>,
    /// Persistent reminder state (one-shot condition-based notifications)
    pub(super) reminder_state: std::sync::Mutex<crate::reminders::ReminderState>,
    /// Hash of the last PR poll response body, used to skip re-processing when data hasn't changed.
    /// This doesn't reduce API calls, but avoids redundant lock acquisition and issue detection
    /// when the PR state hasn't changed between poll cycles.
    pub(super) last_pr_poll_hash: Mutex<u64>,
    /// Unified cache for PR-to-coworker mappings (open + merged + CI status).
    pub(super) pr_coworker_cache: std::sync::RwLock<PrCoworkerCache>,
    /// Saved session IDs for coworkers on PR break, keyed by coworker name.
    /// When a coworker is shut down for PR break (CI passing, idle), we save their
    /// session ID here so they can be resumed with `--resume <id>` when PR activity
    /// (review comments, CI failure, etc.) requires them back.
    pub(super) pr_break_sessions: std::sync::RwLock<HashMap<String, String>>,
    /// Tracks stuck conditions that warrant nudging the lead (no review, unresolved feedback, etc.)
    pub(super) stuck_tracker: Mutex<super::StuckConditionTracker>,
    /// Per-coworker pane content hash and last-changed timestamp (for stuck detection).
    /// Maps coworker name → (last_hash, last_changed_at).
    pub(super) coworker_pane_hashes: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Cached GitHub repo full names (owner/repo) by repo path.
    /// Repo names never change during a daemon session, so we cache indefinitely.
    pub(super) repo_name_cache: std::sync::RwLock<HashMap<PathBuf, String>>,
    /// User display name from config (e.g. "Ben"). Used to recognize user @mentions
    /// and identify user-sent messages when the display name differs from "user".
    pub(super) user_display_name: Option<String>,
    /// Per-coworker zombie respawn attempt counter. Tracks how many times each
    /// coworker has been respawned as a zombie without recovering. Reset when a
    /// coworker is spawned normally (non-zombie path). Used to cap respawn loops.
    pub(super) zombie_respawn_counts: std::sync::Mutex<HashMap<String, u32>>,
    /// Timestamp of the last received webhook event (monotonic).
    /// Used by the PR poll task to determine webhook health: if recent,
    /// polling uses a relaxed interval; if stale or absent, polling is aggressive.
    pub(super) last_webhook_event_at: Mutex<Option<tokio::time::Instant>>,
}

impl DaemonState {
    /// Get the GitHub full name (owner/repo) for a repo path, using cache.
    ///
    /// On first call for a given path, runs `gh repo view --json nameWithOwner`.
    /// Subsequent calls return the cached value without any API call.
    pub(super) fn get_repo_full_name(&self, repo_path: &std::path::Path) -> String {
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
    pub(super) fn is_at_coworker_limit(&self) -> bool {
        self.coworkers.list().len() >= self.max_coworkers
    }

    /// Check if the daemon is at the dev coworker limit.
    /// Reserves `REVIEW_HEADROOM` slots for reviewers, but always allows at least 1 dev slot.
    pub(super) fn is_at_dev_limit(&self) -> bool {
        let dev_cap = self.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1);
        self.coworkers.list().len() >= dev_cap
    }

    /// Check if a PR has a review comment from a Claude coworker.
    ///
    /// Uses `github_state` as the single source of truth. First checks the
    /// persistent cache; if not found, makes GitHub API calls and caches
    /// positive results permanently (review status is monotonic).
    pub(super) async fn is_pr_reviewed(&self, pr_number: u64) -> bool {
        // Fast path: check github_state cache (single source of truth)
        {
            let github_state = self.github_state.lock().await;
            if github_state.has_cached_review(pr_number) {
                debug!(
                    "PR #{} has cached Claude review (skipping API call)",
                    pr_number
                );
                return true;
            }
        }

        // Slow path: check via API calls
        let has_review = super::pr::pr_has_claude_review_uncached(pr_number);

        // Cache positive results (review status is monotonic)
        if has_review {
            let mut github_state = self.github_state.lock().await;
            github_state.mark_reviewed_pr(pr_number);
        }

        has_review
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
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

        // Load persistent reminder state
        let reminder_path = crate::paths::reminders_file_for_repo(&repo_name);
        let reminder_state =
            crate::reminders::ReminderState::load(&reminder_path).unwrap_or_else(|e| {
                warn!("Failed to load reminders.json: {}, using defaults", e);
                crate::reminders::ReminderState::default()
            });

        let user_display_name = config::get_user_display_name_for_project(&repo_name);

        Ok(Self {
            coworkers,
            channel,
            socket_path,
            coworker_lifecycles: RwLock::new(HashMap::new()),
            pr_issue_tracker: Mutex::new(super::PrIssueTracker::new()),
            repo_name,
            default_branch,
            all_repo_paths,
            cooldowns: std::sync::Mutex::new(crate::rules::CooldownTracker::new()),
            orphan_tracker: std::sync::RwLock::new(super::OrphanTracker::new()),
            github_state: Mutex::new(github_state),
            web_updates_tx,
            max_coworkers,
            push_manager,
            usage_limit_nudge_at: Mutex::new(None),
            lead_typing: std::sync::Mutex::new(trackers::LeadTypingState::default()),
            reminder_state: std::sync::Mutex::new(reminder_state),
            last_pr_poll_hash: Mutex::new(0),
            pr_coworker_cache: std::sync::RwLock::new(PrCoworkerCache::new()),
            pr_break_sessions: std::sync::RwLock::new(HashMap::new()),
            stuck_tracker: Mutex::new(super::StuckConditionTracker::new()),
            coworker_pane_hashes: std::sync::Mutex::new(HashMap::new()),
            repo_name_cache: std::sync::RwLock::new(HashMap::new()),
            user_display_name,
            zombie_respawn_counts: std::sync::Mutex::new(HashMap::new()),
            last_webhook_event_at: Mutex::new(None),
        })
    }

    /// Spawn a coworker and initialize its lifecycle state.
    ///
    /// Wraps `CoworkerManager::spawn_with_name` and inserts a fresh
    /// `CoworkerLifecycle` entry on success, ensuring stale timestamps
    /// from any previous incarnation are replaced.
    pub(super) async fn spawn_coworker(
        &self,
        config: &crate::tmux::ClaudeLaunchConfig,
    ) -> crate::Result<()> {
        self.coworkers.spawn_with_name(config)?;
        let mut lc = self.coworker_lifecycles.write().await;
        lc.insert(
            config.name.clone(),
            crate::rules::CoworkerLifecycle::new_spawn(),
        );
        // Clear zombie respawn counter on successful spawn
        {
            let mut counts = self.zombie_respawn_counts.lock().unwrap();
            counts.remove(&config.name);
        }
        Ok(())
    }

    /// Check if a sender name represents the user (either "user" or the configured display name).
    pub(super) fn is_user_sender(&self, from: &str) -> bool {
        from.eq_ignore_ascii_case("user")
            || self
                .user_display_name
                .as_ref()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(from))
    }

    /// Send a message to the channel and broadcast it to WebSocket clients.
    pub(super) fn send_and_broadcast(&self, message: &Message) -> crate::Result<()> {
        self.channel.send(message)?;
        if let Some(ref tx) = self.web_updates_tx {
            web::broadcast_channel_message(tx, message);
        }
        Ok(())
    }

    /// Send a web push notification to all subscribed PWA clients.
    ///
    /// This is fire-and-forget: push sending runs in a background task.
    pub(super) fn send_push_notification(&self, title: &str, body: &str, tag: &str) {
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
    pub(super) fn broadcast_coworker_update(
        &self,
        name: &str,
        status: &str,
        current_task: Option<&str>,
    ) {
        if let Some(ref tx) = self.web_updates_tx {
            web::broadcast_coworker_status(tx, name, status, current_task);
        }
    }
}
