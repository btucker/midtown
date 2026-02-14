//! Plugin RPC handlers for the Zellij dashboard plugin.
//!
//! These endpoints provide the data contract between the daemon and the
//! Zellij WASM plugin. The plugin polls `plugin.dashboard` once per second
//! to get a complete UI state snapshot.
//!
//! Endpoints:
//! - `plugin.dashboard` — complete dashboard state (tasks, coworkers, channel, nudges)
//! - `plugin.attach` — pause headless coworker and return session ID for interactive use
//! - `plugin.detach` — resume headless session after interactive detach
//! - `plugin.coworker-stream` — recent streaming events from a headless coworker

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};
use midtown_types::{
    AttachResponse, ChannelMessage, CoworkerStreamOutput, CoworkerSummary, DashboardState,
    StreamEvent, TaskSummary,
};

use super::DaemonState;
use super::snapshot::ProcessHealth;

// ============================================================================
// Handler: plugin.dashboard
// ============================================================================

/// Handle `plugin.dashboard` RPC method.
///
/// Returns a complete `DashboardState` snapshot for the Zellij plugin to render.
/// This is the primary polling endpoint — the plugin calls it ~1/s.
///
/// Collects data from multiple daemon subsystems:
/// - Tasks from Claude Code task storage
/// - Coworker records + health from daemon state
/// - Recent channel messages from the channel log
/// - Queued lead nudges (drained on read)
pub(super) async fn handle_dashboard(id: RequestId, state: &DaemonState) -> Response {
    // Collect tasks (blocking file I/O)
    let tasks = match tokio::task::spawn_blocking(crate::tasks::read_tasks).await {
        Ok(tasks) => tasks,
        Err(e) => {
            error!("spawn_blocking panic in plugin.dashboard: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };
    let task_summaries = build_task_summaries(&tasks);

    // Collect coworker data
    let coworker_summaries = {
        let active_coworkers = state.coworkers.list();
        let coworker_records = state.coworker_records.read().await;
        let health_snapshot: std::collections::HashMap<String, ProcessHealth> = {
            let guard = state.headless_health.read().unwrap();
            guard.clone()
        };

        // Build coworker task map for current_task lookup
        let coworker_tasks: std::collections::HashMap<String, String> =
            crate::tasks::get_in_progress_tasks_with_subjects()
                .into_iter()
                .filter_map(|(_task_id, subject, owner)| {
                    if owner.is_empty() {
                        None
                    } else {
                        Some((owner.to_lowercase(), subject))
                    }
                })
                .collect();

        let inputs: Vec<CoworkerBuildInput> = active_coworkers
            .iter()
            .map(|cw| {
                let record = coworker_records.get(&cw.name);
                let health = health_snapshot.get(&cw.name);
                let phase = record
                    .and_then(|r| r.workflow_phase)
                    .map(|p| format!("{:?}", p).to_lowercase());
                let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
                let session_id = {
                    // Best-effort: try persistent state without blocking
                    state.persistent_state.try_lock().ok().and_then(|ps| {
                        ps.headless_sessions
                            .get(&cw.name)
                            .map(|info| info.session_id.clone())
                    })
                };

                CoworkerBuildInput {
                    name: cw.name.clone(),
                    phase,
                    current_task,
                    session_id,
                    model: format!("{}/{}", cw.provider.as_str(), "sonnet"),
                    is_alive: health.is_none_or(|h| h.is_alive),
                    has_usage_limit: health.is_some_and(|h| h.has_usage_limit),
                    has_api_error: health.is_some_and(|h| h.has_api_error),
                    last_event_at: health.and_then(|h| h.last_event_at),
                }
            })
            .collect();

        build_coworker_summaries(&inputs)
    };

    // Collect recent channel messages (blocking file I/O)
    let channel_messages =
        match tokio::task::spawn_blocking(|| read_recent_channel_messages(20)).await {
            Ok(msgs) => build_channel_messages(&msgs),
            Err(e) => {
                warn!("Failed to read channel messages: {}", e);
                vec![]
            }
        };

    // Drain lead nudge queue
    let lead_nudge_queue = {
        let mut queue = state.lead_nudge_queue.lock().await;
        std::mem::take(&mut *queue)
    };

    let dashboard = DashboardState {
        tasks: task_summaries,
        coworkers: coworker_summaries,
        channel_messages,
        lead_nudge_queue,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    Response::success(id, serde_json::to_value(&dashboard).unwrap_or_default())
}

// ============================================================================
// Handler: plugin.attach
// ============================================================================

/// Handle `plugin.attach` RPC method.
///
/// Pauses the headless coworker and returns the session ID so the plugin
/// can open a terminal pane with `claude --resume <session-id>`.
pub(super) async fn handle_attach(
    id: RequestId,
    name: &str,
    force: bool,
    state: &DaemonState,
) -> Response {
    let name = name.to_lowercase();

    // Verify the coworker exists
    if state.coworkers.get(&name).is_none() {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Coworker '{}' is not running", name)),
        );
    }

    // Guard against double-attach
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains(&name) {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Coworker '{}' is already attached", name)),
            );
        }
    }

    // Get the session ID
    let session_id = {
        let persistent = state.persistent_state.lock().await;
        persistent
            .headless_sessions
            .get(&name)
            .map(|info| info.session_id.clone())
    };

    let session_id = match session_id {
        Some(sid) => sid,
        None => {
            return Response::success(
                id,
                serde_json::to_value(&AttachResponse {
                    success: false,
                    session_id: None,
                    error: Some(format!("No session ID found for coworker '{}'", name)),
                })
                .unwrap(),
            );
        }
    };

    // Shut down the headless coworker
    if force {
        // Force mode: kill immediately
        if let Err(e) = state.coworkers.shutdown(&name) {
            return Response::success(
                id,
                serde_json::to_value(&AttachResponse {
                    success: false,
                    session_id: None,
                    error: Some(format!("Failed to shut down coworker '{}': {}", name, e)),
                })
                .unwrap(),
            );
        }
    } else {
        // Graceful mode: same as force for now (headless sessions don't have
        // a "wait for turn completion" mode yet)
        if let Err(e) = state.coworkers.shutdown(&name) {
            return Response::success(
                id,
                serde_json::to_value(&AttachResponse {
                    success: false,
                    session_id: None,
                    error: Some(format!("Failed to shut down coworker '{}': {}", name, e)),
                })
                .unwrap(),
            );
        }
    }

    // Record stop time and mark as attached
    state.record_coworker_stop_time(&name);
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.insert(name.clone());
    }

    info!(
        "Plugin attached to coworker '{}' (session={})",
        name, session_id
    );

    let _ = state
        .send_and_broadcast_async(&Message::system(format!(
            "Plugin attached to {} — headless paused",
            name
        )))
        .await;

    Response::success(
        id,
        serde_json::to_value(&AttachResponse {
            success: true,
            session_id: Some(session_id),
            error: None,
        })
        .unwrap(),
    )
}

