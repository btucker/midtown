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
    /// Manages git worktree creation/removal for worker isolation.
    worktree_manager: Option<WorktreeManager>,
    /// Registry tracking worktree-to-task assignments.
    worktree_registry: WorktreeRegistry,
}

/// Exit status returned by [`DaemonV2::run`].
#[derive(Debug, PartialEq, Eq)]
pub enum DaemonV2ExitStatus {
    Shutdown,
}

/// Wrapper matching `DecisionFn = fn(&Projections, &str) -> Vec<Command>`.
/// Ignores the channel argument; delegates to the channel-agnostic health check.
fn check_dead_workers_fn(
    proj: &Projections,
    _channel: &str,
) -> Vec<crate::daemon_v2::decisions::Command> {
    health::check_dead_workers(proj)
}

/// Wrapper matching `DecisionFn`.
fn ensure_channel_leads_alive_fn(proj: &Projections, channel: &str) -> Vec<Command> {
    health::ensure_channel_leads_alive(proj, channel)
}

/// Wrapper: dispatch pending tasks (up to 3 concurrent workers).
fn dispatch_pending_tasks_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::dispatch::dispatch_pending_tasks(proj, 3)
}

/// Wrapper: stop agents whose tasks have completed.
fn stop_completed_agents_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::dispatch::stop_completed_agents(proj)
}

/// Wrapper: stop workers that have been running without a task for > 5 minutes.
fn check_idle_workers_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    health::check_idle_workers(proj)
}

/// Wrapper: stop the older of two agents assigned to the same task.
fn check_duplicate_workers_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::dispatch::check_duplicate_workers(proj)
}

/// Wrapper: detect auth errors from session stderr (stub).
fn check_auth_errors_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    health::check_auth_errors(proj)
}

/// Wrapper: detect usage limits from session output (stub).
fn check_usage_limits_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    health::check_usage_limits(proj)
}

/// Wrapper: poll process health for all running sessions.
fn poll_process_health_fn(_proj: &Projections, _channel: &str) -> Vec<Command> {
    vec![Command::PollProcessHealth]
}

/// Wrapper: poll GitHub PRs.
fn poll_prs_fn(_proj: &Projections, _channel: &str) -> Vec<Command> {
    vec![Command::PollPrs]
}

/// Wrapper: complete tasks for merged PRs.
fn handle_merged_prs_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::prs::handle_merged_prs(proj)
}

/// Wrapper: spawn reviewer agents for PRs needing review.
fn spawn_reviewers_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::prs::spawn_reviewers(proj)
}

/// Wrapper: suspend author agents whose tasks have open PRs awaiting review.
fn suspend_authors_with_prs_fn(proj: &Projections, _channel: &str) -> Vec<Command> {
    decisions::prs::suspend_authors_with_prs(proj)
}

