//! Web API server for Svelte mobile app
//!
//! Provides API endpoints and WebSocket connections for live updates
//! (channel messages, coworker status, etc.). Static files are served
//! only by the shared gateway on port 47022.

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::push::PushManager;
use crate::tmux;

/// Configuration for the web server
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Repository name for channel access
    pub repo: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            repo: "default".to_string(),
        }
    }
}

/// A mobile channel post to be forwarded to the daemon for processing.
pub struct MobileChannelPost {
    pub content: String,
}

/// Shared state for WebSocket connections
pub struct WebState {
    pub config: WebConfig,
    /// Broadcast channel for real-time updates
    pub updates_tx: broadcast::Sender<WebUpdate>,
    /// Coworker manager for querying live coworker state
    pub coworkers: Option<CoworkerManager>,
    /// Sender for channel posts to be processed by the daemon
    pub channel_post_tx: mpsc::Sender<MobileChannelPost>,
    /// Web Push notification manager (shared with daemon)
    pub push_manager: Option<Arc<PushManager>>,
}

/// Types of real-time updates sent to clients
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WebUpdate {
    /// New channel message
    #[serde(rename = "channel_message")]
    ChannelMessage(ChannelMessageData),
    /// Coworker status changed
    #[serde(rename = "coworker_status")]
    CoworkerStatus(CoworkerStatusData),
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelMessageData {
    pub from: String,
    pub content: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoworkerStatusData {
    pub name: String,
    pub status: String,
    pub current_task: Option<String>,
}

/// WebSocket message from client
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Send a message to the channel (to lead)
    #[serde(rename = "send_message")]
    SendMessage { content: String },
    /// Request full channel history
    #[serde(rename = "get_history")]
    GetHistory,
    /// Request coworker status
    #[serde(rename = "get_status")]
    GetStatus,
}

/// Create the web server router
///
/// This can be nested into the main webhook server.
/// Only serves API endpoints — static files are handled by the shared gateway.
pub fn create_web_router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/channel", get(api_channel_history))
        .route("/api/status", get(api_status))
        .route("/api/lead-pane", get(api_lead_pane))
        .route("/api/tmux-pane", get(api_tmux_pane))
        .route("/api/tmux-windows", get(api_tmux_windows))
        .route("/api/push/vapid-key", get(api_push_vapid_key))
        .route("/api/push/subscribe", post(api_push_subscribe))
        .route("/api/push/unsubscribe", post(api_push_unsubscribe))
        .with_state(state)
}

/// Health check endpoint
async fn api_health() -> &'static str {
    "ok"
}

/// Get channel message history
async fn api_channel_history(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let channel = Channel::for_repo(&state.config.repo).map_err(|e| {
        error!("Failed to open channel: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let messages = channel.read_all().map_err(|e| {
        error!("Failed to read channel: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let response: Vec<ChannelMessageData> = messages
        .into_iter()
        .map(|m| ChannelMessageData {
            from: m.from,
            content: m.content,
            timestamp: m.timestamp.to_rfc3339(),
            msg_type: format!("{:?}", m.message_type).to_lowercase(),
        })
        .collect();

    Ok(axum::Json(response))
}

/// CI status for the repository
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiStatus {
    Passed,
    Failed,
    Running,
    Unknown,
}

/// Repository status information (commit, CI, release)
#[derive(Debug, Clone, Serialize, Default)]
pub struct RepoStatus {
    pub commit_hash: String,
    pub commit_time: Option<String>,
    pub ci_status: Option<CiStatus>,
    pub release_tag: Option<String>,
    pub release_time: Option<String>,
}

/// Fetch repository status via gh CLI
fn fetch_repo_status() -> RepoStatus {
    let mut status = RepoStatus::default();

    // Fetch latest commit on default branch
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/commits/{branch}",
            "--jq",
            r#"{sha: .sha[0:7], date: .commit.author.date}"#,
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(sha) = data.get("sha").and_then(|v| v.as_str()) {
                status.commit_hash = sha.to_string();
            }
            if let Some(date_str) = data.get("date").and_then(|v| v.as_str()) {
                status.commit_time = Some(date_str.to_string());
            }
        }
    }

    // Fetch CI status from latest workflow run on main branch
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/actions/runs?branch=main&per_page=1",
            "--jq",
            ".workflow_runs[0] | {status: .status, conclusion: .conclusion}",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            let run_status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let conclusion = data.get("conclusion").and_then(|v| v.as_str());

            status.ci_status = Some(match (run_status, conclusion) {
                ("completed", Some("success")) => CiStatus::Passed,
                ("completed", Some("failure")) => CiStatus::Failed,
                ("completed", Some("cancelled")) => CiStatus::Failed,
                ("in_progress", _) | ("queued", _) | ("waiting", _) => CiStatus::Running,
                _ => CiStatus::Unknown,
            });
        }
    }

    // Fetch latest release
    if let Ok(output) = std::process::Command::new("gh")
        .args([
            "api",
            "repos/{owner}/{repo}/releases/latest",
            "--jq",
            "{tag: .tag_name, published_at: .published_at}",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(tag) = data.get("tag").and_then(|v| v.as_str()) {
                status.release_tag = Some(tag.to_string());
            }
            if let Some(date_str) = data.get("published_at").and_then(|v| v.as_str()) {
                status.release_time = Some(date_str.to_string());
            }
        }
    }

    status
}