// ============================================================================
// Handler: plugin.detach
// ============================================================================

/// Handle `plugin.detach` RPC method.
///
/// Resumes headless execution for a coworker that was attached via the plugin.
pub(super) async fn handle_detach(id: RequestId, name: &str, state: &DaemonState) -> Response {
    let name = name.to_lowercase();

    // Clear attached state
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.remove(&name);
    }

    // Idempotency: if already running, skip re-spawn
    if state.coworkers.get(&name).is_some() {
        info!("Coworker '{}' already running — detach is a no-op", name);
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("Coworker {} is already running", name),
            }),
        );
    }

    // Get session ID
    let session_id = {
        let persistent = state.persistent_state.lock().await;
        persistent
            .headless_sessions
            .get(&name)
            .map(|info| info.session_id.clone())
    };

    let session_id = match session_id {
        Some(sid) => sid,
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("No session ID found for coworker '{}' to resume", name),
                ),
            );
        }
    };

    // Re-spawn with resumed session
    let config = crate::launch::LaunchConfig::coworker(
        &name,
        &state.repo_name,
        crate::launch::SessionMode::ResumeSession(session_id.clone()),
        Some(
            "You were running headless. The user attached to your session \
             via the Zellij plugin and has now detached. Continue where you \
             left off — read the channel for any updates."
                .to_string(),
        ),
    );

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!(
                "Resumed headless coworker '{}' after plugin detach (session={})",
                name, session_id
            );

            let _ = state
                .send_and_broadcast_async(&Message::system(format!(
                    "Plugin detached from {} — headless session resumed",
                    name
                )))
                .await;

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Resumed headless session for {}", name),
                }),
            )
        }
        Err(e) => {
            warn!(
                "Failed to resume coworker '{}' after plugin detach: {}",
                name, e
            );
            Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to resume headless session for '{}': {}", name, e),
                ),
            )
        }
    }
}

