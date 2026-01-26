//! Midtown daemon server.
//!
//! This module provides the daemon server that listens on a Unix socket and
//! handles JSON-RPC requests for workspace management operations. It also
//! runs a webhook server to receive GitHub events.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::coworker::CoworkerManager;
use crate::message::{Message, MessageType};
use crate::rpc::{Request, RequestId, Response, RpcError};
use crate::webhook::{WebhookConfig, start_webhook_server};
use crate::worktree::WorktreeManager;

/// Configuration for the daemon server.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the Unix socket.
    pub socket_path: PathBuf,
    /// Working directory for spawned coworkers.
    pub workdir: PathBuf,
    /// Enable verbose logging.
    pub verbose: bool,
    /// Port for the webhook server (None to disable).
    pub webhook_port: Option<u16>,
    /// GitHub webhook secret for signature verification.
    pub webhook_secret: Option<String>,
}

/// Default port for the webhook server (obscure to avoid conflicts)
pub const DEFAULT_WEBHOOK_PORT: u16 = 47022;

impl Default for DaemonConfig {
    fn default() -> Self {
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
            });

        // Check env vars for webhook config (can override or disable with MIDTOWN_WEBHOOK_PORT=0)
        let webhook_port = std::env::var("MIDTOWN_WEBHOOK_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(|p| if p == 0 { None } else { Some(p) })
            .unwrap_or(Some(DEFAULT_WEBHOOK_PORT));
        let webhook_secret = std::env::var("MIDTOWN_WEBHOOK_SECRET").ok();

        Self {
            socket_path: state_dir.join("midtown").join("daemon.sock"),
            workdir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            verbose: false,
            webhook_port,
            webhook_secret,
        }
    }
}

/// Shared daemon state.
struct DaemonState {
    coworkers: CoworkerManager,
    channel: Channel,
    socket_path: PathBuf,
}

impl DaemonState {
    fn new(socket_path: PathBuf, workdir: PathBuf, channel: Channel) -> crate::Result<Self> {
        // Derive the tmux session name from the workdir (repo name)
        let repo_name = workdir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        let session_name = format!("midtown-{}", repo_name);

        // Create worktree manager for coworker isolation
        let worktree_manager = WorktreeManager::new(workdir).map_err(|e| crate::Error::Rpc {
            code: -32603,
            message: format!("Failed to initialize worktree manager: {}", e),
        })?;

        Ok(Self {
            coworkers: CoworkerManager::new(session_name, worktree_manager),
            channel,
            socket_path,
        })
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

    // Ensure parent directory exists
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Derive repo name from workdir
    let repo_name = config
        .workdir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    // Create channel for the repo
    let channel = Channel::for_repo(&repo_name)?;
    info!("Channel: {}", channel.base_dir().display());

    // Create daemon state (pass channel to state so RPC handlers can use it)
    let state = Arc::new(DaemonState::new(
        config.socket_path.clone(),
        config.workdir,
        channel,
    )?);

    // Remove existing socket file if present
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }

    // Bind to Unix socket
    let listener = UnixListener::bind(&config.socket_path)?;
    info!("Listening on {}", config.socket_path.display());

    // Start webhook server and gh forwarder if configured
    let mut webhook_rx = None;
    let mut gh_forward_process: Option<std::process::Child> = None;