/// Get daemon/coworker status including tasks and PRs for kanban board
async fn api_status(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, StatusCode> {
    // Read tasks directly from Claude Code task storage
    let tasks: Vec<serde_json::Value> = crate::tasks::read_tasks()
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
        .collect();

    // Load GitHub state to get reviewer assignments
    let github_state =
        crate::github_state::load_state_for_repo(&state.config.repo).unwrap_or_default();

    // Get open PRs via gh CLI (spawn blocking to avoid blocking async runtime)
    let raw_prs = tokio::task::spawn_blocking(|| {
        let output = std::process::Command::new("gh")
            .args([
                "pr",
                "list",
                "--json",
                "number,title,author,state,isDraft,reviewDecision,createdAt",
            ])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                serde_json::from_str::<Vec<serde_json::Value>>(&stdout).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    })
    .await
    .unwrap_or_default();

    // Transform PRs and add reviewer info
    let pull_requests: Vec<serde_json::Value> = raw_prs
        .into_iter()
        .map(|pr| {
            let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
            let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
            let status = if is_draft {
                "draft"
            } else {
                match pr
                    .get("reviewDecision")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                {
                    "APPROVED" => "approved",
                    "CHANGES_REQUESTED" => "changes requested",
                    "REVIEW_REQUIRED" => "awaiting review",
                    _ => "open",
                }
            };
            // Look up reviewer assignment from persistent state
            let assignment = github_state.pr_reviewers.get(&pr_number);
            let reviewer = assignment.map(|a| a.reviewer.as_str());
            let reviewer_assigned_at = assignment.map(|a| a.assigned_at.to_rfc3339());
            serde_json::json!({
                "number": pr_number,
                "title": pr.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                "author": pr.get("author").and_then(|a| a.get("login")).and_then(|l| l.as_str()).unwrap_or("unknown"),
                "status": status,
                "reviewer": reviewer,
                "reviewer_assigned_at": reviewer_assigned_at,
                "created_at": pr.get("createdAt").and_then(|c| c.as_str()),
            })
        })
        .collect();

    // Get merged PRs via gh CLI
    let merged_prs = tokio::task::spawn_blocking(|| {
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
            _ => Vec::new(),
        }
    })
    .await
    .unwrap_or_default();

    // Build a map of coworker name -> current task subject from in_progress tasks
    let coworker_tasks: std::collections::HashMap<String, String> = tasks
        .iter()
        .filter_map(|t| {
            let status = t.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let owner = t.get("owner").and_then(|o| o.as_str()).unwrap_or("");
            let subject = t.get("subject").and_then(|s| s.as_str()).unwrap_or("");
            if status == "in_progress" && !owner.is_empty() {
                Some((owner.to_lowercase(), subject.to_string()))
            } else {
                None
            }
        })
        .collect();

    let coworkers_data: Vec<serde_json::Value> = state
        .coworkers
        .as_ref()
        .map(|mgr| {
            mgr.list()
                .into_iter()
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
                .collect()
        })
        .unwrap_or_default();

    // Fetch repo status (blocking I/O)
    let repo_status = tokio::task::spawn_blocking(fetch_repo_status)
        .await
        .unwrap_or_default();

    let status = serde_json::json!({
        "daemon": "running",
        "coworkers": coworkers_data,
        "tasks": tasks,
        "pull_requests": pull_requests,
        "merged_prs": merged_prs,
        "repo_name": state.config.repo,
        "repo_status": repo_status,
    });

    Ok(axum::Json(status))
}
/// Get the lead's tmux pane content
///
/// Captures the current content of the lead window's pane via tmux.
/// The frontend polls this endpoint to stream the lead's terminal output.
async fn api_lead_pane(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);
    let target = format!("{}:lead", session);

    // capture_pane is blocking (spawns a process), so run on blocking thread pool
    let content = tokio::task::spawn_blocking(move || tmux::capture_pane(&target))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match content {
        Some(text) => Ok(axum::Json(serde_json::json!({ "content": text }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Query parameters for tmux pane capture
#[derive(Debug, Deserialize)]
struct TmuxPaneQuery {
    /// Which tmux window to capture (e.g., "lead", "riverside")
    window: String,
}

/// Get the content of any tmux window's pane
///
/// Accepts a `?window=name` query parameter to select which window to capture.
/// Used by the "Tmux" tab in the web UI.
async fn api_tmux_pane(
    State(state): State<Arc<WebState>>,
    Query(params): Query<TmuxPaneQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);
    let window = params.window;

    // Validate window name: only allow non-empty alphanumeric, hyphens, and underscores
    if window.is_empty()
        || !window
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let target = format!("{}:{}", session, window);

    let content = tokio::task::spawn_blocking(move || tmux::capture_pane(&target))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match content {
        Some(text) => Ok(axum::Json(serde_json::json!({ "content": text }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// List all tmux windows in the session
///
/// Returns a JSON array of window names that can be passed to `/api/tmux-pane?window=`.
async fn api_tmux_windows(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let session = format!("{}{}", tmux::SESSION_PREFIX, state.config.repo);

    let windows = tokio::task::spawn_blocking(move || tmux::list_all_windows(&session))
        .await
        .map_err(|e| {
            error!("Failed to spawn blocking task: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map_err(|e| {
            error!("Failed to list tmux windows: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::Json(serde_json::json!({ "windows": windows })))
}

/// Get the VAPID public key for push subscription.
async fn api_push_vapid_key(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let key = push.vapid_public_key_base64().map_err(|e| {
        error!("Failed to get VAPID public key: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(axum::Json(serde_json::json!({ "publicKey": key })))
}

/// Subscribe request body from the browser.
#[derive(Debug, Deserialize)]
struct PushSubscribeRequest {
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// Subscribe a client for push notifications.
async fn api_push_subscribe(
    State(state): State<Arc<WebState>>,
    Json(body): Json<PushSubscribeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let sub = crate::push::PushSubscription {
        endpoint: body.endpoint,
        p256dh: body.p256dh,
        auth: body.auth,
    };

    push.add_subscription(sub).map_err(|e| {
        error!("Failed to store push subscription: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("New push subscription registered");
    Ok(StatusCode::CREATED)
}

/// Unsubscribe request body.
#[derive(Debug, Deserialize)]
struct PushUnsubscribeRequest {
    endpoint: String,
}

/// Unsubscribe a client from push notifications.
async fn api_push_unsubscribe(
    State(state): State<Arc<WebState>>,
    Json(body): Json<PushUnsubscribeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let push = state
        .push_manager
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    push.remove_subscription(&body.endpoint).map_err(|e| {
        error!("Failed to remove push subscription: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!("Push subscription removed");
    Ok(StatusCode::OK)
}

/// WebSocket upgrade handler
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WebState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_websocket(socket, state))
}

/// Handle an individual WebSocket connection
async fn handle_websocket(socket: WebSocket, state: Arc<WebState>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to broadcast updates
    let mut updates_rx = state.updates_tx.subscribe();

    // Spawn task to forward broadcast updates to this client
    let send_task = tokio::spawn(async move {
        while let Ok(update) = updates_rx.recv().await {
            let json = match serde_json::to_string(&update) {
                Ok(j) => j,
                Err(e) => {
                    error!("Failed to serialize update: {}", e);
                    continue;
                }
            };

            if sender.send(WsMessage::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages from client
    let state_clone = state.clone();
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                if let Err(e) = handle_client_message(&text, &state_clone).await {
                    warn!("Error handling client message: {}", e);
                }
            }
            Ok(WsMessage::Close(_)) => break,
            Err(e) => {
                debug!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
    debug!("WebSocket connection closed");
}

/// Handle a message from a WebSocket client
async fn handle_client_message(text: &str, state: &Arc<WebState>) -> Result<(), String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("Invalid message format: {}", e))?;

    match msg {
        ClientMessage::SendMessage { content } => {
            // Forward to the daemon for processing (handles channel write,
            // WebSocket broadcast, and side-effects like nudging the Lead)
            state
                .channel_post_tx
                .send(MobileChannelPost {
                    content: content.clone(),
                })
                .await
                .map_err(|e| format!("Failed to forward message to daemon: {}", e))?;

            info!("User sent: {}", content);
        }
        ClientMessage::GetHistory => {
            // Client should use the REST endpoint for history
            debug!("Client requested history via WebSocket");
        }
        ClientMessage::GetStatus => {
            // Client should use the REST endpoint for status
            debug!("Client requested status via WebSocket");
        }
    }

    Ok(())
}

/// Create a new broadcast channel for updates
pub fn create_updates_channel() -> (broadcast::Sender<WebUpdate>, broadcast::Receiver<WebUpdate>) {
    broadcast::channel(100)
}

/// Broadcast a coworker status change to all WebSocket clients
pub fn broadcast_coworker_status(
    tx: &broadcast::Sender<WebUpdate>,
    name: &str,
    status: &str,
    current_task: Option<&str>,
) {
    let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
        name: name.to_string(),
        status: status.to_string(),
        current_task: current_task.map(|s| s.to_string()),
    });

    let _ = tx.send(update);
}

/// Broadcast a new channel message to all WebSocket clients
pub fn broadcast_channel_message(tx: &broadcast::Sender<WebUpdate>, message: &Message) {
    let update = WebUpdate::ChannelMessage(ChannelMessageData {
        from: message.from.clone(),
        content: message.content.clone(),
        timestamp: message.timestamp.to_rfc3339(),
        msg_type: format!("{:?}", message.message_type).to_lowercase(),
    });

    let _ = tx.send(update);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_parsing() {
        let json = r#"{"type": "send_message", "content": "Hello world"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SendMessage { content } => {
                assert_eq!(content, "Hello world");
            }
            _ => panic!("Expected SendMessage"),
        }
    }

    #[tokio::test]
    async fn test_mobile_send_message_forwards_to_daemon() {
        // Verify that handle_client_message forwards mobile messages through
        // channel_post_tx instead of writing directly to the channel file.
        let (updates_tx, _) = broadcast::channel(10);
        let (channel_post_tx, mut channel_post_rx) = mpsc::channel(10);

        let state = Arc::new(WebState {
            config: WebConfig::default(),
            updates_tx,
            coworkers: None,
            channel_post_tx,
            push_manager: None,
        });

        let json = r#"{"type": "send_message", "content": "hello from mobile"}"#;
        handle_client_message(json, &state).await.unwrap();

        // The message should be forwarded to the daemon via channel_post_tx
        let post = channel_post_rx
            .try_recv()
            .expect("expected a mobile channel post");
        assert_eq!(post.content, "hello from mobile");
    }

    #[test]
    fn test_web_update_serialization() {
        let update = WebUpdate::ChannelMessage(ChannelMessageData {
            from: "test".to_string(),
            content: "Hello".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            msg_type: "text".to_string(),
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("channel_message"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_coworker_status_update_serialization() {
        let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
            name: "lexington".to_string(),
            status: "running".to_string(),
            current_task: Some("Fix auth bug".to_string()),
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("coworker_status"));
        assert!(json.contains("lexington"));
        assert!(json.contains("running"));
        assert!(json.contains("Fix auth bug"));
    }

    #[test]
    fn test_tmux_pane_query_parsing() {
        // Valid window names
        let json = r#"{"window": "lead"}"#;
        let query: TmuxPaneQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.window, "lead");

        let json = r#"{"window": "riverside"}"#;
        let query: TmuxPaneQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.window, "riverside");
    }

    #[test]
    fn test_tmux_window_name_validation() {
        // Valid names: non-empty, alphanumeric, hyphens, underscores
        let valid = |name: &str| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        };

        assert!(valid("lead"));
        assert!(valid("riverside"));
        assert!(valid("my-window"));
        assert!(valid("window_1"));
        assert!(!valid("foo:bar"));
        assert!(!valid("foo;bar"));
        assert!(!valid("foo bar"));
        assert!(!valid(""));
    }

    #[test]
    fn test_coworker_status_update_without_task() {
        let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
            name: "park".to_string(),
            status: "stopped".to_string(),
            current_task: None,
        });

        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("coworker_status"));
        assert!(json.contains("park"));
        assert!(json.contains("stopped"));
    }
}
