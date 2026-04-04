use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::daemon_v2::decisions::{self, Command, SpawnConfig, health, lifecycle};
use crate::daemon_v2::events::{AgentKind, DomainEvent, EventStore};
use crate::daemon_v2::executor;
use crate::daemon_v2::executor::webhook::webhook_to_events;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::rpc;
use crate::daemon_v2::scheduler::Scheduler;
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;
use crate::webhook::WebhookEvent;
use crate::worktree::WorktreeManager;
use crate::worktree_registry::{self, WorktreeAssignment, WorktreeRegistry};

/// Configuration for DaemonV2.
pub struct DaemonV2Config {
    /// The directory key (git repo name) used for path resolution.
    pub dir_key: String,
    /// Path to the Unix domain socket.
    pub socket_path: PathBuf,
    /// Directory for the event log and snapshots.
    pub events_dir: PathBuf,
    /// Default channel name (used for lead health checks).
    pub default_channel: String,
    /// Base directory for channel logs (contains `channels/` subdirectory).
    pub channels_dir: PathBuf,
    /// Optional receiver half of a webhook channel.
    ///
    /// Callers that want real-time webhook integration should:
    /// 1. Create `let (tx, rx) = tokio::sync::mpsc::channel(64);`
    /// 2. Pass `rx` here.
    /// 3. Keep `tx` and give it to the HTTP webhook server (see `src/webhook.rs`).
    ///
    /// When `None`, webhook integration is disabled.
    pub webhook_rx: Option<mpsc::Receiver<WebhookEvent>>,
    /// Optional port for the Axum web API server.
    /// When `Some`, the daemon starts an HTTP server on this port.
    pub web_port: Option<u16>,
}

/// The v2 daemon: owns the event store, projections, and scheduler.
pub struct DaemonV2 {
    config: DaemonV2Config,
    store: EventStore,
    projections: Arc<Mutex<Projections>>,
    scheduler: Scheduler,
    paths: ProjectPaths,
    sessions: HashMap<String, HeadlessSession>,
    /// Commands to resume agents that were running before the daemon restarted.
    /// Populated during `new()`, drained at the start of `run()`.
    pending_resumes: Vec<Command>,
    /// Receiver half of the webhook event channel.  Populated from
    /// `DaemonV2Config::webhook_rx`; `None` when webhook integration is disabled.
    webhook_rx: Option<mpsc::Receiver<WebhookEvent>>,
    /// Broadcast sender for domain events — feeds WebSocket clients.
    event_tx: broadcast::Sender<DomainEvent>,
    /// RPC response cache for read-only methods.
    rpc_cache: crate::daemon_v2::rpc_cache::RpcCache,
    /// Draining mode — when true, dispatch_pending_tasks is skipped.
    draining: bool,
    /// Manages git worktree creation/removal for worker isolation.
    worktree_manager: Option<WorktreeManager>,
    /// Registry tracking worktree-to-task assignments.
    worktree_registry: WorktreeRegistry,
    /// Sender for background executor results back to the main loop.
    result_tx: mpsc::Sender<executor::ExecutorResult>,
    /// Receiver for background executor results in the main loop.
    result_rx: mpsc::Receiver<executor::ExecutorResult>,
    /// Guards agents with in-flight lifecycle operations (spawn/stop).
    lifecycle_guard: executor::LifecycleGuard,
}

/// Exit status returned by [`DaemonV2::run`].
#[derive(Debug, PartialEq, Eq)]
pub enum DaemonV2ExitStatus {
    Shutdown,
}

