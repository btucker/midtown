//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events, and polls PRs for
//! actionable issues.

mod chat;
mod constants;
mod dispatch;
pub(crate) mod effects;
pub(crate) mod events;
mod health;
mod helpers;
mod plugins;
mod pr;
mod rpc;
mod setup;
pub(crate) mod snapshot;
mod state;
mod trackers;
mod webhook_fwd;

use constants::*;
pub use constants::{
    DEFAULT_MAX_COWORKERS, DEFAULT_PR_POLL_INTERVAL_SECS, DEFAULT_WEBHOOK_PORT,
    DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS, MAX_CONCURRENT_REVIEWS, PR_NUDGE_COOLDOWN_SECS,
    PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS, PR_REVIEW_DELAY_SECS,
};
pub use trackers::{
    OrphanTracker, PrIssueTracker, PrIssueType, StuckConditionTracker, StuckConditionType,
};

pub use state::DaemonConfig;
pub(crate) use state::DaemonState;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, watch};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::rpc::RequestId;
use crate::webhook::{WebhookConfig, start_webhook_server};
use crate::worktree::WorktreeManager;

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
    plugins::ensure_plugins_installed().await;

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
    let pid_file = setup::acquire_pid_lock(&config.pid_file_path)?;
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
    let channel = crate::channel::Channel::for_repo(&repo_name)?;
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
    let additional_worktree_managers =
        setup::load_additional_worktree_managers(&project_name, &config);
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

    // Set up session monitor interval
    let mut session_monitor_interval = interval(SESSION_MONITOR_INTERVAL);

    // Set up lead typing indicator check interval
    let mut lead_typing_interval = interval(LEAD_TYPING_CHECK_INTERVAL);

    // Set up lead health check interval (recreates lead window if killed).
    // Track daemon start time so we can skip health checks during the startup
    // grace period, preventing races with `midtown restart` where the daemon
    // tries to respawn a lead window before the tmux session is fully settled.
    let mut lead_health_interval = interval(LEAD_HEALTH_CHECK_INTERVAL);
    let daemon_start_instant = tokio::time::Instant::now();

    // Start PR polling background task
    let (pr_poll_shutdown_tx, pr_poll_shutdown_rx) = watch::channel(false);
    {
        let state = Arc::clone(&state);
        let interval_secs = config.pr_poll_interval_secs;
        tokio::spawn(async move {
            pr::pr_poll_task(state, interval_secs, pr_poll_shutdown_rx).await;
        });
        info!(
            "PR polling started (adaptive: {}s aggressive / {}s relaxed)",
            config.pr_poll_interval_secs, RELAXED_PR_POLL_INTERVAL_SECS
        );
    }

    // Start chat monitor background task if enabled
    let (chat_monitor_shutdown_tx, chat_monitor_shutdown_rx) = watch::channel(false);
    if config.chat_monitor_enabled {
        let state = Arc::clone(&state);
        let channel_path = state.channel.channel_file_path().to_path_buf();
        tokio::spawn(async move {
            chat::chat_monitor_loop(state, channel_path, chat_monitor_shutdown_rx).await;
        });
        info!("Chat monitor started (tailf on channel.jsonl)");
    } else {
        debug!("Chat monitor disabled (MIDTOWN_CHAT_MONITOR=0)");
    }

    // Timer for periodic task dispatch
    let mut task_dispatch_interval =
        tokio::time::interval(std::time::Duration::from_secs(TASK_DISPATCH_INTERVAL_SECS));
    // Skip the first tick (which fires immediately)
    task_dispatch_interval.tick().await;

    // Timer for periodic channel rotation
    let mut channel_rotation_interval = interval(CHANNEL_ROTATION_CHECK_INTERVAL);
    // Skip the first tick (which fires immediately)
    channel_rotation_interval.tick().await;

    // Nudge any coworkers discovered from tmux to continue their tasks.
    // This runs once at startup after the daemon has fully initialized.
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            dispatch::nudge_discovered_coworkers(&state).await;
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
                        tokio::spawn(rpc::handle_connection(stream, shutdown_rx, state));
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

                // Record webhook event timestamp for adaptive PR polling.
                // The PR poll task reads this to decide between relaxed/aggressive intervals.
                {
                    let mut ts = state.last_webhook_event_at.lock().await;
                    *ts = Some(tokio::time::Instant::now());
                }

                if let Err(e) = state.send_and_broadcast(&webhook_event.message) {
                    error!("Failed to forward webhook message to channel: {}", e);
                }

                // Nudge PR owner when someone else comments on their PR
                if let Some(activity) = webhook_event.pr_activity {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        pr::handle_pr_comment_nudge(&state, activity).await;
                    });
                }

                // Queue a reviewer spawn after the delay (persisted in github-state.json)
                if let Some(pr_number) = webhook_event.needs_review {
                    let spawn_after = chrono::Utc::now()
                        + chrono::Duration::seconds(PR_REVIEW_DELAY_SECS as i64);
                    let mut github_state = state.github_state.lock().await;
                    github_state.add_pending_review_spawn(pr_number, spawn_after);
                    if let Err(e) = crate::github_state::save_state_for_repo(
                        &state.repo_name,
                        &github_state,
                    ) {
                        warn!("Failed to persist pending review spawn: {}", e);
                    }
                    info!(
                        "Webhook: PR #{} queued for review spawn in {}s",
                        pr_number, PR_REVIEW_DELAY_SECS
                    );
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
                    chat::route_mentions(&state, &nudge_msg).await;
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
                    let mut github_state = state.github_state.lock().await;
                    github_state.mark_reviewed_pr(pr_number);
                }

                // Route @mentions in webhook messages directly (chat monitor skips
                // "github" sender for loop protection, so we handle it here)
                chat::route_mentions(&state, &webhook_event.message).await;
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
                    &state,
                ).await;
            }

            // Periodically monitor coworker sessions: idle shutdown, nudges, stuck detection
            _ = session_monitor_interval.tick() => {
                // Sync internal state with actual tmux windows first
                if let Err(e) = state.coworkers.sync_with_tmux() {
                    warn!("Failed to sync coworker state with tmux: {}", e);
                }
                // event → snapshot → evaluate → execute
                let snap = snapshot::collect_world_snapshot(&state).await;
                let tick_effects = events::evaluate_tick(
                    &events::DaemonEvent::SessionMonitorTick,
                    &snap,
                    &state,
                ).await;
                effects::execute_effects(tick_effects, &state).await;

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
                    tokio::task::spawn_blocking(move || {
                        health::check_and_respawn_lead(&session, &workdir, &project, &additional);
                    }).await.ok();
                }
            }

            // Periodic task dispatch: orphan recovery, duplicate detection, spawning, cleanup
            _ = task_dispatch_interval.tick() => {
                // event → snapshot → evaluate → execute
                let snap = snapshot::collect_world_snapshot(&state).await;
                let tick_effects = events::evaluate_tick(
                    &events::DaemonEvent::TaskDispatchTick,
                    &snap,
                    &state,
                ).await;
                effects::execute_effects(tick_effects, &state).await;
                // cleanup_orphaned_worktrees is not yet effect-based
                dispatch::cleanup_orphaned_worktrees(&state);
                // Process any pending webhook review spawns whose delay has expired
                pr::process_pending_review_spawns(&state).await;
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

// ─── Pure Decision Functions ───────────────────────────────────────────────
//
// Per-coworker decision helpers for unit tests. The batch `decide_*` functions
// in `rules.rs` handle the full coworker set; these single-coworker variants
// make individual test cases easier to write.
#[cfg(test)]
mod tests {
    use super::helpers::*;
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
}
