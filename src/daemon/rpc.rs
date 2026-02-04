//! RPC request handlers for the daemon's Unix socket protocol.
//!
//! Each `handle_*` function processes a specific JSON-RPC method and returns
//! a `Response`. The entry point is `handle_connection`, which reads requests
//! from a Unix stream and dispatches them via `handle_request`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{Request, RequestId, Response, RpcError};

use super::constants::*;
use super::helpers::*;
use super::{DaemonState, effects, snapshot};

// ============================================================================
// Connection handling
// ============================================================================

pub(super) async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    state: std::sync::Arc<DaemonState>,
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
                        let response = handle_request(&line, &state).await;
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

// ============================================================================
// Request dispatch
// ============================================================================

/// Process a JSON-RPC request and return a response.
async fn handle_request(line: &str, state: &DaemonState) -> Response {
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

        "coworker.spawn" => {
            let params = request.params.as_ref();
            let resume = params
                .and_then(|p| p.get("resume"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let prompt = params
                .and_then(|p| p.get("prompt"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            handle_coworker_spawn(request.id, state, resume, prompt)
        }

        "coworker.break" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_break(request.id, name, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.report-state" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let phase = params.and_then(|p| p.get("phase")).and_then(|v| v.as_str());
            let task_id = params
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);

            match (name, phase) {
                (Some(name), Some(phase)) => {
                    handle_coworker_report_state(request.id, name, phase, task_id, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.nudge" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");

            match (name, message) {
                (Some(name), Some(message)) => {
                    handle_coworker_nudge(request.id, from, name, message, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.asking" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let question = params
                .and_then(|p| p.get("question"))
                .and_then(|v| v.as_str());

            match (name, question) {
                (Some(name), Some(question)) => {
                    handle_coworker_asking(request.id, name, question, state)
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "status" => handle_status(request.id, state).await,

        "kanban.data" => handle_kanban_data(request.id, state).await,

        "channel.post" => {
            let params = request.params.as_ref();
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");

            match message {
                Some(msg) => handle_channel_post(request.id, from, msg, state).await,
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

        "reminder.create" => {
            let params = request.params.as_ref();
            let trigger = params
                .and_then(|p| p.get("trigger"))
                .and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());

            match (trigger, message) {
                (Some("all-work-merged"), Some(msg)) => {
                    handle_reminder_create(request.id, msg, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "reminder.list" => handle_reminder_list(request.id, state).await,

        "reminder.cancel" => {
            let id = request
                .params
                .as_ref()
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str());

            match id {
                Some(id) => handle_reminder_cancel(request.id, id, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            // Mark in-flight tasks BEFORE executing effects to prevent race conditions.
            // Without this, a TaskDispatchTick firing while effects execute would see
            // the task as still pending and generate a duplicate AssignAndSpawn.
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        "task.updated" => {
            let params = request.params.as_ref();
            let task_id = params
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_str());
            let updater = params
                .and_then(|p| p.get("updater"))
                .and_then(|v| v.as_str());
            let task_list_id = params
                .and_then(|p| p.get("task_list_id"))
                .and_then(|v| v.as_str());

            match (task_id, updater) {
                (Some(task_id), Some(updater)) => {
                    handle_task_updated(request.id, task_id, updater, task_list_id, state)
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "snapshot" => {
            // Collect and return the full WorldSnapshot for debugging/testing.
            // Debug context (channel messages, daemon logs) is only populated here,
            // not during normal tick collection, to avoid I/O overhead on the hot path.
            let snapshot = super::snapshot::collect_world_snapshot(state)
                .await
                .with_debug_context(&state.channel);
            match serde_json::to_value(&snapshot) {
                Ok(value) => Response::success(request.id, value),
                Err(e) => Response::error(
                    request.id,
                    RpcError::new(-32603, format!("Failed to serialize snapshot: {}", e)),
                ),
            }
        }

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

// ============================================================================
// Individual RPC handlers
// ============================================================================

/// Handle coworker.spawn RPC method.
fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
) -> Response {
    // Check dev coworkers limit (reserve slots for reviewers)
    if state.is_at_dev_limit() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "Dev coworkers limit ({}) reached (reserving {} slots for reviewers). Adjust with MIDTOWN_MAX_COWORKERS or max_coworkers in config.toml",
                    state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
                    REVIEW_HEADROOM
                ),
            ),
        );
    }

    // Pass prompt to spawn() - it handles waiting and nudging internally
    // Use shared task list (not isolated) for manual spawns
    let config = crate::tmux::ClaudeLaunchConfig {
        name: String::new(), // spawn() picks a name
        session_mode: if resume {
            crate::tmux::SessionMode::Resume
        } else {
            crate::tmux::SessionMode::Fresh
        },
        task_mode: crate::tmux::TaskMode::Shared {
            repo_name: state.repo_name.clone(),
        },
        role: crate::tmux::CoworkerRole::Coworker,
        initial_prompt: prompt,
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
    };
    match state.coworkers.spawn(&config) {
        Ok(name) => {
            info!("Spawned coworker: {}", name);
            state.broadcast_coworker_update(&name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Called in coworker: {}", name),
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

/// Handle coworker.break RPC method.
fn handle_coworker_break(id: RequestId, name: &str, state: &DaemonState) -> Response {
    state.broadcast_coworker_update(name, "stopped", None);
    match state.coworkers.shutdown(name) {
        Ok(()) => {
            info!("Sent coworker on a break: {}", name);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Sent {} on a break", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to send coworker {} on a break: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.list RPC method.
fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
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

    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
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
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "coworkers": coworkers,
        }),
    )
}

/// Handle coworker.report-state RPC method.
///
/// Stores the coworker's workflow phase in daemon memory and updates the
/// tmux tab display. Replaces the previous file-based state.json approach
/// so the daemon is the single authority for coworker state.
async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    state: &DaemonState,
) -> Response {
    // Parse the phase string into a WorkflowPhase enum
    let phase = match phase_str {
        "claiming" => crate::coworker_state::WorkflowPhase::Claiming,
        "developing" => crate::coworker_state::WorkflowPhase::Developing,
        "testing" => crate::coworker_state::WorkflowPhase::Testing,
        "pull_request" | "pull-request" => crate::coworker_state::WorkflowPhase::PullRequest,
        "reviewing" => crate::coworker_state::WorkflowPhase::Reviewing,
        "debugging" => crate::coworker_state::WorkflowPhase::Debugging,
        "completed" => crate::coworker_state::WorkflowPhase::Completed,
        "idle" => crate::coworker_state::WorkflowPhase::Idle,
        _ => {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Unknown phase: {}", phase_str)),
            );
        }
    };

    // Store in unified coworker record
    let status_display = {
        let mut records = state.coworker_records.write().await;
        crate::rules::set_workflow(&mut records, name, phase, task_id);
        records
            .get(name)
            .and_then(|r| r.display_status())
            .unwrap_or_default()
    };

    // Persist state to file for recovery across daemon restarts.
    // The daemon is the authority for state, but we write to disk so that
    // if the daemon restarts, it can recover the last known workflow phase.
    let report = crate::coworker_state::CoworkerStateReport::new(phase, task_id);
    if let Err(e) = crate::coworker_state::write_state(&state.repo_name, name, &report) {
        debug!("Failed to persist state file for {}: {}", name, e);
    }

    // Update tmux tab display
    if let Err(e) = state
        .coworkers
        .update_status_formatted(name, &status_display)
    {
        debug!("Failed to update tmux tab for {}: {}", name, e);
    }

    info!("Coworker {} reported state: {}", name, status_display);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("{} → {}", name, status_display),
        }),
    )
}

/// Handle coworker.nudge RPC method.
///
/// Sends the nudge directly to the coworker's tmux window without posting to the channel,
/// to avoid the chat monitor seeing the @mention and creating a duplicate nudge.
async fn handle_coworker_nudge(
    id: RequestId,
    _from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Run blocking tmux operation in spawn_blocking to avoid blocking async runtime
    let coworkers = state.coworkers.clone();
    let name_owned = name.to_string();
    let message_owned = message.to_string();

    match tokio::task::spawn_blocking(move || coworkers.nudge(&name_owned, &message_owned)).await {
        Ok(Ok(())) => {
            info!("Nudged coworker {}: {}", name, message);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Nudged coworker: {}", name),
                }),
            )
        }
        Ok(Err(e)) => {
            error!("Failed to nudge coworker {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
        Err(e) => {
            error!("spawn_blocking panic while nudging {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, "Internal error".to_string()))
        }
    }
}

/// Handle coworker.asking RPC method.
///
/// Called when a coworker uses AskUserQuestion tool. This:
/// 1. Posts the question to the channel
/// 2. Nudges the Lead with the question
/// 3. Marks the coworker as waiting for feedback
fn handle_coworker_asking(
    id: RequestId,
    name: &str,
    question: &str,
    state: &DaemonState,
) -> Response {
    // Post question to channel
    let msg = Message::text(name, format!("Question for Lead: {}", question));
    if let Err(e) = state.send_and_broadcast(&msg) {
        error!("Failed to post question to channel: {}", e);
    }

    // Mark the coworker as waiting for feedback in tmux tab and nudge the Lead.
    // Run blocking tmux operations in spawn_blocking to avoid blocking async runtime.
    let coworkers = state.coworkers.clone();
    let name_owned = name.to_string();
    let nudge_message = format!("{} is asking: {}", name, question);

    tokio::task::spawn_blocking(move || {
        // Update tmux tab status
        if let Err(e) = coworkers.update_status_display(&name_owned, Some("waiting for feedback")) {
            debug!("Failed to update tmux tab for {}: {}", name_owned, e);
        }
        // Nudge the Lead with the question
        if let Err(e) = coworkers.nudge("Lead", &nudge_message) {
            debug!("Failed to nudge Lead: {}", e);
        }
    });

    info!("Coworker {} asking: {}", name, question);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Notified Lead about question from {}", name),
        }),
    )
}

/// Handle task.updated RPC — nudge the task owner when someone else updates their task.
///
/// Looks up the task by ID, checks the owner, and if the updater differs from
/// the owner, sends a nudge so the owner sees the change immediately.
///
/// The `task_list_id` parameter (from `CLAUDE_CODE_TASK_LIST_ID` env var) is used to
/// verify the update came from the shared team task list. If it refers to a different
/// session (e.g., a coworker's local subtasks), we skip the lookup to avoid cross-list
/// ID collisions causing spurious nudges.
fn handle_task_updated(
    id: RequestId,
    task_id: &str,
    updater: &str,
    task_list_id: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Check if this update is for the shared team task list
    if !should_lookup_task(task_list_id, &state.repo_name) {
        debug!(
            "task.updated: task_list_id {:?} doesn't match repo {}, skipping lookup",
            task_list_id, state.repo_name
        );
        return Response::success(
            id,
            serde_json::json!({
                "nudged": false,
                "reason": "task list mismatch",
            }),
        );
    }

    let tasks = crate::tasks::read_tasks();
    let task = tasks.iter().find(|t| t.id == task_id);

    let Some(task) = task else {
        info!("task.updated: task {} not found, skipping nudge", task_id);
        return Response::success(
            id,
            serde_json::json!({
                "nudged": false,
                "reason": "task not found",
            }),
        );
    };

    // Use helper function to decide whether to nudge
    let Some((owner, nudge_message)) = should_nudge_task_owner(task, updater) else {
        // Log the specific reason for skipping
        let reason = if task.owner.is_none() {
            "task has no owner"
        } else if task.owner.as_ref().is_some_and(|o| o == updater) {
            "updater is owner"
        } else if task.status == crate::tasks::TaskStatus::Completed {
            "task is completed"
        } else {
            "unknown"
        };
        debug!("task.updated: task {} skipping nudge ({})", task_id, reason);
        return Response::success(
            id,
            serde_json::json!({
                "nudged": false,
                "reason": reason,
            }),
        );
    };

    // Nudge the owner (could be a coworker or Lead).
    // Run in spawn_blocking to avoid blocking the async runtime.
    let coworkers = state.coworkers.clone();
    let owner_clone = owner.clone();
    let nudge_clone = nudge_message.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = coworkers.nudge(&owner_clone, &nudge_clone) {
            debug!("Failed to nudge {} for task update: {}", owner_clone, e);
        }
    });

    info!(
        "task.updated: nudged {} about task {} (updated by {})",
        owner, task_id, updater
    );
    Response::success(
        id,
        serde_json::json!({
            "nudged": true,
            "owner": owner,
        }),
    )
}

/// Check whether a task.updated RPC should look up the task in the main project list.
///
/// Returns true if:
/// - `task_list_id` is None (backwards compatibility with old clients)
/// - `task_list_id` matches `midtown-{repo_name}` (the shared team task list)
///
/// Returns false if `task_list_id` refers to a different session (e.g., a coworker's
/// local subtask list), preventing cross-list ID collisions from causing spurious nudges.
fn should_lookup_task(task_list_id: Option<&str>, repo_name: &str) -> bool {
    let expected = crate::paths::task_list_id_for_repo(repo_name);
    match task_list_id {
        None => true, // Backwards compatibility
        Some(id) => id == expected,
    }
}

/// Determine whether to nudge a task owner about an update.
///
/// Returns `Some((owner, message))` if a nudge should be sent, or `None` if not.
///
/// Nudges are skipped when:
/// - Task has no owner
/// - Updater is the owner (self-update)
/// - Task is already completed (no need to alert about finished work)
fn should_nudge_task_owner(task: &crate::tasks::Task, updater: &str) -> Option<(String, String)> {
    // Skip if no owner
    let owner = task.owner.as_ref()?;

    // Skip if updater is the owner
    if owner == updater {
        return None;
    }

    // Skip completed tasks — they're done, no need to nudge about updates
    if task.status == crate::tasks::TaskStatus::Completed {
        return None;
    }

    let message = format!(
        "Your task #{} ({}) was updated by {} — check the latest changes",
        task.id, task.subject, updater
    );
    Some((owner.clone(), message))
}

/// Remove shell escaping artifacts from channel messages.
///
/// When Claude Code posts messages via its Bash tool, the LLM often escapes `!`
/// as `\!` (to avoid bash history expansion). Since the Bash tool runs in
/// non-interactive mode where history expansion is disabled, the backslash passes
/// through literally. This function cleans up such artifacts.
fn unescape_shell_artifacts(s: &str) -> String {
    s.replace("\\!", "!")
}

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in their tmux tab name.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
pub(super) async fn handle_channel_post(
    id: RequestId,
    from: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Clean up shell escaping artifacts (e.g. "\!" from bash history expansion escaping)
    let message = unescape_shell_artifacts(message);

    // Check for /me prefix (IRC-style action)
    let (content, msg_type) = if let Some(action) = message.strip_prefix("/me ") {
        (action.to_string(), MessageType::Action)
    } else {
        (message.to_string(), MessageType::Text)
    };

    let msg = Message::new(from, content.clone(), msg_type.clone());

    // Use async version to avoid blocking the runtime during file lock acquisition
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to write to channel: {}", e);
        return Response::error(id, RpcError::new(-32603, e.to_string()));
    }

    info!("Channel post from {}: {}", from, message);

    // Track last activity time for coworker (used for silent coworker detection)
    if is_coworker_sender(from) {
        let mut records = state.coworker_records.write().await;
        records
            .entry(from.to_string())
            .or_insert_with(crate::rules::CoworkerRecord::new_spawn)
            .last_activity = Some(Instant::now());
        drop(records); // Release write lock before acquiring read lock
    }

    // Update tmux tab for coworkers when they post /me actions.
    // Prefer structured state from daemon memory (reported via RPC) over
    // parsing the freeform /me message text with keyword matching.
    //
    // Run tmux operations in spawn_blocking to avoid blocking the async
    // runtime. This prevents RPC timeouts when tmux commands are slow.
    if msg_type == MessageType::Action {
        let display_status = {
            let records = state.coworker_records.read().await;
            records.get(from).and_then(|record| record.display_status())
        };

        let coworkers = state.coworkers.clone();
        let from_clone = from.to_string();
        let content_clone = content.clone();

        tokio::task::spawn_blocking(move || {
            if let Some(display) = display_status {
                if let Err(e) = coworkers.update_status_formatted(&from_clone, &display) {
                    debug!("Failed to update tmux tab for {}: {}", from_clone, e);
                }
            } else {
                // Fallback: parse /me message text with keyword matching
                if let Err(e) = coworkers.update_status_display(&from_clone, Some(&content_clone)) {
                    debug!("Failed to update tmux tab for {}: {}", from_clone, e);
                }
            }
        });
    }

    // Nudge lead when user messages arrive (from web UI or TUI input)
    if state.is_user_sender(from) {
        // Check if user is @mentioning specific coworkers or @all
        let has_coworker_mentions =
            !extract_mentions(&content).is_empty() || contains_at_all(&content);
        let has_lead_mention = content.to_lowercase().contains("@lead");

        // Route @mentions in user messages directly to coworkers
        super::chat::route_mentions(state, &msg).await;

        // Only nudge lead if there are no coworker @mentions (regular
        // message for the lead) or if the user also @mentioned the lead.
        // This lets users talk directly to coworkers without the lead
        // acting as a middleman.
        if !has_coworker_mentions || has_lead_mention {
            let nudge_msg = format!("user: {}", content);
            info!("Nudging Lead about user message");
            // Run in spawn_blocking to avoid blocking the async runtime
            let coworkers = state.coworkers.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
                    warn!("Failed to nudge Lead about user message: {}", e);
                }
            });
        } else {
            info!("Skipping Lead nudge — user message routed directly to mentioned coworker(s)");
        }
    }

    // Nudge the Lead when a coworker explicitly mentions @lead
    let content_lower = content.to_lowercase();
    if is_coworker_sender(from) && content_lower.contains("@lead") {
        // Use CooldownTracker to avoid duplicate nudges (expires after 1 hour)
        let should_nudge = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("lead_mention", &msg.id, Duration::from_secs(3600))
        };

        if should_nudge {
            // Record that we're nudging for this message
            {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record("lead_mention", &msg.id);
            }

            // Truncate message for nudge (max 100 chars)
            let summary = if content.len() > 100 {
                format!("{}...", &content[..97])
            } else {
                content.clone()
            };

            let nudge_msg = format!("{} mentioned @lead: {}", from, summary);
            info!("Nudging Lead about @lead mention from {}", from);

            // Nudge the Lead window (spawn_blocking to avoid blocking async runtime)
            let coworkers = state.coworkers.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
                    warn!("Failed to nudge Lead about @lead mention: {}", e);
                }
            });

            // Send push notification to mobile PWA
            state.send_push_notification(&format!("@lead from {}", from), &summary, "mention");
        }
    }

    // Send bell notification and push notification for @user mentions
    // Also recognize @<display_name> if configured (e.g., @Ben)
    let has_user_mention = content_lower.contains("@user")
        || state
            .user_display_name
            .as_ref()
            .is_some_and(|dn| content_lower.contains(&format!("@{}", dn.to_lowercase())));
    if has_user_mention && !state.is_user_sender(from) {
        info!("Bell notification: @user mentioned by {}", from);
        // Run in spawn_blocking to avoid blocking the async runtime
        let coworkers = state.coworkers.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = coworkers.notify_user() {
                warn!("Failed to send bell notification for @user mention: {}", e);
            }
        });
        let display = state.user_display_name.as_deref().unwrap_or("user");
        let summary = if content.len() > 100 {
            format!("{}...", &content[..97])
        } else {
            content.clone()
        };
        state.send_push_notification(&format!("@{} from {}", display, from), &summary, "mention");
    }

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": "Message posted to channel",
        }),
    )
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