impl DaemonV2 {
    /// Create a new DaemonV2, recovering state from the event store.
    ///
    /// If `config.webhook_rx` is `Some`, the daemon wires the receiver into
    /// its event loop so webhook events are processed in real time.  The
    /// matching sender should be retained by the caller and passed to the
    /// HTTP webhook server (see `src/webhook.rs`).
    pub fn new(mut config: DaemonV2Config) -> std::io::Result<Self> {
        let (mut store, snapshot, replay_events) = EventStore::recover(config.events_dir.clone())?;

        let mut projections = snapshot.unwrap_or_default();
        projections.apply_all(&replay_events);

        let paths = ProjectPaths::new(&config.dir_key);

        // Reconcile: agents that were "running" when the daemon last shut down
        // may have dead processes. Check PIDs and emit AgentStopped events.
        // Track which agents we reconcile so we can resume those with session_ids.
        let mut reconcile_events = Vec::new();
        let mut reconciled_agent_ids = Vec::new();
        for agent_id in projections.agents.running.clone() {
            if let Some(agent) = projections.agents.by_id.get(&agent_id) {
                let is_alive = agent
                    .pid
                    .map(|pid| {
                        std::process::Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);

                if !is_alive {
                    tracing::info!(%agent_id, name = %agent.name, "reconciling dead agent on startup");
                    reconciled_agent_ids.push(agent_id.clone());
                    reconcile_events.push(DomainEvent::AgentStopped {
                        id: agent_id.clone(),
                        reason: "process not found on startup".into(),
                    });
                }
            }
        }

        for event in &reconcile_events {
            store.append(event)?;
            projections.apply(event);
        }

        // Schedule resumes for reconciled agents that have a session_id.
        // The agent is now "stopped" in projections, but we still have the
        // session_id from before the stop (AgentStopped doesn't clear it).
        let mut pending_resumes = Vec::new();
        for agent_id in &reconciled_agent_ids {
            if let Some(agent) = projections.agents.by_id.get(agent_id)
                && agent.session_id.is_some()
            {
                tracing::info!(
                    %agent_id, name = %agent.name,
                    "scheduling resume for agent that was running before restart"
                );
                pending_resumes.push(Command::ResumeAgent {
                    id: agent_id.clone(),
                });
            }
        }

        let mut scheduler = Scheduler::new();
        // Global decisions (channel-agnostic)
        scheduler.register_global(
            "check_dead_workers",
            Duration::from_secs(30),
            health::check_dead_workers,
        );
        let max_in_progress = crate::config::load_full_project_config(&config.dir_key)
            .and_then(|c| c.default.max_in_progress_tasks())
            .unwrap_or(12);
        scheduler.register_global(
            "dispatch_pending_tasks",
            Duration::from_secs(5),
            move |proj| decisions::dispatch::dispatch_pending_tasks(proj, max_in_progress),
        );
        scheduler.register_global(
            "stop_completed_agents",
            Duration::from_secs(5),
            decisions::dispatch::stop_completed_agents,
        );
        scheduler.register_global("poll_process_health", Duration::from_secs(10), |_| {
            vec![Command::PollProcessHealth]
        });
        scheduler.register_global("poll_prs", Duration::from_secs(45), |_| {
            vec![Command::PollPrs]
        });
        scheduler.register_global(
            "handle_merged_prs",
            Duration::from_secs(10),
            decisions::prs::handle_merged_prs,
        );
        scheduler.register_global(
            "spawn_reviewers",
            Duration::from_secs(45),
            decisions::prs::spawn_reviewers,
        );
        scheduler.register_global(
            "resume_dead_reviewers",
            Duration::from_secs(30),
            decisions::prs::resume_dead_reviewers,
        );
        scheduler.register_global(
            "nudge_rebase_after_merge",
            Duration::from_secs(30),
            decisions::prs::nudge_rebase_after_merge,
        );
        scheduler.register_global(
            "suspend_authors_with_prs",
            Duration::from_secs(10),
            decisions::prs::suspend_authors_with_prs,
        );
        scheduler.register_global(
            "check_idle_workers",
            Duration::from_secs(30),
            health::check_idle_workers,
        );
        scheduler.register_global("check_duplicate_workers", Duration::from_secs(30), |proj| {
            let mut cmds = decisions::dispatch::check_duplicate_workers(proj);
            cmds.extend(decisions::dispatch::check_duplicate_leads(proj));
            cmds
        });
        scheduler.register_global(
            "check_auth_errors",
            Duration::from_secs(30),
            health::check_auth_errors,
        );
        scheduler.register_global(
            "check_usage_limits",
            Duration::from_secs(60),
            health::check_usage_limits,
        );
        scheduler.register_global(
            "nudge_stale_workers",
            Duration::from_secs(300),
            health::nudge_stale_workers,
        );
        scheduler.register_global(
            "stop_idle_reported_workers",
            Duration::from_secs(30),
            health::stop_idle_reported_workers,
        );
        scheduler.register_global(
            "detect_stale_ci",
            Duration::from_secs(60),
            decisions::prs::detect_stale_ci,
        );
        // Channel-aware decisions
        scheduler.register(
            "ensure_channel_leads_alive",
            Duration::from_secs(30),
            health::ensure_channel_leads_alive,
        );
        scheduler.register(
            "garbage_collect",
            Duration::from_secs(3600),
            lifecycle::gc_decision,
        );

        // Move the receiver out so `config` can be stored on the daemon.
        let webhook_rx = config.webhook_rx.take();

        // Broadcast channel for domain events (feeds WebSocket clients).
        let (event_tx, _) = broadcast::channel::<DomainEvent>(256);

        let worktree_manager = match WorktreeManager::from_current_dir() {
            Ok(wm) => {
                tracing::info!(repo = %wm.repo_name(), "worktree manager initialized");
                Some(wm)
            }
            Err(e) => {
                tracing::warn!(%e, "failed to initialize worktree manager — workers will use repo root");
                None
            }
        };
        let worktree_registry = WorktreeRegistry::default();

        let projections = Arc::new(Mutex::new(projections));

        let (result_tx, result_rx) = tokio::sync::mpsc::channel::<executor::ExecutorResult>(64);

        Ok(Self {
            config,
            store,
            projections,
            scheduler,
            paths,
            sessions: HashMap::new(),
            pending_resumes,
            webhook_rx,
            event_tx,
            rpc_cache: crate::daemon_v2::rpc_cache::RpcCache::new(std::time::Duration::from_secs(
                2,
            )),
            draining: false,
            worktree_manager,
            worktree_registry,
            result_tx,
            result_rx,
            lifecycle_guard: executor::LifecycleGuard::new(),
        })
    }

    /// Run the event loop. Drives the Unix socket listener and the scheduler.
    /// Returns when a shutdown request is received.
    pub async fn run(mut self) -> DaemonV2ExitStatus {
        // Remove a stale socket file if it exists.
        let _ = std::fs::remove_file(&self.config.socket_path);

        // Write and lock PID file so the shared webserver can discover this project.
        // The exclusive lock is held for the daemon's lifetime — `is_pid_locked()` checks it.
        let pid_file_path = self.paths.base_dir().join("daemon.pid");
        if let Some(parent) = pid_file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pid_file_path, std::process::id().to_string());
        let _pid_lock = {
            use fs2::FileExt;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .open(&pid_file_path)
                .expect("failed to open PID file for locking");
            f.lock_exclusive().expect("failed to lock PID file");
            f // Hold the lock for the daemon's lifetime
        };

        // Ensure the socket directory exists.
        if let Some(parent) = self.config.socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener =
            UnixListener::bind(&self.config.socket_path).expect("failed to bind daemon socket");

        tracing::info!(socket = %self.config.socket_path.display(), "DaemonV2 listening");

        // Start the web server. The shared webserver (port 47022) proxies HTTP
        // to this port, so it must be running for the web UI to work.
        // Use explicit --web-port, or fall back to the project's webhook_port from config.
        let web_port = self.config.web_port.or_else(|| {
            crate::config::load_full_project_config(self.paths.dir_key())
                .and_then(|c| c.daemon.webhook_port)
        });
        // Command channel: the web layer sends commands (nudges) to the daemon event loop.
        let (web_cmd_tx, mut web_cmd_rx) =
            tokio::sync::mpsc::channel::<crate::daemon_v2::decisions::Command>(64);

        if let Some(port) = web_port {
            // Resolve repo full name (owner/repo) once at startup for PR link generation.
            let repo_full_name = tokio::task::spawn_blocking(|| {
                std::process::Command::new("gh")
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
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();

            let web_state = std::sync::Arc::new(crate::daemon_v2::web::WebState {
                projections: self.projections.clone(),
                channels_dir: self.config.channels_dir.clone(),
                event_tx: self.event_tx.clone(),
                command_tx: web_cmd_tx.clone(),
                repo_name: self.config.dir_key.clone(),
                repo_full_name,
            });
            let router = crate::daemon_v2::web::create_router(web_state);
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            let tcp_listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("failed to bind web server port");
            tracing::info!(%port, "DaemonV2 web API listening");
            tokio::spawn(async move {
                axum::serve(tcp_listener, router).await.ok();
            });
        }

        // Start the webhook forwarder watchdog if we have a web port.
        // The forwarder runs `gh webhook forward` to receive GitHub events,
        // which get routed through the same nudge system as everything else.
        let (_webhook_shutdown_tx, webhook_shutdown_rx) = tokio::sync::watch::channel(false);
        if let Some(port) = web_port {
            let webhook_port = port;
            let restart_secs = crate::config::load_full_project_config(self.paths.dir_key())
                .and_then(|c| c.daemon.webhook_restart_interval_secs)
                .unwrap_or(300);

            // Only start if MIDTOWN_WEBHOOK_PORT != 0 (0 means disabled for tests)
            let disabled = std::env::var("MIDTOWN_WEBHOOK_PORT")
                .map(|v| v == "0")
                .unwrap_or(false);

            if !disabled {
                tokio::spawn(async move {
                    crate::webhook_fwd::webhook_forwarder_watchdog(
                        webhook_port,
                        restart_secs,
                        webhook_shutdown_rx,
                    )
                    .await;
                });
                tracing::info!(%webhook_port, "webhook forwarder watchdog started");
            }
        }

        // Queue pending resumes for processing during the first event loop iterations.
        // Don't block startup — the daemon needs to accept RPC connections immediately.
        let mut pending_resumes = std::mem::take(&mut self.pending_resumes);

        loop {
            // Process one pending resume per loop iteration (non-blocking startup)
            if let Some(cmd) = pending_resumes.pop() {
                self.dispatch_command(cmd).await;
            }
            // Use zero deadline while processing pending resumes to avoid sleeping
            let deadline = if !pending_resumes.is_empty() {
                Duration::ZERO
            } else {
                self.scheduler
                    .next_deadline(Instant::now())
                    .unwrap_or(Duration::from_secs(30))
            };

            let sleep = tokio::time::sleep(deadline);
            tokio::pin!(sleep);

            // Build a future for the webhook receiver that resolves when a
            // webhook event arrives, or never resolves when disabled.
            let webhook_recv: std::pin::Pin<
                Box<dyn std::future::Future<Output = Option<WebhookEvent>> + Send>,
            > = match self.webhook_rx.as_mut() {
                Some(rx) => Box::pin(async move { rx.recv().await }),
                None => Box::pin(std::future::pending()),
            };

            // Spec 8.1: biased select prioritizes RPC/web commands over the
            // scheduler tick, ensuring user messages are dispatched within 5s.
            tokio::select! {
                biased;

                // Commands from the web layer — highest priority (user-initiated)
                Some(cmd) = web_cmd_rx.recv() => {
                    self.dispatch_command(cmd).await;
                }

                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            // Spec 8.1: don't hold projections lock across socket I/O.
                            // handle_rpc_connection reads the request, dispatches
                            // (briefly locking projections), and writes the response.
                            let (outcome, events, commands) =
                                handle_rpc_connection(
                                    stream,
                                    &self.projections,
                                    &self.config.channels_dir,
                                    &mut self.rpc_cache,
                                ).await;

                            // Check for daemon.set-draining (managed at daemon level)
                            if outcome == RpcOutcome::SetDraining(true) {
                                self.draining = true;
                                tracing::info!("daemon entering draining mode");
                            } else if outcome == RpcOutcome::SetDraining(false) {
                                self.draining = false;
                                tracing::info!("daemon exiting draining mode");
                            }

                            self.apply_events(&events).await;
                            for command in commands {
                                self.dispatch_command(command).await;
                            }
                            if outcome == RpcOutcome::Shutdown {
                                tracing::info!("shutdown requested via RPC");
                                return DaemonV2ExitStatus::Shutdown;
                            }
                        }
                        Err(e) => {
                            tracing::error!(%e, "accept error");
                        }
                    }
                }

                maybe_event = webhook_recv => {
                    match maybe_event {
                        Some(webhook_event) => {
                            tracing::debug!("webhook event received");
                            let domain_events = webhook_to_events(&webhook_event);
                            self.apply_events(&domain_events).await;
                        }
                        None => {
                            // Channel closed — disable future webhook receives.
                            tracing::debug!("webhook channel closed");
                            self.webhook_rx = None;
                        }
                    }
                }

                // Results from background executor tasks (spawns, stops, PR polls, etc.)
                Some(result) = self.result_rx.recv() => {
                    match result {
                        executor::ExecutorResult::Events { events, lifecycle_key } => {
                            self.handle_worktree_cleanup(&events);
                            self.apply_events(&events).await;
                            // Clear lifecycle guard on failure (P1 fix)
                            if let Some(key) = lifecycle_key {
                                let stashed = self.lifecycle_guard.complete(&key);
                                for msg in stashed {
                                    self.dispatch_nudge(&key, &msg).await;
                                }
                            }
                        }
                        executor::ExecutorResult::SessionReady { id, session, events, lifecycle_key } => {
                            self.sessions.insert(id.clone(), *session);
                            self.handle_worktree_cleanup(&events);
                            self.apply_events(&events).await;
                            self.auto_assign_tasks(&events).await;
                            // Use lifecycle_key if set (P2 fix: respawn uses old
                            // agent ID as key, but SessionReady has new ID)
                            let guard_key = lifecycle_key.as_deref().unwrap_or(&id);
                            let stashed = self.lifecycle_guard.complete(guard_key);
                            for msg in stashed {
                                if let Some(s) = self.sessions.get_mut(&id)
                                    && let Err(e) = s.send_message(&msg).await
                                {
                                    tracing::error!(%id, %e, "failed to deliver stashed nudge");
                                }
                            }
                        }
                        executor::ExecutorResult::LifecycleComplete { id, events } => {
                            self.handle_worktree_cleanup(&events);
                            self.apply_events(&events).await;
                            let stashed = self.lifecycle_guard.complete(&id);
                            for msg in stashed {
                                self.dispatch_nudge(&id, &msg).await;
                            }
                        }
                    }
                }

                () = &mut sleep => {
                    self.run_due_decisions().await;
                }
            }
        }
    }

    /// Apply a batch of domain events to the event store and projections.
    fn apply_events<'a>(
        &'a mut self,
        events: &'a [crate::daemon_v2::events::DomainEvent],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let commands = self.apply_events_core(events).await;
            for cmd in commands {
                self.dispatch_command(cmd).await;
            }
        })
    }

    async fn apply_events_core(
        &mut self,
        events: &[crate::daemon_v2::events::DomainEvent],
    ) -> Vec<Command> {
        if events.is_empty() {
            return vec![];
        }
        let mut deferred_commands: Vec<Command> = Vec::new();
        let mut proj = self.projections.lock().await;
        for event in events {
            if let Err(e) = self.store.append(event) {
                tracing::error!(%e, "failed to append event");
            }
            proj.apply(event);
            // Broadcast to WebSocket clients (ignore if no receivers).
            let _ = self.event_tx.send(event.clone());

            // Record SpawnFailure cooldown when an agent dies shortly after starting.
            // For leads, keyed by channel. For workers, keyed by task ID.
            // This prevents tight respawn loops.
            if let DomainEvent::AgentStopped { id, .. } = event
                && let Some(agent) = proj.agents.by_id.get(id)
            {
                let died_quickly = agent
                    .started_at
                    .is_some_and(|t| (chrono::Utc::now() - t).num_seconds() < 60);

                let agent_kind = agent.kind.clone();
                let cooldown_key = match agent_kind {
                    crate::daemon_v2::events::AgentKind::Lead => agent.channel.clone(),
                    crate::daemon_v2::events::AgentKind::Worker => agent.task_id.clone(),
                    _ => None,
                };

                if died_quickly {
                    if let Some(ref key) = cooldown_key {
                        tracing::warn!(
                            %id, key = %key, kind = ?agent_kind,
                            "agent died within 60s of start — applying spawn cooldown"
                        );
                        proj.cooldowns.record(
                            crate::daemon_v2::projections::cooldowns::CooldownCategory::SpawnFailure,
                            key.clone(),
                        );

                        // Record escalation cooldown once max failures reached,
                        // preventing ops channel spam on every subsequent tick.
                        let failures = proj.cooldowns.failure_count(
                            crate::daemon_v2::projections::cooldowns::CooldownCategory::SpawnFailure,
                            key,
                        );
                        let max_restarts = match agent_kind {
                            crate::daemon_v2::events::AgentKind::Lead => {
                                crate::daemon_v2::decisions::health::MAX_LEAD_RESTARTS
                            }
                            _ => crate::daemon_v2::decisions::health::MAX_WORKER_RESTARTS,
                        };
                        if failures >= max_restarts {
                            let esc_key = match agent_kind {
                                crate::daemon_v2::events::AgentKind::Lead => {
                                    format!("lead-escalation-{key}")
                                }
                                _ => format!("worker-escalation-{key}"),
                            };
                            proj.cooldowns.record(
                                crate::daemon_v2::projections::cooldowns::CooldownCategory::TaskNudge,
                                esc_key,
                            );
                        }
                    }
                } else if let Some(ref key) = cooldown_key {
                    // Agent survived past 60s — reset failure streak so
                    // transient issues don't accumulate into a permanent block.
                    proj.cooldowns.reset_count(
                        crate::daemon_v2::projections::cooldowns::CooldownCategory::SpawnFailure,
                        key,
                    );
                }
            }

            // Log when a resume fails because the session doesn't exist in the
            // current project directory. The actual session_id clearing happens in
            // AgentIndex::apply() so it works during both live processing AND replay.
            if let DomainEvent::AgentSessionNotFound { name } = event
                && let Some(agent_id) = proj.agents.by_name.get(name)
            {
                tracing::warn!(
                    %agent_id, %name,
                    "clearing session_id — conversation not found in project dir"
                );
            }

            // Route @mentions and !task references in auto-output messages.
            // These bypass the channel.post RPC path (which does its own routing),
            // so we handle them here to ensure mentions in agent output still
            // trigger nudges. Skip "user" and "midtown" senders (already routed
            // or system messages).
            if let DomainEvent::MessagePosted {
                channel,
                sender,
                content,
                thread_id,
                auto_output: true,
                ..
            } = event
                && sender != "user"
                && sender != "midtown"
                && sender != "system"
                && (content.contains('@') || content.contains('!'))
            {
                let commands = crate::daemon_v2::decisions::chat::route_message(
                    &proj,
                    channel,
                    sender,
                    content,
                    thread_id.as_deref(),
                    None,
                );
                deferred_commands.extend(commands);
            }
        }
        drop(proj);
        deferred_commands
    }

    /// Run all currently due decisions, execute the resulting commands, and
    /// apply the produced events to the event store and projections.
    async fn run_due_decisions(&mut self) {
        let now = Instant::now();
        // Collect due decisions into owned data to release the scheduler borrow
        // before calling mark_ran and dispatch_command (which need &mut self).
        let due: Vec<(&'static str, Vec<Command>)> = {
            let proj = self.projections.lock().await;
            self.scheduler
                .due_decisions(now)
                .into_iter()
                .map(|d| {
                    let cmds = if self.draining && d.name == "dispatch_pending_tasks" {
                        vec![]
                    } else {
                        (d.run)(&proj, &self.config.default_channel)
                    };
                    (d.name, cmds)
                })
                .collect()
        };

        for (name, commands) in due {
            self.scheduler.mark_ran(name, now);
            for command in commands {
                self.dispatch_command(command).await;
            }
        }
    }

    /// Create a worktree for a SpawnAgent command and set its `working_dir`.
    ///
    /// - Workers with a task_id get a task worktree (branched).
    /// - Leads get the shared lead worktree.
    fn prepare_worktree_for_spawn(&mut self, config: &mut SpawnConfig) {
        let Some(ref wm) = self.worktree_manager else {
            return;
        };

        match config.kind {
            AgentKind::Worker => {
                let Some(ref task_id) = config.task_id else {
                    return;
                };

                // If this task already has a worktree (e.g. re-dispatch after reset),
                // reuse it instead of creating a new one.
                if let Some(wt_id) = self.worktree_registry.find_worktree_by_task(task_id) {
                    let path = wm.task_worktree_path(&wt_id);
                    config.working_dir = Some(path.to_string_lossy().to_string());
                    // Rebind the coworker (old one is dead if we're re-dispatching).
                    let _ = self
                        .worktree_registry
                        .force_rebind_coworker(&wt_id, &config.name);
                    tracing::info!(
                        %task_id,
                        worktree = %wt_id,
                        "reusing existing worktree for re-dispatched task"
                    );
                    return;
                }

                let subject = config.initial_prompt.as_deref().unwrap_or("task");
                let slug = worktree_registry::branch_slug_for_task(task_id, subject);

                match wm.create_task_worktree(&slug) {
                    Ok(path) => {
                        config.working_dir = Some(path.to_string_lossy().to_string());
                        let assignment = WorktreeAssignment {
                            worktree_id: slug.clone(),
                            branch_name: slug,
                            task_id: Some(task_id.clone()),
                            current_coworker: Some(config.name.clone()),
                            pr_number: None,
                            created_at: chrono::Utc::now(),
                            completed_at: None,
                        };
                        if let Err(e) = self.worktree_registry.assign_worktree(assignment) {
                            tracing::warn!(%task_id, %e, "worktree registry assignment failed");
                        }
                        tracing::info!(%task_id, worktree = %path.display(), "created task worktree");
                    }
                    Err(e) => {
                        tracing::error!(%task_id, %e, "failed to create task worktree");
                    }
                }
            }
            AgentKind::Lead | AgentKind::Fork => {
                // Only set working_dir from the worktree manager if it isn't
                // already set (e.g. by a channel directory override).
                if config.working_dir.is_some() {
                    return;
                }
                match wm.create_lead_worktree() {
                    Ok(path) => {
                        config.working_dir = Some(path.to_string_lossy().to_string());
                        tracing::info!(worktree = %path.display(), "lead worktree ready");
                    }
                    Err(e) => {
                        tracing::error!(%e, "failed to create lead worktree");
                    }
                }
            }
        }
    }

    /// After executing a command, check for TaskCompleted events and clean up
    /// the associated worktree.
    fn handle_worktree_cleanup(&mut self, events: &[DomainEvent]) {
        for event in events {
            if let DomainEvent::TaskCompleted { task_id } = event
                && let Some(wt_id) = self.worktree_registry.find_worktree_by_task(task_id)
            {
                // Mark as completed (for potential time-based deferred cleanup).
                self.worktree_registry
                    .mark_completed(&wt_id, chrono::Utc::now());
                // Remove the git worktree from disk.
                if let Some(ref wm) = self.worktree_manager {
                    match wm.remove_task_worktree(&wt_id, false) {
                        Ok(()) => {
                            tracing::info!(%task_id, %wt_id, "removed task worktree");
                        }
                        Err(e) => {
                            tracing::warn!(%task_id, %wt_id, %e, "failed to remove task worktree");
                        }
                    }
                }
                self.worktree_registry.remove_worktree(&wt_id);
            }
        }
    }

    // ── Non-blocking command dispatch ─────────────────────────────────

    /// Classify and dispatch a command: inline commands execute immediately,
    /// background commands are spawned as tokio tasks, and nudges are resolved
    /// at dispatch time.
    async fn dispatch_command(&mut self, command: Command) {
        use executor::{CommandClass, classify_command};
        // PersistEvents: store-only — projections are already updated by the web layer.
        if let Command::PersistEvents(events) = command {
            for event in &events {
                if let Err(e) = self.store.append(event) {
                    tracing::error!(%e, "failed to persist web event");
                }
            }
            return;
        }
        match classify_command(&command) {
            CommandClass::Inline => {
                let events = executor::execute_inline(
                    command,
                    &mut self.sessions,
                    &self.config.channels_dir,
                );
                self.handle_worktree_cleanup(&events);
                self.apply_events(&events).await;
                self.auto_assign_tasks(&events).await;
            }
            CommandClass::Background => {
                self.dispatch_background(command).await;
            }
            CommandClass::NeedsResolution => {
                if let Command::NudgeAgent { id, message } = command {
                    self.dispatch_nudge(&id, &message).await;
                }
            }
        }
    }

    /// Dispatch a background command by spawning a tokio task that sends
    /// results back via `self.result_tx`.
    async fn dispatch_background(&mut self, command: Command) {
        match command {
            Command::SpawnAgent(mut config) => {
                // If a spawn for this agent name is already in-flight, stash
                // the initial prompt as a nudge instead of spawning a duplicate.
                if self.lifecycle_guard.is_pending(&config.name) {
                    if let Some(prompt) = &config.initial_prompt {
                        tracing::info!(
                            name = %config.name,
                            "spawn already in-flight — stashing message as nudge"
                        );
                        self.lifecycle_guard
                            .stash_nudge(&config.name, prompt.clone());
                    }
                    return;
                }
                self.prepare_worktree_for_spawn(&mut config);
                let key = config.name.clone();
                self.lifecycle_guard.mark_pending(key.clone());
                executor::spawn_background_agent(
                    config,
                    self.paths.clone(),
                    self.config.channels_dir.clone(),
                    self.event_tx.clone(),
                    self.result_tx.clone(),
                    Some(key),
                );
            }
            Command::ResumeAgent { id } => {
                let agent = {
                    let proj = self.projections.lock().await;
                    proj.agents.by_id.get(&id).cloned()
                };
                if let Some(agent) = agent {
                    // Derive the worktree path for resumed agents (same as fresh spawns).
                    // Channel directory overrides must be set first so
                    // prepare_worktree_for_spawn doesn't overwrite them.
                    let mut config = executor::spawn_config_from_agent(&agent);
                    if let Some(ref ch) = agent.channel {
                        let proj = self.projections.lock().await;
                        if let Some(dir) = proj.channels.channel_directory(ch) {
                            config.working_dir = Some(dir.to_string());
                        }
                    }
                    self.prepare_worktree_for_spawn(&mut config);
                    self.lifecycle_guard.mark_pending(id.clone());
                    let working_dir = config.working_dir.clone();
                    executor::spawn_background_resume(
                        id,
                        agent,
                        self.paths.clone(),
                        self.config.channels_dir.clone(),
                        self.event_tx.clone(),
                        self.result_tx.clone(),
                        None, // id matches — no alias needed
                        working_dir,
                    );
                }
            }
            Command::StopAgent { id, reason } => {
                if let Some(session) = self.sessions.remove(&id) {
                    self.lifecycle_guard.mark_pending(id.clone());
                    executor::spawn_background_stop(id, reason, session, self.result_tx.clone());
                }
            }
            Command::PollPrs => {
                let work = {
                    let proj = self.projections.lock().await;
                    proj.work.clone()
                };
                executor::spawn_background_poll_prs(work, self.result_tx.clone());
            }
            Command::MergePr { .. } | Command::PostPrComment { .. } | Command::RerunCi { .. } => {
                executor::spawn_background_gh_command(command, self.result_tx.clone());
            }
            other => {
                // Shouldn't happen — classified as Background but not matched above.
                // Fallback to inline execution.
                tracing::warn!(
                    ?other,
                    "dispatch_background called with unhandled command, falling back to inline"
                );
                let events =
                    executor::execute_inline(other, &mut self.sessions, &self.config.channels_dir);
                self.apply_events(&events).await;
            }
        }
    }

    /// Resolve and dispatch a nudge: deliver to running agents, resume stopped
    /// agents, or stash if a lifecycle operation is in-flight.
    async fn dispatch_nudge(&mut self, id: &str, message: &str) {
        // Check by ID first, then by agent name (spawns key by name, not ID)
        if self.lifecycle_guard.is_pending(id) {
            self.lifecycle_guard.stash_nudge(id, message.to_string());
            return;
        }
        let agent_name = {
            let proj = self.projections.lock().await;
            proj.agents.by_id.get(id).map(|a| a.name.clone())
        };
        if let Some(ref name) = agent_name
            && self.lifecycle_guard.is_pending(name)
        {
            tracing::info!(%id, %name, "nudge stashed — agent spawn in-flight (keyed by name)");
            self.lifecycle_guard.stash_nudge(name, message.to_string());
            return;
        }
        let action = {
            let proj = self.projections.lock().await;
            executor::resolve_nudge_action(id, &proj)
        };
        match action {
            executor::NudgeAction::Deliver => {
                if let Some(session) = self.sessions.get_mut(id)
                    && let Err(e) = session.send_message(message).await
                {
                    tracing::error!(%id, %e, "failed to deliver nudge");
                }
            }
            executor::NudgeAction::ResumeAndDeliver { .. } => {
                let agent = {
                    let proj = self.projections.lock().await;
                    proj.agents.by_id.get(id).cloned()
                };
                if let Some(agent) = agent {
                    let mut config = executor::spawn_config_from_agent(&agent);
                    if let Some(ref ch) = agent.channel {
                        let proj = self.projections.lock().await;
                        if let Some(dir) = proj.channels.channel_directory(ch) {
                            config.working_dir = Some(dir.to_string());
                        }
                    }
                    self.prepare_worktree_for_spawn(&mut config);
                    self.lifecycle_guard.mark_pending(id.to_string());
                    self.lifecycle_guard.stash_nudge(id, message.to_string());
                    executor::spawn_background_resume(
                        id.to_string(),
                        agent,
                        self.paths.clone(),
                        self.config.channels_dir.clone(),
                        self.event_tx.clone(),
                        self.result_tx.clone(),
                        None, // id matches — no alias needed
                        config.working_dir,
                    );
                }
            }
            executor::NudgeAction::RespawnAndDeliver { config } => {
                let key = id.to_string();
                self.lifecycle_guard.mark_pending(key.clone());
                self.lifecycle_guard.stash_nudge(id, message.to_string());
                executor::spawn_background_agent(
                    *config,
                    self.paths.clone(),
                    self.config.channels_dir.clone(),
                    self.event_tx.clone(),
                    self.result_tx.clone(),
                    Some(key), // old agent ID — new agent gets a different ID
                );
            }
            executor::NudgeAction::Drop => {
                tracing::debug!(%id, "nudge target unknown, dropping");
            }
        }
    }

    /// Emit TaskAssigned events for any AgentCreated events that have a task_id.
    async fn auto_assign_tasks(&mut self, events: &[DomainEvent]) {
        let mut assign_events = Vec::new();
        for event in events {
            if let DomainEvent::AgentCreated {
                id,
                task_id: Some(tid),
                ..
            } = event
            {
                assign_events.push(DomainEvent::TaskAssigned {
                    task_id: tid.clone(),
                    agent_id: id.clone(),
                });
            }
        }
        if !assign_events.is_empty() {
            self.apply_events(&assign_events).await;
        }
    }
}

