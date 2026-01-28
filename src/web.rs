//! Web server for Svelte mobile app
//!
//! Serves static files for the Svelte frontend and provides WebSocket
//! connections for live updates (channel messages, coworker status, etc.)

use axum::{
    Router,
    extract::{
        State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, error, info, warn};

use crate::channel::Channel;
use crate::message::Message;
use crate::paths;

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

/// Shared state for WebSocket connections
pub struct WebState {
    pub config: WebConfig,
    /// Broadcast channel for real-time updates
    pub updates_tx: broadcast::Sender<WebUpdate>,
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

/// Get daemon/coworker status by calling the daemon RPC over Unix socket.
async fn api_status(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, StatusCode> {
    let repo = state.config.repo.clone();

    let status = tokio::task::spawn_blocking(move || call_daemon_status(&repo))
        .await
        .map_err(|e| {
            error!("Status task join error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(axum::Json(status))
}

/// Call the daemon's "status" RPC method over Unix socket.
///
/// Returns the daemon's status response, or a fallback JSON if the daemon is unreachable.
fn call_daemon_status(repo: &str) -> serde_json::Value {
    let socket_path = paths::daemon_socket_for_repo(repo);

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            debug!("Cannot connect to daemon socket: {}", e);
            return serde_json::json!({
                "daemon": "stopped",
                "coworkers": [],
                "tasks": []
            });
        }
    };

    // Set a timeout so we don't hang forever
    let timeout = std::time::Duration::from_secs(5);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "status",
        "id": 1
    });

    if let Err(e) = writeln!(stream, "{}", request) {
        warn!("Failed to write to daemon socket: {}", e);
        return serde_json::json!({
            "daemon": "error",
            "coworkers": [],
            "tasks": []
        });
    }

    if let Err(e) = stream.flush() {
        warn!("Failed to flush daemon socket: {}", e);
        return serde_json::json!({
            "daemon": "error",
            "coworkers": [],
            "tasks": []
        });
    }

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    if let Err(e) = reader.read_line(&mut response_line) {
        warn!("Failed to read from daemon socket: {}", e);
        return serde_json::json!({
            "daemon": "error",
            "coworkers": [],
            "tasks": []
        });
    }

    // Parse the JSON-RPC response and extract the result
    match serde_json::from_str::<serde_json::Value>(&response_line) {
        Ok(rpc_response) => {
            if let Some(result) = rpc_response.get("result") {
                // Add top-level "daemon" field for the frontend
                let mut status = result.clone();
                if let Some(obj) = status.as_object_mut() {
                    obj.entry("daemon".to_string())
                        .or_insert(serde_json::json!("running"));
                }
                status
            } else if let Some(error) = rpc_response.get("error") {
                warn!("Daemon RPC error: {:?}", error);
                serde_json::json!({
                    "daemon": "error",
                    "coworkers": [],
                    "tasks": []
                })
            } else {
                serde_json::json!({
                    "daemon": "running",
                    "coworkers": [],
                    "tasks": []
                })
            }
        }
        Err(e) => {
            warn!("Failed to parse daemon response: {}", e);
            serde_json::json!({
                "daemon": "error",
                "coworkers": [],
                "tasks": []
            })
        }
    }
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
            // Post message to channel as "mobile" user
            let channel = Channel::for_repo(&state.config.repo)
                .map_err(|e| format!("Failed to open channel: {}", e))?;

            let message = Message::text("mobile", &content);
            channel
                .send(&message)
                .map_err(|e| format!("Failed to send message: {}", e))?;

            info!("Mobile user sent: {}", content);

            // Broadcast the message to all connected clients
            let update = WebUpdate::ChannelMessage(ChannelMessageData {
                from: "mobile".to_string(),
                content,
                timestamp: message.timestamp.to_rfc3339(),
                msg_type: "text".to_string(),
            });

            let _ = state.updates_tx.send(update);
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
}