// ============================================================================
// Handler: plugin.coworker-stream
// ============================================================================

/// Handle `plugin.coworker-stream` RPC method.
///
/// Returns recent streaming events from a headless coworker's JSON stream.
/// Used for the read-only coworker activity view in the plugin.
pub(super) async fn handle_coworker_stream(
    id: RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    let name = name.to_lowercase();

    // Read from the stream event buffer
    let events = {
        let buffer = state.stream_event_buffer.read().unwrap();
        buffer.get(&name).cloned().unwrap_or_default()
    };

    let stream_events: Vec<StreamEvent> = events
        .into_iter()
        .map(|evt| StreamEvent {
            timestamp: evt.timestamp,
            event_type: evt.event_type.clone(),
            content: evt.content.clone(),
        })
        .collect();

    let output = CoworkerStreamOutput {
        coworker_name: name,
        events: stream_events,
    };

    Response::success(id, serde_json::to_value(&output).unwrap_or_default())
}

// ============================================================================
// Pure builder functions (tested in rpc_plugin_tests.rs)
// ============================================================================

/// Input data for building a `CoworkerSummary`.
///
/// Extracted from `DaemonState` by the handler, passed to the pure builder.
#[derive(Debug)]
pub(super) struct CoworkerBuildInput {
    pub name: String,
    pub phase: Option<String>,
    pub current_task: Option<String>,
    pub session_id: Option<String>,
    pub model: String,
    pub is_alive: bool,
    pub has_usage_limit: bool,
    pub has_api_error: bool,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// Build task summaries from the canonical `Task` structs.
pub(super) fn build_task_summaries(tasks: &[crate::tasks::Task]) -> Vec<TaskSummary> {
    tasks
        .iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            TaskSummary {
                id: task.id.clone(),
                subject: task.subject.clone(),
                status: status.to_string(),
                owner: task.owner.as_ref().filter(|o| !o.is_empty()).cloned(),
                pr_number: task.pr,
                pr_status: None, // PR status requires GitHub API; omit for now
            }
        })
        .collect()
}

/// Build coworker summaries from pre-collected input data.
pub(super) fn build_coworker_summaries(inputs: &[CoworkerBuildInput]) -> Vec<CoworkerSummary> {
    inputs
        .iter()
        .map(|input| CoworkerSummary {
            name: input.name.clone(),
            status: input.phase.clone().unwrap_or_else(|| "unknown".to_string()),
            current_task: input.current_task.clone(),
            session_id: input.session_id.clone(),
            model: input.model.clone(),
            is_alive: input.is_alive,
            has_usage_limit: input.has_usage_limit,
            has_api_error: input.has_api_error,
            last_event_at: input.last_event_at,
        })
        .collect()
}

/// Build channel messages from the canonical `Message` structs.
pub(super) fn build_channel_messages(messages: &[Message]) -> Vec<ChannelMessage> {
    messages
        .iter()
        .map(|msg| {
            let message_type = format!("{:?}", msg.message_type).to_lowercase();
            ChannelMessage {
                from: msg.from.clone(),
                content: msg.content.clone(),
                timestamp: msg.timestamp,
                message_type,
            }
        })
        .collect()
}