/// Handle reminder.create RPC method.
async fn handle_reminder_create(id: RequestId, message: &str, state: &DaemonState) -> Response {
    let mut ps = state.persistent_state.lock().await;
    let reminder_id = ps.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        message.to_string(),
    );

    if let Err(e) = ps.save_for_repo(&state.repo_name) {
        error!("Failed to save daemon-state.json: {}", e);
    }

    let confirmation = format!(
        "Reminder set (id: {}): I'll notify you when all tasks are completed and all PRs are merged. Message: \"{}\"",
        reminder_id, message
    );
    info!("{}", confirmation);
    Response::success(id, serde_json::json!({ "message": confirmation }))
}

/// Handle reminder.list RPC method.
async fn handle_reminder_list(id: RequestId, state: &DaemonState) -> Response {
    let ps = state.persistent_state.lock().await;
    let active = ps.reminders.active();

    if active.is_empty() {
        return Response::success(id, serde_json::json!({ "message": "No active reminders." }));
    }

    let lines: Vec<String> = active
        .iter()
        .map(|r| {
            format!(
                "  {} [{}] \"{}\" (created {})",
                r.id,
                r.trigger,
                r.message,
                r.created_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect();

    let output = format!("Active reminders:\n{}", lines.join("\n"));
    Response::success(id, serde_json::json!({ "message": output }))
}

/// Handle reminder.cancel RPC method.
async fn handle_reminder_cancel(id: RequestId, reminder_id: &str, state: &DaemonState) -> Response {
    let mut ps = state.persistent_state.lock().await;
    if ps.reminders.cancel(reminder_id) {
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            error!("Failed to save daemon-state.json: {}", e);
        }
        let msg = format!("Reminder {} cancelled.", reminder_id);
        info!("{}", msg);
        Response::success(id, serde_json::json!({ "message": msg }))
    } else {
        Response::error(
            id,
            RpcError::new(-32602, format!("Reminder '{}' not found", reminder_id)),
        )
    }
}

// ============================================================================
// Status & Kanban handlers
// ============================================================================

/// Handle status RPC method.
///
/// This handler runs blocking operations (gh CLI, file I/O) in spawn_blocking
/// to avoid blocking the async runtime and causing RPC timeouts.
async fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
    // This is the source of truth for what each coworker is working on
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

    // Get coworkers with their details, looking up current task from task storage
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
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
        .collect();

    // Run blocking operations (gh CLI calls, file I/O) in spawn_blocking
    // to avoid blocking the async runtime and causing RPC timeouts.
    let (pull_requests, tasks, merged_prs, recent_activity) =
        match tokio::task::spawn_blocking(move || {
            let pull_requests = get_open_prs();
            let tasks = get_all_tasks();
            let merged_prs = get_merged_prs();
            let recent_activity = get_recent_channel_activity();
            (pull_requests, tasks, merged_prs, recent_activity)
        })
        .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("spawn_blocking panic in status handler: {}", e);
                return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
            }
        };

    let pending_count = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .count();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "max_coworkers": state.max_coworkers,
            "max_dev_coworkers": state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
            "pending_tasks": pending_count,
            "socket_path": state.socket_path.to_string_lossy(),
            "coworkers": coworkers,
            "tasks": tasks,
            "pull_requests": pull_requests,
            "merged_prs": merged_prs,
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
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get PRs from gh CLI: {}", stderr.trim());
            Vec::new()
        }
        Err(e) => {
            warn!("Failed to execute gh pr list: {}", e);
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

/// Get all tasks from Claude Code task storage with their status.
fn get_all_tasks() -> Vec<serde_json::Value> {
    crate::tasks::read_tasks()
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
                "assignee": task.owner,
            })
        })
        .collect()
}

