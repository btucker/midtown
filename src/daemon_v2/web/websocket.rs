use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde_json::json;
use std::sync::Arc;

use super::WebState;
use crate::daemon_v2::executor::channel_io;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<WebState>) {
    let mut rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(domain_event) => {
                        let json = serde_json::to_string(&domain_event).unwrap_or_default();
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        handle_client_message(&text, &state, &mut socket).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn handle_client_message(text: &str, state: &WebState, socket: &mut WebSocket) {
    let msg: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "send_message" => {
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() {
                return;
            }

            // Use channel from message, or default
            let channel = msg
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("midtown");
            let thread_parent_id = msg
                .get("thread_parent_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Post the message to the channel JSONL
            if let Err(e) = channel_io::post_message(
                &state.channels_dir,
                channel,
                "user",
                content,
                thread_parent_id.as_deref(),
            ) {
                tracing::error!(%e, "failed to post message via WS");
                return;
            }

            // Send confirmation back to the client
            let msg_id = uuid::Uuid::new_v4().to_string();
            let confirmation = json!({
                "type": "channel_message",
                "data": {
                    "id": msg_id,
                    "from": "user",
                    "content": content,
                    "channel": channel,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "msg_type": "text",
                    "thread_parent_id": thread_parent_id,
                }
            });
            let _ = socket
                .send(Message::Text(confirmation.to_string().into()))
                .await;
        }
        "get_history" | "get_status" => {
            // These are handled via HTTP polling, ignore on WS
        }
        _ => {
            tracing::debug!(msg_type, "unhandled WS message type");
        }
    }
}