/// Read recent channel messages from the channel log file.
///
/// Returns the last `count` messages from the default channel.
fn read_recent_channel_messages(count: usize) -> Vec<Message> {
    let channel_file = crate::paths::channel_file_for_repo("default");
    if !channel_file.exists() {
        return Vec::new();
    }

    match std::fs::read_to_string(&channel_file) {
        Ok(content) => {
            let messages: Vec<Message> = content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            // Return last N messages
            messages.into_iter().rev().take(count).rev().collect()
        }
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Stream event buffer types
// ============================================================================

/// Maximum number of recent events kept per coworker in the stream buffer.
///
/// The plugin polls `plugin.coworker-stream` to render a read-only activity
/// feed. 100 events is enough for several minutes of coworker activity
/// without consuming excessive memory (each event is a small string).
pub const MAX_STREAM_EVENTS_PER_COWORKER: usize = 100;

/// A buffered stream event for the coworker-stream endpoint.
///
/// Stored in a ring buffer per coworker in `DaemonState`.
#[derive(Debug, Clone)]
pub struct BufferedStreamEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub content: String,
}

/// Convert a headless `StreamEvent` into a `BufferedStreamEvent` for the ring buffer.
///
/// Extracts a human-readable event type and content summary from each variant.
/// System init events are included (they mark session starts). Result events
/// include cost info. Assistant events extract text content blocks.
pub fn stream_event_to_buffered(event: &crate::headless::StreamEvent) -> BufferedStreamEvent {
    let now = Utc::now();
    match event {
        crate::headless::StreamEvent::System {
            subtype,
            session_id,
            ..
        } => BufferedStreamEvent {
            timestamp: now,
            event_type: format!("system:{}", subtype),
            content: session_id
                .as_deref()
                .map(|sid| format!("Session initialized ({})", sid))
                .unwrap_or_else(|| format!("System event: {}", subtype)),
        },
        crate::headless::StreamEvent::Assistant { message, .. } => {
            // Extract text content from assistant message blocks
            let content = message
                .get("content")
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            let block_type = block.get("type")?.as_str()?;
                            match block_type {
                                "text" => block.get("text")?.as_str().map(|t| {
                                    if t.len() > 200 {
                                        format!("{}...", &t[..200])
                                    } else {
                                        t.to_string()
                                    }
                                }),
                                "tool_use" => {
                                    let name = block
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("unknown");
                                    Some(format!("Tool: {}", name))
                                }
                                _ => Some(format!("[{}]", block_type)),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();

            BufferedStreamEvent {
                timestamp: now,
                event_type: "assistant".to_string(),
                content,
            }
        }
        crate::headless::StreamEvent::User { .. } => BufferedStreamEvent {
            timestamp: now,
            event_type: "user".to_string(),
            content: "User message".to_string(),
        },
        crate::headless::StreamEvent::Result {
            is_error,
            result,
            total_cost_usd,
            ..
        } => {
            let content = if *is_error {
                result
                    .as_deref()
                    .map(|r| format!("Error: {}", r))
                    .unwrap_or_else(|| "Error (no details)".to_string())
            } else {
                let cost_str = total_cost_usd
                    .map(|c| format!(", cost=${:.4}", c))
                    .unwrap_or_default();
                format!("Turn completed{}", cost_str)
            };

            BufferedStreamEvent {
                timestamp: now,
                event_type: "result".to_string(),
                content,
            }
        }
    }
}

/// Append events to a coworker's ring buffer, enforcing the max size.
///
/// This is the only function that writes to the stream event buffer.
/// Called from the daemon event loop after `drain_events()`.
pub fn append_to_stream_buffer(
    buffer: &std::sync::RwLock<std::collections::HashMap<String, Vec<BufferedStreamEvent>>>,
    coworker_name: &str,
    new_events: Vec<BufferedStreamEvent>,
) {
    if new_events.is_empty() {
        return;
    }
    let mut buf = buffer.write().unwrap();
    let entry = buf.entry(coworker_name.to_lowercase()).or_default();
    entry.extend(new_events);
    // Trim to ring buffer size
    if entry.len() > MAX_STREAM_EVENTS_PER_COWORKER {
        let excess = entry.len() - MAX_STREAM_EVENTS_PER_COWORKER;
        entry.drain(..excess);
    }
}

/// Remove a coworker's stream buffer entry (cleanup on session exit).
pub fn remove_stream_buffer(
    buffer: &std::sync::RwLock<std::collections::HashMap<String, Vec<BufferedStreamEvent>>>,
    coworker_name: &str,
) {
    let mut buf = buffer.write().unwrap();
    buf.remove(&coworker_name.to_lowercase());
}

#[path = "rpc_plugin_tests.rs"]
#[cfg(test)]
mod tests;