/// Handle kanban.data RPC method - returns PR data for the kanban board.
///
/// Returns open PRs with author, reviewer, CI status, and timestamps,
/// plus recently merged PRs for the Done column.
/// Handle kanban.data RPC method.
///
/// Runs blocking GraphQL operations in spawn_blocking to avoid blocking
/// the async runtime and causing RPC timeouts.
async fn handle_kanban_data(id: RequestId, state: &DaemonState) -> Response {
    // Get reviewer assignments from GitHubState (best-effort via try_lock)
    let reviewer_assignments: HashMap<u64, crate::github_state::PrReviewerAssignment> = state
        .persistent_state
        .try_lock()
        .map(|ps| ps.github.active_assignments())
        .unwrap_or_default();

    // Clone data needed for the blocking task
    let all_repo_paths = state.all_repo_paths.clone();
    let is_multi_repo = all_repo_paths.len() > 1;

    // Pre-resolve repo full names (this uses caching and is fast)
    let repo_data: Vec<(std::path::PathBuf, String, String)> = all_repo_paths
        .iter()
        .map(|repo_path| {
            let full_name = state.get_repo_full_name(repo_path);
            let label = repo_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            (repo_path.clone(), full_name, label)
        })
        .collect();

    // Run blocking GraphQL operations in spawn_blocking
    let (prs, merged_prs, repos) = match tokio::task::spawn_blocking(move || {
        let mut prs = Vec::new();
        let mut merged_prs = Vec::new();
        let mut repos = Vec::new();

        for (repo_path, full_name, label) in repo_data {
            let repo_label = if is_multi_repo {
                repo_path.file_name().and_then(|s| s.to_str())
            } else {
                None
            };

            repos.push(serde_json::json!({
                "label": label,
                "full_name": full_name,
            }));

            let (open, merged) =
                fetch_kanban_all_prs(&reviewer_assignments, &full_name, &repo_path, repo_label);
            prs.extend(open);
            merged_prs.extend(merged);
        }

        (prs, merged_prs, repos)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in kanban_data handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    Response::success(
        id,
        serde_json::json!({
            "prs": prs,
            "merged_prs": merged_prs,
            "repos": repos,
        }),
    )
}

// ============================================================================
// Kanban / PR data helpers
// ============================================================================

/// GraphQL query that fetches both open and recently merged PRs in a single call.
///
/// This replaces two separate `gh pr list` CLI calls with one GraphQL request,
/// cutting API usage in half for the kanban board.
const KANBAN_GRAPHQL_QUERY: &str = r#"
query($owner: String!, $repo: String!) {
  repository(owner: $owner, name: $repo) {
    openPrs: pullRequests(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number
        title
        author { login }
        createdAt
        body
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup {
                contexts(first: 100) {
                  nodes {
                    __typename
                    ... on CheckRun {
                      status
                      conclusion
                    }
                    ... on StatusContext {
                      state
                    }
                  }
                }
              }
            }
          }
        }
        comments(first: 100) {
          nodes {
            body
            createdAt
          }
        }
      }
    }
    mergedPrs: pullRequests(states: MERGED, first: 10, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        mergedAt
      }
    }
  }
}
"#;

