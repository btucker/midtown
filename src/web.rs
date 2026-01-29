//! Web server for Svelte mobile app
//!
//! Serves static files for the Svelte frontend and provides WebSocket
//! connections for live updates (channel messages, coworker status, etc.)

use axum::{
    Router,
    extract::{
        Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::coworker::CoworkerManager;
use crate::message::Message;
use crate::tmux;

/// Configuration for the web server
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Path to static files directory (built Svelte app)
    pub static_dir: PathBuf,
    /// Repository name for channel access
    pub repo: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        // Default to looking for web app in executable's directory
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            static_dir: exe_dir.join("web"),
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
pub fn create_web_router(state: Arc<WebState>) -> Router {
    let static_dir = state.config.static_dir.clone();
    let index_path = static_dir.join("index.html");

    // Check if static files exist
    let has_static = static_dir.exists() && index_path.exists();

    if has_static {
        info!("Serving static files from {:?}", static_dir);
        // Serve static files with SPA fallback
        let serve_dir = ServeDir::new(&static_dir).fallback(ServeFile::new(&index_path));

        Router::new()
            .route("/api/ws", get(ws_handler))
            .route("/api/health", get(api_health))
            .route("/api/channel", get(api_channel_history))
            .route("/api/status", get(api_status))
            .route("/api/lead-pane", get(api_lead_pane))
            .route("/api/tmux-pane", get(api_tmux_pane))
            .route("/api/tmux-windows", get(api_tmux_windows))
            .fallback_service(serve_dir)
            .with_state(state)
    } else {
        warn!(
            "Static directory not found at {:?}, serving API only",
            static_dir
        );
        // API-only mode for development
        Router::new()
            .route("/api/ws", get(ws_handler))
            .route("/api/health", get(api_health))
            .route("/api/channel", get(api_channel_history))
            .route("/api/status", get(api_status))
            .route("/api/lead-pane", get(api_lead_pane))
            .route("/api/tmux-pane", get(api_tmux_pane))
            .route("/api/tmux-windows", get(api_tmux_windows))
            .route("/", get(dev_placeholder))
            .with_state(state)
    }
}

/// Placeholder page for development
async fn dev_placeholder() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Midtown Mobile</title>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
        body {
            font-family: system-ui, sans-serif;
            background: #1a1a2e;
            color: #eee;
            padding: 20px;
            margin: 0;
        }
        h1 { color: #00d9ff; }
        .status { color: #4ade80; }
        code {
            background: #16213e;
            padding: 2px 6px;
            border-radius: 4px;
        }
    </style>
</head>
<body>
    <h1>Midtown Mobile API</h1>
    <p class="status">Server is running</p>
    <p>Build the Svelte app and place in the <code>web/</code> directory to enable the UI.</p>
    <h2>API Endpoints</h2>
    <ul>
        <li><code>GET /api/health</code> - Health check</li>
        <li><code>GET /api/channel</code> - Get channel history</li>
        <li><code>GET /api/status</code> - Get daemon status</li>
        <li><code>GET /api/ws</code> - WebSocket for live updates</li>
    </ul>
</body>
</html>"#,
    )
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

    let coworkers_data: Vec<serde_json::Value> = state
        .coworkers
        .as_ref()
        .map(|mgr| {
            mgr.list()
                .into_iter()
                .map(|cw| {
                    serde_json::json!({
                        "name": cw.name,
                        "status": cw.status.to_string(),
                        "current_task": cw.current_task,
                        "started_at": cw.started_at.to_rfc3339(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let status = serde_json::json!({
        "daemon": "running",
        "coworkers": coworkers_data,
        "tasks": tasks,
        "pull_requests": pull_requests,
        "merged_prs": merged_prs,
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