/// Wrapper: garbage collect old stopped agent records.
fn garbage_collect_fn(proj: &Projections, channel: &str) -> Vec<Command> {
    lifecycle::gc_decision(proj, channel)
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
        scheduler.register(
            "check_dead_workers",
            Duration::from_secs(30),
            check_dead_workers_fn,
        );
        scheduler.register(
            "ensure_channel_leads_alive",
            Duration::from_secs(30),
            ensure_channel_leads_alive_fn,
        );
        scheduler.register(
            "dispatch_pending_tasks",
            Duration::from_secs(5),
            dispatch_pending_tasks_fn,
        );
        scheduler.register(
            "stop_completed_agents",
            Duration::from_secs(5),
            stop_completed_agents_fn,
        );
        scheduler.register(
            "poll_process_health",
            Duration::from_secs(10),
            poll_process_health_fn,
        );
        scheduler.register("poll_prs", Duration::from_secs(45), poll_prs_fn);
        scheduler.register(
            "handle_merged_prs",
            Duration::from_secs(10),
            handle_merged_prs_fn,
        );
        scheduler.register(
            "spawn_reviewers",
            Duration::from_secs(45),
            spawn_reviewers_fn,
        );
        scheduler.register(
            "suspend_authors_with_prs",
            Duration::from_secs(10),
            suspend_authors_with_prs_fn,
        );
        scheduler.register(
            "garbage_collect",
            Duration::from_secs(3600),
            garbage_collect_fn,
        );
        scheduler.register(
            "check_idle_workers",
            Duration::from_secs(30),
            check_idle_workers_fn,
        );
        scheduler.register(
            "check_duplicate_workers",
            Duration::from_secs(30),
            check_duplicate_workers_fn,
        );
        scheduler.register(
            "check_auth_errors",
            Duration::from_secs(30),
            check_auth_errors_fn,
        );
        scheduler.register(
            "check_usage_limits",
            Duration::from_secs(60),
            check_usage_limits_fn,
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
            worktree_manager,
            worktree_registry,
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
        if let Some(port) = web_port {
            let web_state = std::sync::Arc::new(crate::daemon_v2::web::WebState {
                projections: self.projections.clone(),
                channels_dir: self.config.channels_dir.clone(),
                event_tx: self.event_tx.clone(),
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
                    crate::daemon::webhook_fwd::webhook_forwarder_watchdog(
                        webhook_port,
                        restart_secs,
                        webhook_shutdown_rx,
                    )
                    .await;
                });
                tracing::info!(%webhook_port, "webhook forwarder watchdog started");
            }
        }

        // Resume agents that were running before the daemon restarted.
        for cmd in std::mem::take(&mut self.pending_resumes) {
            let events = {
                let proj = self.projections.lock().await;
                executor::execute(
                    cmd,
                    &mut self.sessions,
                    &self.paths,
                    &proj,
                    &self.config.channels_dir,
                )
                .await
            };
            self.apply_events(&events).await;
        }

        loop {
            let deadline = self
                .scheduler
                .next_deadline(Instant::now())
                .unwrap_or(Duration::from_secs(30));

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

            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _addr)) => {
                            let (outcome, events, mut commands) = {
                                let proj = self.projections.lock().await;
                                handle_rpc_connection(stream, &proj, &self.config.channels_dir).await
                            };
                            self.apply_events(&events).await;
                            for cmd in &mut commands {
                                if let Command::SpawnAgent(config) = cmd {
                                    self.prepare_worktree_for_spawn(config);
                                }
                            }
                            for command in commands {
                                let cmd_events = {
                                    let proj = self.projections.lock().await;
                                    executor::execute(
                                        command,
                                        &mut self.sessions,
                                        &self.paths,
                                        &proj,
                                        &self.config.channels_dir,
                                    )
                                    .await
                                };
                                self.handle_worktree_cleanup(&cmd_events);
                                self.apply_events(&cmd_events).await;
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

                () = &mut sleep => {
                    self.run_due_decisions().await;
                }
            }
        }
    }

    /// Apply a batch of domain events to the event store and projections.
    async fn apply_events(&mut self, events: &[crate::daemon_v2::events::DomainEvent]) {
        if events.is_empty() {
            return;
        }
        let mut proj = self.projections.lock().await;
        for event in events {
            if let Err(e) = self.store.append(event) {
                tracing::error!(%e, "failed to append event");
            }
            proj.apply(event);
            // Broadcast to WebSocket clients (ignore if no receivers).
            let _ = self.event_tx.send(event.clone());
        }
    }

    /// Run all currently due decisions, execute the resulting commands, and
    /// apply the produced events to the event store and projections.
    async fn run_due_decisions(&mut self) {
        let now = Instant::now();
        let due = self.scheduler.due_decisions(now);

        for decision in due {
            let mut commands = {
                let proj = self.projections.lock().await;
                (decision.run)(&proj, &self.config.default_channel)
            };
            self.scheduler.mark_ran(decision.name, now);

            // Enrich SpawnAgent commands with worktree paths before execution.
            for cmd in &mut commands {
                if let Command::SpawnAgent(config) = cmd {
                    self.prepare_worktree_for_spawn(config);
                }
            }

            for command in commands {
                let events = {
                    let proj = self.projections.lock().await;
                    executor::execute(
                        command,
                        &mut self.sessions,
                        &self.paths,
                        &proj,
                        &self.config.channels_dir,
                    )
                    .await
                };
                // Clean up worktrees for completed tasks.
                self.handle_worktree_cleanup(&events);
                self.apply_events(&events).await;
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
}

// ── RPC connection handling ────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum RpcOutcome {
    Continue,
    Shutdown,
}

/// Read one JSON-RPC request from `stream`, dispatch it, and write the response.
/// Returns `RpcOutcome::Shutdown` if the request was a shutdown request, plus any
/// domain events produced by mutating RPC methods (e.g., `task.create`), and any
/// commands to execute (e.g., `session.fork` spawning a new agent).
async fn handle_rpc_connection(
    mut stream: UnixStream,
    proj: &Projections,
    channels_dir: &Path,
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

    // Check for the special "shutdown" method before dispatching.
    let is_shutdown = request.get("method").and_then(|m| m.as_str()) == Some("shutdown");

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

    let (response, events, commands) = rpc::dispatch_request(request, proj, channels_dir);
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
