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

            let channel = msg
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("midtown");
            let thread_parent_id = msg.get("thread_parent_id").and_then(|v| v.as_str());

            // Go through the same channel.post RPC dispatch as the CLI.
            // This ensures @mention routing, thread fork routing, and lead nudging
            // all happen in one place — no duplicate logic.
            let rpc_request = json!({
                "jsonrpc": "2.0",
                "method": "channel.post",
                "params": {
                    "channel": channel,
                    "sender": "user",
                    "content": content,
                    "thread_id": thread_parent_id,
                },
                "id": 1,
            });

            let (response, events, commands) = {
                let proj = state.projections.lock().await;
                crate::daemon_v2::rpc::dispatch_request(rpc_request, &proj, &state.channels_dir)
            };

            // Check for RPC error (e.g., channel is archived)
            if response.get("error").is_some() {
                let error_msg = response
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error");
                let _ = socket
                    .send(Message::Text(
                        json!({"type": "error", "data": {"message": error_msg}})
                            .to_string()
                            .into(),
                    ))
                    .await;
                return;
            }

            // Apply events to projections + broadcast + persist via daemon
            if !events.is_empty() {
                {
                    let mut proj = state.projections.lock().await;
                    for event in &events {
                        proj.apply(event);
                        let _ = state.event_tx.send(event.clone());
                    }
                }
                let persist = crate::daemon_v2::decisions::Command::PersistEvents(events);
                let _ = state.command_tx.send(persist).await;
            }

            // Send commands (mention/thread nudges) to the daemon for execution
            for cmd in commands {
                if let Err(e) = state.command_tx.send(cmd).await {
                    tracing::warn!(%e, "failed to send command to daemon");
                }
            }

            // No explicit confirmation needed — the MessagePosted event broadcast
            // (via event_tx above) already delivers the message to all WS clients,
            // including the sender. Sending an extra confirmation would double-deliver.
        }
        "get_history" | "get_status" => {
            // These are handled via HTTP polling, ignore on WS
        }
        "answer_question" => {
            let coworker = msg
                .get("coworker_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let answer = msg.get("answer").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!(%coworker, "answering question via WS");
            // Post the answer to the coworker's DM channel
            if !coworker.is_empty() && !answer.is_empty() {
                let dm = format!("dm-{coworker}");
                let _ = channel_io::post_message(&state.channels_dir, &dm, "user", answer, None);
            }
        }
        "fork_thread" => {
            let thread_parent_id = msg
                .get("thread_parent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let channel = msg
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("midtown");
            let fork_message = msg.get("message").and_then(|v| v.as_str());
            tracing::info!(%thread_parent_id, %channel, "fork_thread via WS");

            // Dispatch through session.fork RPC to spawn or find existing fork
            let rpc_request = json!({
                "jsonrpc": "2.0",
                "method": "session.fork",
                "params": {
                    "thread_parent_id": thread_parent_id,
                    "channel": channel,
                    "message": fork_message,
                },
                "id": 1,
            });

            let (response, _events, commands) = {
                let proj = state.projections.lock().await;
                crate::daemon_v2::rpc::dispatch_request(rpc_request, &proj, &state.channels_dir)
            };

            // Send spawn commands to daemon
            for cmd in commands {
                if let Err(e) = state.command_tx.send(cmd).await {
                    tracing::warn!(%e, "failed to send fork command");
                }
            }

            let has_fork = response
                .get("result")
                .and_then(|r| r.get("existing"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || response
                    .get("result")
                    .and_then(|r| r.get("forking"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            let ownership = json!({
                "type": "thread_ownership",
                "data": {
                    "thread_parent_id": thread_parent_id,
                    "channel": channel,
                    "has_dedicated_session": has_fork,
                }
            });
            let _ = socket
                .send(Message::Text(ownership.to_string().into()))
                .await;
        }
        "unfork_thread" => {
            let thread_parent_id = msg
                .get("thread_parent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            tracing::info!(%thread_parent_id, "unfork_thread via WS");
        }
        "get_thread_ownership" => {
            let thread_parent_id = msg
                .get("thread_parent_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let channel = msg
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("midtown");
            // Check if a fork session exists for this thread
            let proj = state.projections.lock().await;
            let has_fork = proj
                .agents
                .fork_for_thread(thread_parent_id)
                .map(|a| proj.agents.running.contains(&a.id))
                .unwrap_or(false);
            drop(proj);
            let ownership = json!({
                "type": "thread_ownership",
                "data": {
                    "thread_parent_id": thread_parent_id,
                    "channel": channel,
                    "has_dedicated_session": has_fork,
                }
            });
            let _ = socket
                .send(Message::Text(ownership.to_string().into()))
                .await;
        }
        "cancel_lead" => {
            let channel = msg
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("midtown");
            tracing::info!(%channel, "cancel_lead via WS");
            // Find and stop the lead for this channel
            let proj = state.projections.lock().await;
            if let Some(lead) = proj.agents.channel_lead(channel) {
                let stop_cmd = crate::daemon_v2::decisions::Command::StopAgent {
                    id: lead.id.clone(),
                    reason: "user cancelled via web UI".into(),
                };
                drop(proj);
                if let Err(e) = state.command_tx.send(stop_cmd).await {
                    tracing::warn!(%e, %channel, "failed to send cancel_lead command");
                }
            }
        }
        "nudge" => {
            let target = msg.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!(%target, "nudge via WS");
            if !target.is_empty() && !message.is_empty() {
                // Post to DM channel
                let dm = format!("dm-{target}");
                let _ = channel_io::post_message(&state.channels_dir, &dm, "user", message, None);
                // Also send NudgeAgent command to actually deliver the message
                let proj = state.projections.lock().await;
                if let Some(agent_id) = proj.agents.by_name.get(target) {
                    let cmd = crate::daemon_v2::decisions::Command::NudgeAgent {
                        id: agent_id.clone(),
                        message: message.to_string(),
                    };
                    drop(proj);
                    if let Err(e) = state.command_tx.send(cmd).await {
                        tracing::warn!(%e, %target, "failed to send nudge command");
                    }
                }
            }
        }
        _ => {
            tracing::debug!(msg_type, "unhandled WS message type");
        }
    }
}