/// Fetch both open and merged PRs for a repo using a single GraphQL call.
///
/// `name_with_owner` should be `"owner/repo"` (e.g. `"anthropics/midtown"`).
/// Returns `(open_prs, merged_prs)` formatted for the kanban board.
/// Falls back to empty vectors on failure.
fn fetch_kanban_all_prs(
    reviewer_assignments: &HashMap<u64, crate::github_state::PrReviewerAssignment>,
    name_with_owner: &str,
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let parts: Vec<&str> = name_with_owner.splitn(2, '/').collect();
    if parts.len() != 2 {
        debug!("Unexpected nameWithOwner format: {}", name_with_owner);
        return (Vec::new(), Vec::new());
    }
    let (owner, repo_name) = (parts[0], parts[1]);

    // Execute the batched GraphQL query
    let graphql_output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={}", owner),
            "-F",
            &format!("repo={}", repo_name),
            "-f",
            &format!("query={}", KANBAN_GRAPHQL_QUERY),
        ])
        .output();

    let data = match graphql_output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse kanban GraphQL response: {}", e);
                    return (Vec::new(), Vec::new());
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "GitHub API query failed for {}: {}",
                name_with_owner,
                stderr.trim()
            );
            return (Vec::new(), Vec::new());
        }
        Err(e) => {
            warn!("Failed to execute gh command: {}", e);
            return (Vec::new(), Vec::new());
        }
    };

    let repository = match data.pointer("/data/repository") {
        Some(r) => r,
        None => {
            debug!("No repository data in kanban GraphQL response");
            return (Vec::new(), Vec::new());
        }
    };

    // Process open PRs
    let open_prs = repository
        .pointer("/openPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;

                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let github_author = pr
                        .pointer("/author/login")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let author = extract_coworker_from_pr_body(body).unwrap_or(github_author);

                    let created_at = pr.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");

                    // Extract CI status from the last commit's statusCheckRollup
                    let check_contexts: Vec<serde_json::Value> = pr
                        .pointer("/commits/nodes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.last())
                        .and_then(|node| node.pointer("/commit/statusCheckRollup/contexts/nodes"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ci_status = kanban_ci_status(&check_contexts);

                    // Extract reviewer from comments
                    let comments: Vec<serde_json::Value> = pr
                        .pointer("/comments/nodes")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let (comment_reviewer, reviewed_at) =
                        extract_reviewer_from_pr_comments(&comments);

                    // Use comment reviewer, or fall back to assigned reviewer.
                    // Track whether the review was actually posted (vs just assigned).
                    let (reviewer, reviewer_assigned_at, review_posted) =
                        if let Some(reviewer) = comment_reviewer {
                            (Some(reviewer), reviewed_at, true)
                        } else if let Some(assignment) = reviewer_assignments.get(&number) {
                            (
                                Some(assignment.reviewer.clone()),
                                Some(assignment.assigned_at.to_rfc3339()),
                                false,
                            )
                        } else {
                            (None, None, false)
                        };

                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "author": author,
                        "created_at": created_at,
                        "ci_status": ci_status,
                        "reviewer": reviewer,
                        "reviewed_at": reviewer_assigned_at,
                        "review_posted": review_posted,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // Process merged PRs
    let merged_prs = repository
        .pointer("/mergedPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;
                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let merged_at = pr
                        .get("mergedAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "merged_at": merged_at,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    (open_prs, merged_prs)
}

/// Extract coworker name from PR body frontmatter (<!-- midtown: name -->).
fn extract_coworker_from_pr_body(body: &str) -> Option<String> {
    let marker = "midtown:";
    let marker_pos = body.find(marker)?;
    let before = &body[..marker_pos];
    if !before.contains("<!--") {
        return None;
    }
    let after_marker = &body[marker_pos + marker.len()..];
    let end = after_marker.find("-->")?;
    let name = after_marker[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract reviewer name and timestamp from PR comments.
fn extract_reviewer_from_pr_comments(
    comments: &[serde_json::Value],
) -> (Option<String>, Option<String>) {
    for comment in comments {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if !body.contains("Code Review") && !body.contains("Code review") {
            continue;
        }

        // Try frontmatter first
        let reviewer = extract_coworker_from_pr_body(body).or_else(|| {
            // Fall back to "Code Review by {name}" header
            for line in body.lines() {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if let Some(rest) = trimmed
                    .strip_prefix("Code Review by ")
                    .or_else(|| trimmed.strip_prefix("Code review by "))
                {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        });

        if let Some(name) = reviewer {
            let created_at = comment
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return (Some(name), created_at);
        }
    }
    (None, None)
}

/// Compute CI status string from statusCheckRollup array.
fn kanban_ci_status(checks: &[serde_json::Value]) -> &'static str {
    if checks.is_empty() {
        return "unknown";
    }

    let mut has_running = false;
    let mut has_failed = false;
    let mut has_passed = false;

    for check in checks {
        let status = check.get("status").and_then(|v| v.as_str());
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        let state = check.get("state").and_then(|v| v.as_str());

        if let Some(status) = status {
            match status {
                "IN_PROGRESS" | "QUEUED" | "WAITING" | "PENDING" => has_running = true,
                "COMPLETED" => match conclusion {
                    Some("SUCCESS") => has_passed = true,
                    Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => has_failed = true,
                    _ => {}
                },
                _ => {}
            }
        }

        if let Some(state) = state {
            match state {
                "PENDING" => has_running = true,
                "SUCCESS" => has_passed = true,
                "FAILURE" | "ERROR" => has_failed = true,
                _ => {}
            }
        }
    }

    if has_failed {
        "failed"
    } else if has_running {
        "running"
    } else if has_passed {
        "passed"
    } else {
        "unknown"
    }
}

/// Get recently merged PRs from GitHub using gh CLI.
fn get_merged_prs() -> Vec<serde_json::Value> {
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
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get merged PRs from gh CLI: {}", stderr.trim());
            Vec::new()
        }
        Err(e) => {
            warn!("Failed to execute gh pr list (merged): {}", e);
            Vec::new()
        }
    }
}

/// Get recent channel activity.
fn get_recent_channel_activity() -> Vec<serde_json::Value> {
    // Try to read from the default channel location
    let channel_file = crate::paths::channel_file_for_repo("default");

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_shell_artifacts_exclamation() {
        assert_eq!(
            unescape_shell_artifacts("Game time\\! Let's go"),
            "Game time! Let's go"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_multiple_exclamations() {
        assert_eq!(
            unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
            "Wow! Amazing! Done!"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_no_escapes() {
        assert_eq!(
            unescape_shell_artifacts("Normal message with ! marks"),
            "Normal message with ! marks"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_preserves_other_backslashes() {
        assert_eq!(
            unescape_shell_artifacts("path\\to\\file and \\!"),
            "path\\to\\file and !"
        );
    }

    #[test]
    fn test_extract_coworker_from_pr_body() {
        assert_eq!(
            extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
            Some("york".to_string())
        );
        assert_eq!(
            extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
            Some("park".to_string())
        );
        assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
        assert_eq!(extract_coworker_from_pr_body(""), None);
    }

    #[test]
    fn test_extract_reviewer_from_pr_comments() {
        let comments = vec![serde_json::json!({
            "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
            "createdAt": "2026-01-29T10:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("lexington".to_string()));
        assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

        let comments = vec![serde_json::json!({
            "body": "## Code Review by vernon\nLGTM",
            "createdAt": "2026-01-29T11:00:00Z"
        })];
        let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("vernon".to_string()));

        let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
        assert_eq!(reviewer, None);
    }

    #[test]
    fn test_kanban_ci_status() {
        assert_eq!(kanban_ci_status(&[]), "unknown");
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})
            ]),
            "passed"
        );
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})
            ]),
            "failed"
        );
        assert_eq!(
            kanban_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
            "running"
        );
    }

    #[test]
    fn test_should_lookup_task_matching_task_list() {
        // When task_list_id matches the expected midtown-<repo>, should proceed
        assert!(should_lookup_task(Some("midtown-myrepo"), "myrepo"));
    }

    #[test]
    fn test_should_lookup_task_none_backwards_compat() {
        // When task_list_id is None (old clients), should proceed for backwards compatibility
        assert!(should_lookup_task(None, "myrepo"));
    }

    #[test]
    fn test_should_lookup_task_different_session() {
        // When task_list_id is a different session (e.g., local coworker subtasks),
        // should NOT proceed to avoid cross-list ID collisions
        assert!(!should_lookup_task(
            Some("some-random-uuid-session"),
            "myrepo"
        ));
    }

    #[test]
    fn test_should_lookup_task_different_repo() {
        // When task_list_id is for a different repo, should NOT proceed
        assert!(!should_lookup_task(Some("midtown-otherrepo"), "myrepo"));
    }

    #[test]
    fn test_should_nudge_task_owner_in_progress_task() {
        // In-progress task with owner, updated by someone else — should nudge
        let task = crate::tasks::Task {
            id: "42".to_string(),
            subject: "Fix the bug".to_string(),
            status: crate::tasks::TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        let result = should_nudge_task_owner(&task, "lead");
        assert!(result.is_some());
        let (owner, message) = result.unwrap();
        assert_eq!(owner, "york");
        assert!(message.contains("task #42"));
        assert!(message.contains("lead"));
    }

    #[test]
    fn test_should_nudge_task_owner_completed_task() {
        // Completed task — should NOT nudge (this is the bug fix)
        let task = crate::tasks::Task {
            id: "42".to_string(),
            subject: "Fix the bug".to_string(),
            status: crate::tasks::TaskStatus::Completed,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        let result = should_nudge_task_owner(&task, "lead");
        assert!(result.is_none(), "Should not nudge for completed tasks");
    }

    #[test]
    fn test_should_nudge_task_owner_no_owner() {
        // Task without owner — should NOT nudge
        let task = crate::tasks::Task {
            id: "42".to_string(),
            subject: "Fix the bug".to_string(),
            status: crate::tasks::TaskStatus::InProgress,
            owner: None,
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        let result = should_nudge_task_owner(&task, "lead");
        assert!(result.is_none());
    }

    #[test]
    fn test_should_nudge_task_owner_self_update() {
        // Owner updating their own task — should NOT nudge
        let task = crate::tasks::Task {
            id: "42".to_string(),
            subject: "Fix the bug".to_string(),
            status: crate::tasks::TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        let result = should_nudge_task_owner(&task, "york");
        assert!(
            result.is_none(),
            "Should not nudge when owner updates their own task"
        );
    }

    #[test]
    fn test_should_nudge_task_owner_pending_task() {
        // Pending task with owner, updated by someone else — should nudge
        let task = crate::tasks::Task {
            id: "42".to_string(),
            subject: "Fix the bug".to_string(),
            status: crate::tasks::TaskStatus::Pending,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        let result = should_nudge_task_owner(&task, "lead");
        assert!(result.is_some());
    }
}
