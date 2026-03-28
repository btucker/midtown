use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use std::sync::Arc;

use super::WebState;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<WebState>) {
    let mut rx = state.event_tx.subscribe();

    // Send events to the client as they arrive
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(domain_event) => {
                        let json = serde_json::to_string(&domain_event).unwrap_or_default();
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(_) => break, // Channel closed
                }
            }
            // Handle incoming messages from client (ping/pong, close)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    _ => {} // Ignore other messages for now
                }
            }
        }
    }
}