// ── RPC connection handling ────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum RpcOutcome {
    Continue,
    Shutdown,
    SetDraining(bool),
}

/// Read one JSON-RPC request from `stream`, dispatch it, and write the response.
/// Returns `RpcOutcome::Shutdown` if the request was a shutdown request, plus any
/// domain events produced by mutating RPC methods (e.g., `task.create`), and any
/// commands to execute (e.g., `session.fork` spawning a new agent).
async fn handle_rpc_connection(
    mut stream: UnixStream,
    projections: &Arc<Mutex<Projections>>,
    channels_dir: &Path,
    rpc_cache: &mut crate::daemon_v2::rpc_cache::RpcCache,
) -> (
    RpcOutcome,
    Vec<crate::daemon_v2::events::DomainEvent>,
    Vec<crate::daemon_v2::decisions::Command>,
) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    // Read until the connection closes or we have a complete JSON object.
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                // Attempt a parse; if it succeeds we have a complete request.
                if serde_json::from_slice::<serde_json::Value>(&buf).is_ok() {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(%e, "RPC read error");
                return (RpcOutcome::Continue, vec![], vec![]);
            }
        }
    }

    let request: serde_json::Value = match serde_json::from_slice(&buf) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%e, "malformed RPC request");
            return (RpcOutcome::Continue, vec![], vec![]);
        }
    };

    let method = request
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // Handle daemon.set-draining before dispatching
    if method == "daemon.set-draining" {
        let draining = request
            .get("params")
            .and_then(|p| p.get("draining"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "result": { "ok": true, "draining": draining },
            "id": id
        });
        let _ = stream
            .write_all(serde_json::to_vec(&response).unwrap_or_default().as_slice())
            .await;
        let _ = stream.shutdown().await;
        return (RpcOutcome::SetDraining(draining), vec![], vec![]);
    }

    // Check for the special "shutdown" method before dispatching.
    let is_shutdown = method == "shutdown";

    if is_shutdown {
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "result": { "ok": true },
            "id": id
        });
        let _ = write_response(&mut stream, &response).await;
        return (RpcOutcome::Shutdown, vec![], vec![]);
    }

    // Build cache key from method + params (fixes: different params returning wrong data)
    let request_id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let cache_key = {
        let params_str = request
            .get("params")
            .map(|p| p.to_string())
            .unwrap_or_default();
        format!("{method}:{params_str}")
    };

    // Check cache for read-only methods
    if crate::daemon_v2::rpc_cache::RpcCache::is_cacheable(&method)
        && let Some(cached) = rpc_cache.get(&cache_key)
    {
        // Patch the response ID to match this caller's request (fixes: stale ID from cache)
        let mut patched = cached.clone();
        if let Some(obj) = patched.as_object_mut() {
            obj.insert("id".to_string(), request_id);
        }
        let _ = write_response(&mut stream, &patched).await;
        return (RpcOutcome::Continue, vec![], vec![]);
    }

    // Spec 8.1: lock projections only for the dispatch call, not across I/O
    let (response, events, commands) = {
        let proj = projections.lock().await;
        rpc::dispatch_request(request, &proj, channels_dir)
    };

    // Cache read-only responses
    if crate::daemon_v2::rpc_cache::RpcCache::is_cacheable(&method)
        && events.is_empty()
        && commands.is_empty()
    {
        rpc_cache.set(cache_key, response.clone());
    }

    // Invalidate cache on mutations
    if !events.is_empty() || !commands.is_empty() {
        rpc_cache.invalidate_all();
    }

    let _ = write_response(&mut stream, &response).await;

    (RpcOutcome::Continue, events, commands)
}

async fn write_response(
    stream: &mut UnixStream,
    response: &serde_json::Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(response)?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}