    if let Some(port) = config.webhook_port {
        let webhook_config = WebhookConfig {
            port,
            secret: config.webhook_secret.clone(),
            repo: repo_name.clone(),
        };
        match start_webhook_server(webhook_config).await {
            Ok(rx) => {
                info!("Webhook server started on port {}", port);
                webhook_rx = Some(rx);

                // Get the GitHub repo name (owner/repo) for webhook forwarding
                let gh_repo = std::process::Command::new("gh")
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
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

                if let Some(repo) = gh_repo {
                    // Ensure gh-webhook extension is installed
                    let extension_check = std::process::Command::new("gh")
                        .args(["extension", "list"])
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).contains("webhook"))
                        .unwrap_or(false);

                    if !extension_check {
                        info!("Installing gh-webhook extension...");
                        let install_result = std::process::Command::new("gh")
                            .args(["extension", "install", "cli/gh-webhook"])
                            .status();
                        if let Err(e) = install_result {
                            warn!("Failed to install gh-webhook extension: {}", e);
                        }
                    }
                    // Start gh webhook forward to receive GitHub events
                    let url = format!("http://localhost:{}/webhook", port);
                    match std::process::Command::new("gh")
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
                    {
                        Ok(child) => {
                            info!("Started gh webhook forward for {} to {}", repo, url);
                            gh_forward_process = Some(child);
                        }
                        Err(e) => {
                            warn!("Failed to start gh webhook forward: {}", e);
                        }
                    }
                } else {
                    warn!(
                        "Could not determine GitHub repo (gh repo view failed). Webhook forwarding disabled."
                    );
                    warn!("Webhooks will still work if configured manually in GitHub settings.");
                }
            }
            Err(e) => {
                error!("Failed to start webhook server: {}", e);
            }
        }
    } else {
        debug!("Webhook server disabled (no port configured)");
    }

    // Set up shutdown signal handler
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

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

            // Forward webhook messages to channel
            Some(msg) = async {
                match webhook_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!("Received webhook message: {}", msg.content);
                if let Err(e) = state.channel.send(&msg) {
                    error!("Failed to forward webhook message to channel: {}", e);
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

    // Stop gh webhook forward if running
    if let Some(mut child) = gh_forward_process {
        info!("Stopping gh webhook forward...");
        let _ = child.kill();
        let _ = child.wait();
    }

    // Shutdown all coworkers
    info!("Shutting down coworkers...");
    if let Err(e) = state.coworkers.shutdown_all() {
        warn!("Error shutting down coworkers: {}", e);
    }

    // Clean up socket file
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }

    info!("Daemon stopped");
    Ok(())
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

        "coworker.spawn" => handle_coworker_spawn(request.id, state),

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

            match (name, message) {
                (Some(name), Some(message)) => {
                    handle_coworker_nudge(request.id, name, message, state)
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
                .unwrap_or("Lead");

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
fn handle_coworker_spawn(id: RequestId, state: &DaemonState) -> Response {
    match state.coworkers.spawn() {
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
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": cw.current_task,
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
fn handle_coworker_nudge(
    id: RequestId,
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

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
fn handle_channel_post(id: RequestId, from: &str, message: &str, state: &DaemonState) -> Response {
    // Check for /me prefix (IRC-style action)
    let (content, msg_type) = if let Some(action) = message.strip_prefix("/me ") {
        (action.to_string(), MessageType::Action)
    } else {
        (message.to_string(), MessageType::Text)
    };

    let msg = Message::new(from, content, msg_type);

    match state.channel.send(&msg) {
        Ok(()) => {
            info!("Channel post from {}: {}", from, message);
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
    // Get coworkers with their details
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": cw.current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect();

    // Get open PRs from GitHub via gh CLI
    let pull_requests = get_open_prs();

    // Get open tasks from beads system
    let tasks = get_open_tasks();

    // Get recent channel activity
    let recent_activity = get_recent_channel_activity();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "pending_tasks": tasks.len(),
            "socket_path": state.socket_path.to_string_lossy(),
            "coworkers": coworkers,
            "tasks": tasks,
            "pull_requests": pull_requests,
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

/// Get open tasks from beads system.
fn get_open_tasks() -> Vec<serde_json::Value> {
    // Use bd ready to get tasks that are ready to work on
    let output = std::process::Command::new("bd")
        .args(["ready", "--json"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(beads) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                beads
                    .into_iter()
                    .map(|bead| {
                        serde_json::json!({
                            "id": bead.get("id").and_then(|i| i.as_str()).unwrap_or(""),
                            "subject": bead.get("subject").and_then(|s| s.as_str()).unwrap_or(""),
                            "status": bead.get("status").and_then(|s| s.as_str()).unwrap_or("pending"),
                            "assignee": bead.get("owner").and_then(|o| o.as_str()),
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => {
            debug!("Failed to get tasks from bd CLI");
            Vec::new()
        }
    }
}

/// Get recent channel activity.
fn get_recent_channel_activity() -> Vec<serde_json::Value> {
    // Try to read from the default channel location
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let channel_file = std::path::PathBuf::from(&home)
        .join(".midtown")
        .join("default")
        .join("channel.jsonl");

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
