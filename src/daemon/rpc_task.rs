//! Task-related RPC handlers.
//!
//! Handles `task.create`, `task.update`, `task.done`, `task.metadata`,
//! `task.request`, and `task.claim` methods, plus their supporting helpers
//! (model/channel mapping, active form generation).

use tracing::{debug, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Helper functions
// ============================================================================

/// Generate a present-continuous `activeForm` from a task subject.
///
/// Converts imperative subjects like "Fix auth bug" → "Fixing auth bug".
/// Falls back to "Working on: <subject>" for unrecognized patterns.
fn generate_active_form(subject: &str) -> String {
    let trimmed = subject.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    let rest = trimmed.strip_prefix(first_word).unwrap_or("").trim_start();

    // Common imperative verbs → present continuous
    let continuous = match first_word.to_lowercase().as_str() {
        "add" => "Adding",
        "fix" => "Fixing",
        "update" => "Updating",
        "remove" => "Removing",
        "implement" => "Implementing",
        "refactor" => "Refactoring",
        "create" => "Creating",
        "build" => "Building",
        "review" => "Reviewing",
        "address" => "Addressing",
        "debug" => "Debugging",
        "test" => "Testing",
        "move" => "Moving",
        "rename" => "Renaming",
        "delete" => "Deleting",
        "replace" => "Replacing",
        "revert" => "Reverting",
        "migrate" => "Migrating",
        "upgrade" => "Upgrading",
        "clean" => "Cleaning",
        "configure" => "Configuring",
        "enable" => "Enabling",
        "disable" => "Disabling",
        "simplify" => "Simplifying",
        _ => return format!("Working on: {}", trimmed),
    };

    if rest.is_empty() {
        continuous.to_string()
    } else {
        format!("{} {}", continuous, rest)
    }
}

/// Validate model format: must be "provider/model" with exactly one slash
/// and a supported provider.
///
/// Valid examples: "claude/opus", "claude/sonnet", "codex/o3", "codex/o4-mini"
/// Invalid: "claude-opus" (no slash), "claude/opus/extra" (multiple slashes),
///          "/opus" (empty provider), "claude/" (empty model),
///          "unknown/opus" (unsupported provider)
fn validate_model_format(model: &str) -> Result<(), String> {
    let parts: Vec<&str> = model.split('/').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid model format '{}': must be '<provider>/<model>' (e.g., claude/opus)",
            model
        ));
    }
    if parts[0].is_empty() {
        return Err(format!(
            "Invalid model format '{}': provider cannot be empty",
            model
        ));
    }
    if parts[1].is_empty() {
        return Err(format!(
            "Invalid model format '{}': model cannot be empty",
            model
        ));
    }
    // Reject whitespace in provider or model
    if parts[0] != parts[0].trim() || parts[1] != parts[1].trim() {
        return Err(format!(
            "Invalid model format '{}': provider and model must not contain leading/trailing whitespace",
            model
        ));
    }

    // Validate provider is supported
    use std::str::FromStr;
    crate::auth::AuthProvider::from_str(parts[0])
        .map_err(|e| format!("Invalid model format '{}': {}", model, e))?;

    Ok(())
}

/// Apply a task-to-model mapping update to persistent state.
///
/// On `task.create`: pass `model` from the RPC params. Valid non-empty values are stored;
/// `None` or empty strings are ignored. Invalid formats return an error.
///
/// On `task.update`: pass `model` from the RPC params. Valid non-empty values set/overwrite
/// the mapping; an empty string clears it; `None` means no change.
///
/// Returns `Ok(true)` if the mapping was modified (caller should save persistent state).
/// Returns `Ok(false)` if no change was made.
/// Returns `Err` if the model format is invalid.
#[cfg(test)]
fn apply_task_model_mapping(
    task_model: &mut std::collections::HashMap<String, String>,
    task_id: &str,
    model: Option<&str>,
    allow_clear: bool,
) -> Result<bool, String> {
    match model {
        Some(m) if m.is_empty() && allow_clear => {
            // Empty string means clear the mapping (only on update, not create)
            // Returns true only if a mapping was actually removed
            Ok(task_model.remove(task_id).is_some())
        }
        Some(m) if !m.is_empty() => {
            // Validate format before storing
            validate_model_format(m)?;
            task_model.insert(task_id.to_string(), m.to_string());
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Apply a task-to-channel mapping update to persistent state.
///
/// On `task.create`: pass `channel` from the RPC params. Non-empty values are stored;
/// `None` or empty strings are ignored.
///
/// On `task.update`: pass `channel` from the RPC params. Non-empty values set/overwrite
/// the mapping; an empty string clears it; `None` means no change.
///
/// Returns `true` if the mapping was modified (caller should save persistent state).
#[cfg(test)]
fn apply_task_channel_mapping(
    task_channel: &mut std::collections::HashMap<String, String>,
    task_id: &str,
    channel: Option<&str>,
    allow_clear: bool,
) -> bool {
    match channel {
        Some(ch) if ch.is_empty() && allow_clear => {
            // Empty string means clear the mapping (only on update, not create)
            // Returns true only if a mapping was actually removed
            task_channel.remove(task_id).is_some()
        }
        Some(ch) if !ch.is_empty() => {
            task_channel.insert(task_id.to_string(), ch.to_string());
            true
        }
        _ => false,
    }
}

/// Determine the `from` author for a "created task:" channel notification.
///
/// Returns "lead" when the task is in the main channel (i.e., `task_channel`
/// matches `main_channel`), because the project lead owns the main channel.
/// Returns the channel name for topic channels, because channel leads have the
/// same session name as their channel (`channel_lead_session_name` is identity).
pub(crate) fn task_created_message_author(task_channel: &str, main_channel: &str) -> String {
    if task_channel == main_channel {
        "lead".to_string()
    } else {
        task_channel.to_string()
    }
}

/// Build the "created task:" announcement message, threading it under `thread_id`
/// when present.
pub(crate) fn task_announcement_message(
    channel: &str,
    author: &str,
    subject: &str,
    thread_id: Option<&str>,
) -> Message {
    let content = format!("created task: {}", subject);
    if let Some(tid) = thread_id {
        Message::thread_reply(channel, author, content, tid, MessageType::Text)
    } else {
        Message::for_channel(channel, author, content, MessageType::Text)
    }
}

/// Resolve the effective channel for task routing, announcement, and nudge.
///
/// When a task is created with `--channel <name>` pointing to an archived
/// channel (e.g., "daemon"), messages cannot be posted there and no channel
/// lead is active for it. Falls back to the ops channel so the ops channel
/// lead tracks the task instead.
///
/// The effective channel is stored in both the task JSON and `ps.task_channel`
/// so that all downstream routing (MIDTOWN_CHANNEL injection, insight posting,
/// thread routing via `handle_task_metadata`) uses the correct channel.
///
/// If the ops channel is itself archived (defensive edge case), falls back to
/// `main_channel` to avoid a silent routing failure.
pub(crate) fn resolve_effective_task_channel<'a>(
    task_channel: &'a str,
    is_archived: bool,
    is_ops_archived: bool,
    main_channel: &'a str,
) -> &'a str {
    if is_archived {
        if is_ops_archived {
            warn!(
                "Both channel '{}' and ops channel are archived — falling back to main channel",
                task_channel
            );
            main_channel
        } else {
            super::constants::OPS_CHANNEL
        }
    } else {
        task_channel
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// Handle task.request RPC — a coworker surfaces work that should be a separate task.
///
/// Posts a formatted message to the channel so the lead can see the request
/// and decide whether to create a task for it.
pub(super) async fn handle_task_request(
    id: RequestId,
    from: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    let channel_message = format!(
        "@{} [Task Request] from {}: \"{}\"",
        state.project_name, from, message
    );

    let msg = Message::for_channel(
        state.default_channel_name(),
        &state.project_name,
        channel_message.clone(),
        MessageType::Text,
    );

    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post task request to channel: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("Failed to post: {}", e)));
    }

    info!("Task request from {}: {}", from, message);
    Response::success(
        id,
        serde_json::json!({
            "posted": true,
            "from": from,
        }),
    )
}

/// Handle task.create RPC — daemon creates a task directly in shared storage.
///
/// Creates the task with the specified channel (or the project's default channel).
/// Dispatch for the new task happens on the next `TaskDispatchTick` via the
/// canonical event loop pipeline.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_task_create(
    id: RequestId,
    subject: &str,
    description: &str,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    pr: Option<u64>,
    plan: Option<&str>,
    agent_name: Option<&str>,
    thread_id: Option<&str>,
    parent: Option<&str>,
    agent_type: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Require agent_name
    let agent_name = match agent_name {
        Some(name) if !name.is_empty() => name,
        _ => {
            return Response::error(
                id,
                RpcError::new(-32602, "agent_name is required".to_string()),
            );
        }
    };

    // Check agent_name uniqueness via TaskStore
    if state.task_store.is_name_in_use(agent_name) {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "agent_name '{}' is already in use by an active task",
                    agent_name
                ),
            ),
        );
    }

    // Validate model format if provided
    if let Some(m) = model
        && !m.is_empty()
        && let Err(e) = validate_model_format(m)
    {
        return Response::error(id, RpcError::new(-32602, e));
    }

    // Generate active_form (present continuous) from subject for task UI spinner
    let _active_form = generate_active_form(subject);

    // Determine the requested channel, then resolve to an effective channel.
    // Archived channels (e.g., "daemon") cannot receive messages and have no
    // active channel lead, so we redirect to ops. The effective channel is stored
    // in both the task JSON and ps.task_channel so all downstream routing
    // (MIDTOWN_CHANNEL injection, insight posting, thread routing) is consistent.
    let requested_channel = channel.unwrap_or(&state.project_name);
    let is_archived = state.channel_router.is_channel_archived(requested_channel);
    let is_ops_archived = is_archived
        && state
            .channel_router
            .is_channel_archived(super::constants::OPS_CHANNEL);
    let effective_channel = resolve_effective_task_channel(
        requested_channel,
        is_archived,
        is_ops_archived,
        state.default_channel_name(),
    );
    if is_archived {
        info!(
            "Task channel '{}' is archived — redirecting to '{}'",
            requested_channel, effective_channel
        );
    }

    // Normalize parent
    let normalized_parent = parent.map(|p| {
        p.strip_prefix('!')
            .or_else(|| p.strip_prefix('#'))
            .unwrap_or(p)
            .to_string()
    });

    // Create task via TaskStore
    let task_id = state.task_store.next_task_id().to_string();
    let new_task = crate::task_store::Task {
        id: task_id.clone(),
        subject: subject.to_string(),
        status: crate::task_store::TaskStatus::Pending,
        description: if description.is_empty() {
            None
        } else {
            Some(description.to_string())
        },
        blocked_by: blocked_by
            .unwrap_or(&[])
            .iter()
            .map(|s| s.to_string())
            .collect(),
        channel: Some(effective_channel.to_string()),
        pr,
        agent_name: agent_name.to_string(),
        agent_type: agent_type.unwrap_or("midtown-code-author").to_string(),
        session_id: None,
        parent: normalized_parent.clone(),
        message_id: None,
        thread_id: thread_id.map(|t| t.to_string()),
        model: model.map(|m| m.to_string()),
        plan: plan.map(|p| p.to_string()),
        placeholder_comment_id: None,
        restart_count: 0,
        execution_skill: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    if let Err(e) = state.task_store.save(&new_task) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to create task: {}", e)),
        );
    }

    // Validate model format if provided
    if let Some(m) = model
        && !m.is_empty()
        && let Err(e) = validate_model_format(m)
    {
        return Response::error(id, RpcError::new(-32602, e));
    }

    // For child tasks, inherit the parent's thread if no explicit thread_id was given.
    // This ensures child tasks thread under the same conversation as the parent.
    if thread_id.is_none()
        && let Some(ref parent_id) = normalized_parent
        && let Ok(parent_task) = state.task_store.load(parent_id)
        && let Some(ref parent_thread) = parent_task.thread_id
    {
        // Re-save the new task with the inherited thread_id
        if let Ok(mut task) = state.task_store.load(&task_id) {
            task.thread_id = Some(parent_thread.clone());
            let _ = state.task_store.save(&task);
        }
    }

    // Post to the effective channel so the right team sees it, attributed to the
    // channel lead. Capture message ID for task-as-thread feature.
    // Only store the mapping if the write succeeds — a failed write means no channel
    // message exists, so storing the ID would create an orphan thread root.
    //
    // Resolve the effective thread_id: use the explicit --thread-id if provided,
    // otherwise check if the child inherited a thread from its parent.
    let effective_thread_id: Option<String> = match thread_id {
        Some(t) => Some(t.to_string()),
        None => state
            .task_store
            .load(&task_id)
            .ok()
            .and_then(|t| t.thread_id.clone()),
    };
    let author = task_created_message_author(effective_channel, state.default_channel_name());
    let msg = task_announcement_message(
        effective_channel,
        &author,
        subject,
        effective_thread_id.as_deref(),
    );
    let announcement_message_id = msg.id.clone();
    let mut event_message_id = None;
    let mut event_thread_id = effective_thread_id;
    match state.send_and_broadcast_async(&msg).await {
        Ok(()) => {
            event_message_id = Some(announcement_message_id.clone());
            // Update TaskStore with the announcement message ID
            if let Ok(mut task) = state.task_store.load(&task_id) {
                task.message_id = Some(announcement_message_id.clone());
                // Default thread_id to the announcement message ID when no
                // thread_id was resolved (explicit --thread-id, or inherited from
                // parent). This ensures SpawnForTask picks up a bound_thread_id
                // so coworker messages auto-route to the task's thread.
                if task.thread_id.is_none() {
                    task.thread_id = Some(announcement_message_id.clone());
                    event_thread_id = Some(announcement_message_id);
                }
                if let Err(e) = state.task_store.save(&task) {
                    warn!("Failed to save task {} message_id: {}", task_id, e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to post task creation to channel: {}", e);
        }
    }

    // Nudge the effective channel's lead about the new task and emit workflow event
    let nudge_effect = crate::daemon::effects::Effect::NudgeChannelLead {
        channel_name: effective_channel.to_string(),
        reason: crate::daemon::wake_reason::WakeReason::TaskCreated {
            task_id: task_id.clone(),
            subject: subject.to_string(),
        },
    };
    let description_for_event = if description.is_empty() {
        None
    } else {
        Some(description.to_string())
    };
    let workflow_effect = crate::daemon::effects::Effect::EmitWorkflowEvent(
        crate::workflow::WorkflowEvent::TaskCreated {
            channel: effective_channel.to_string(),
            task_id: task_id.clone(),
            subject: subject.to_string(),
            description: description_for_event,
            thread_id: event_thread_id,
            message_id: event_message_id,
        },
    );
    crate::daemon::effects::execute_effects(vec![nudge_effect, workflow_effect], state).await;

    info!("Created task !{}: {}", task_id, subject);
    Response::success(
        id,
        serde_json::json!({
            "type": "message",
            "message": format!("Task !{} created: {}", task_id, subject),
        }),
    )
}

/// Handle task.update RPC — update specific fields on a task directly.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_task_update(
    id: RequestId,
    task_id: &str,
    status: Option<&str>,
    description: Option<&str>,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    pr: Option<u64>,
    plan: Option<&str>,
    session_id: Option<&str>,
    message_id: Option<&str>,
    thread_id: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Validate status if provided
    if let Some(s) = status
        && !["pending", "in_progress", "completed"].contains(&s)
    {
        return Response::error(id, RpcError::new(-32602, format!("Invalid status: {}", s)));
    }

    let status_enum = status.map(|s| match s {
        "in_progress" => crate::task_store::TaskStatus::InProgress,
        "completed" => crate::task_store::TaskStatus::Completed,
        _ => crate::task_store::TaskStatus::Pending,
    });

    if let Err(e) = state.task_store.update_task_fields(
        task_id,
        None, // agent_name
        status_enum,
        description,
        blocked_by,
        channel,
        pr,
    ) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to update task: {}", e)),
        );
    }

    // Clear assignment when task is completed or reset to pending
    if matches!(status, Some("completed") | Some("pending")) {
        state.clear_task_assignment_by_task(task_id).await;
    }

    // Validate model format if provided
    if let Some(m) = model
        && !m.is_empty()
        && let Err(e) = validate_model_format(m)
    {
        return Response::error(id, RpcError::new(-32602, e));
    }

    // Update TaskStore with additional fields
    if let Ok(mut store_task) = state.task_store.load(task_id) {
        if let Some(s) = status {
            store_task.status = match s {
                "pending" => crate::task_store::TaskStatus::Pending,
                "in_progress" => crate::task_store::TaskStatus::InProgress,
                "completed" => crate::task_store::TaskStatus::Completed,
                _ => store_task.status,
            };
        }
        if let Some(desc) = description {
            store_task.description = if desc.is_empty() {
                None
            } else {
                Some(desc.to_string())
            };
        }
        if let Some(bb) = blocked_by {
            store_task.blocked_by = bb.to_vec();
        }
        if let Some(ch) = channel {
            if ch.is_empty() {
                store_task.channel = None;
                store_task.thread_id = None;
            } else {
                // When channel changes, clear stale thread_id (it pointed to a
                // message in the old channel).
                if store_task.channel.as_deref() != Some(ch) {
                    store_task.thread_id = None;
                }
                store_task.channel = Some(ch.to_string());
            }
        }
        if let Some(m) = model {
            store_task.model = if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            };
        }
        if let Some(p) = pr {
            store_task.pr = Some(p);
        }
        if let Some(p) = plan {
            store_task.plan = if p.is_empty() {
                None
            } else {
                Some(p.to_string())
            };
        }
        if let Some(sid) = session_id {
            store_task.session_id = if sid.is_empty() {
                None
            } else {
                Some(sid.to_string())
            };
        }
        if let Some(mid) = message_id {
            store_task.message_id = if mid.is_empty() {
                None
            } else {
                Some(mid.to_string())
            };
        }
        if let Some(tid) = thread_id {
            store_task.thread_id = if tid.is_empty() {
                None
            } else {
                Some(tid.to_string())
            };
        }
        if let Err(e) = state.task_store.save(&store_task) {
            warn!("Failed to update TaskStore task {}: {}", task_id, e);
        } else {
            state.update_task_index(&store_task).await;
        }
    }

    info!("Updated task !{}", task_id);
    let response = Response::success(
        id,
        serde_json::json!({
            "type": "message",
            "message": format!("Task !{} updated", task_id),
        }),
    );
    debug!("Returning response: {:?}", response);
    response
}

/// Handle task.done RPC — mark a task as completed directly.
pub(super) async fn handle_task_done(
    id: RequestId,
    task_id: &str,
    state: &DaemonState,
) -> Response {
    let dir_key = state.paths.dir_key().to_string();

    if let Err(e) = state.task_store.complete_task(task_id) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to complete task: {}", e)),
        );
    }

    // Mark worktree as completed (for time-based cleanup)
    {
        let mut ps = state.persistent_state.lock().await;
        if let Some(wt_id) = ps.worktree_registry.find_worktree_by_task(task_id) {
            ps.worktree_registry
                .mark_completed(&wt_id, chrono::Utc::now());
            if let Err(e) = ps.save_for_repo(&dir_key) {
                warn!("Failed to save worktree completion timestamp: {}", e);
            }
        }
    }

    // Clear session-based task assignment tracking
    state.clear_task_assignment_by_task(task_id).await;

    // Unblock dependent tasks
    if let Err(e) = state.task_store.clear_blocked_by(task_id) {
        warn!("Failed to clear blockedBy for task !{}: {}", task_id, e);
    }

    // Also update TaskStore
    if let Ok(mut store_task) = state.task_store.load(task_id) {
        store_task.status = crate::task_store::TaskStatus::Completed;
        if let Err(e) = state.task_store.save(&store_task) {
            warn!(
                "Failed to update TaskStore task {} to completed: {}",
                task_id, e
            );
        } else {
            state.update_task_index(&store_task).await;
        }
    }

    info!("Completed task !{}", task_id);
    Response::success(
        id,
        serde_json::json!({
            "type": "message",
            "message": format!("Task !{} completed", task_id),
        }),
    )
}

/// Handle task.metadata RPC — return daemon-side metadata for a task.
///
/// Returns channel and model mappings stored in DaemonPersistentState.
/// These are stored separately from Claude Code's native task storage.
/// Returns an error if the task does not exist in native task storage.
pub(super) async fn handle_task_metadata(
    id: RequestId,
    task_id: &str,
    state: &DaemonState,
) -> Response {
    // Try TaskStore first, then fall back to native task storage + persistent state
    if let Ok(store_task) = state.task_store.load(task_id) {
        return Response::success(
            id,
            serde_json::json!({
                "channel": store_task.channel,
                "model": store_task.model,
                "plan": store_task.plan,
                "message_id": store_task.message_id,
                "thread_id": store_task.thread_id,
                "parent": store_task.parent,
                "agent_type": store_task.agent_type,
            }),
        );
    }

    // Fallback: verify the task exists in native task storage before returning metadata.
    let tasks = state.task_store.load_all();
    if !tasks.iter().any(|t| t.id == task_id) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    }

    // Read task metadata from TaskStore.
    let task = state.task_store.load(task_id).ok();
    let channel = task.as_ref().and_then(|t| t.channel.clone());
    let model = task.as_ref().and_then(|t| t.model.clone());
    let plan = task.as_ref().and_then(|t| t.plan.clone());
    let execution_skill = task.as_ref().and_then(|t| t.execution_skill.clone());
    let message_id = task.as_ref().and_then(|t| t.message_id.clone());
    let thread_id = task.as_ref().and_then(|t| t.thread_id.clone());
    let parent = task.as_ref().and_then(|t| t.parent.clone());
    let agent_type = task.as_ref().map(|t| t.agent_type.clone());

    Response::success(
        id,
        serde_json::json!({
            "channel": channel,
            "model": model,
            "plan": plan,
            "execution_skill": execution_skill,
            "message_id": message_id,
            "thread_id": thread_id,
            "parent": parent,
            "agent_type": agent_type,
        }),
    )
}

/// Handle task.claim RPC — a coworker claims a task by writing directly to disk.
///
/// Validates the task exists and is pending, then sets owner and status to in_progress
/// directly. No Lead proxy needed. Posts a task divider to the coworker's DM channel.
pub(super) async fn handle_task_claim(
    id: RequestId,
    task_id: &str,
    from: &str,
    state: &DaemonState,
) -> Response {
    let tasks = state.task_store.load_all();
    let task = tasks.iter().find(|t| t.id == task_id);

    let Some(task) = task else {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    };

    if task.status != crate::task_store::TaskStatus::Pending {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "Task !{} is not pending (status: {:?})",
                    task_id, task.status
                ),
            ),
        );
    }

    let task_subject = task.subject.clone();
    let _dir_key = state.paths.dir_key().to_string();

    // Write owner and status directly to disk (with retry on transient failures).
    // Disk write happens BEFORE in-memory recording so that a failure leaves
    // no stale in-memory state. Without reconcile_stale_claims, consistency
    // depends on this ordering.
    let mut last_err = None;
    for attempt in 0..3 {
        match state.task_store.update_task_fields(
            task_id,
            Some(from),
            Some(crate::task_store::TaskStatus::InProgress),
            None,
            None,
            None,
            None,
        ) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                warn!(
                    "Task claim disk write attempt {} failed for task !{}: {}",
                    attempt + 1,
                    task_id,
                    e
                );
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }

    if let Some(e) = last_err {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to claim task after retries: {}", e)),
        );
    }

    // Update session-based task assignment (only after disk write succeeds)
    {
        let mut ps = state.persistent_state.lock().await;
        if let Some(record) = ps.session_by_name_mut(&from.to_lowercase()) {
            record.task_id = Some(task_id.to_string());
        }
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to save state after task.claim assignment: {}", e);
        }
    }

    // Post task divider to the coworker's DM channel.
    // Reuse build_dm_separator_effect (already tested in effects_tests.rs) to
    // produce the PostSystemMessage effect, then execute it.
    let subject_opt = if task_subject.is_empty() {
        None
    } else {
        Some(task_subject.as_str())
    };
    let separator_effect = super::effects::build_dm_separator_effect(from, task_id, subject_opt);
    super::effects::execute_effects(vec![separator_effect], state).await;

    info!("Task claim: {} claimed task !{} directly", from, task_id);

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Claimed task !{}", task_id),
        }),
    )
}

/// Result of a successful task prompt delivery.
pub(crate) struct TaskPromptResult {
    pub message: String,
    pub action: &'static str, // "nudged_attached", "nudged", or "resumed"
    pub session_id: String,
}

/// Core task prompt delivery — shared by the RPC handler and Effect executor.
///
/// Returns Ok(TaskPromptResult) on success, Err(error_message) on failure.
/// The `from` field identifies the sender for DM channel observability.
pub(crate) async fn deliver_task_prompt(
    task_id: &str,
    message: &str,
    from: &str,
    model: Option<&str>,
    state: &DaemonState,
) -> Result<TaskPromptResult, String> {
    // Strip #/! prefix from task_id
    let task_id = task_id
        .strip_prefix('#')
        .or_else(|| task_id.strip_prefix('!'))
        .unwrap_or(task_id);
    let tasks = state.task_store.load_all();
    let task = tasks.iter().find(|t| t.id == task_id);
    let Some(task) = task else {
        return Err(format!("Task !{} not found", task_id));
    };

    // Find the session for this task from persistent state.
    let (session_id, coworker_name) = {
        let ps = state.persistent_state.lock().await;
        match ps.session_by_task(task_id) {
            Some(r) => (
                r.session_id.clone(),
                Some(r.name.clone()).filter(|n| !n.is_empty()),
            ),
            None => {
                return Err(format!(
                    "No session found for task !{} — task may not have been dispatched yet",
                    task_id
                ));
            }
        }
    };
    let is_alive = if let Some(ref name) = coworker_name {
        state.session_manager.is_alive(name).await
    } else {
        false
    };

    // Check if the coworker is attached (interactive terminal via `midtown agent attach`).
    // Attached sessions have is_alive=false (headless process paused), but the session
    // is actively used interactively. Route through headed intercom instead of resuming.
    let is_attached = if let Some(ref name) = coworker_name {
        state.attached_coworkers.lock().unwrap().contains_key(name)
    } else {
        false
    };

    if is_attached {
        // Session is attached — deliver via headed intercom
        let name = coworker_name.as_deref().unwrap_or("unknown");
        state.enqueue_headed_nudge(name, message).await;

        info!(
            "Delivered prompt to attached session (coworker {}) for task !{}",
            name, task_id
        );
        Ok(TaskPromptResult {
            message: format!("Prompt delivered to {} (attached, task !{})", name, task_id),
            action: "nudged_attached",
            session_id,
        })
    } else if is_alive {
        // Session is running — deliver prompt via send_message (like nudge)
        let name = coworker_name.as_deref().unwrap_or("unknown");
        match state.session_manager.send_message(name, message).await {
            Ok(()) => {
                // Post to DM channel for observability (skip fork sessions)
                let is_fork = state.fork_bound_threads.lock().unwrap().contains_key(name);
                if !is_fork {
                    let dm_effect = super::effects::Effect::PostToChannel {
                        sender: from.to_string(),
                        message: message.to_string(),
                        channel: Some(format!("dm-{}", name)),
                        auto_output: false,
                        message_type: Some(crate::message::MessageType::Nudge),
                        nudge_type: Some("task_prompt".to_string()),
                        tool_data: None,
                        provider: None,
                        tool_use_id: None,
                        parent_tool_use_id: None,
                    };
                    Box::pin(super::effects::execute_effects(vec![dm_effect], state)).await;
                }

                info!(
                    "Delivered prompt to running session {} (coworker {}) for task !{}",
                    session_id, name, task_id
                );
                Ok(TaskPromptResult {
                    message: format!("Prompt delivered to {} (task !{})", name, task_id),
                    action: "nudged",
                    session_id,
                })
            }
            Err(e) => Err(format!("Failed to deliver prompt to {}: {}", name, e)),
        }
    } else {
        // Session is stopped — resume with the prompt as initial message
        let record = {
            let ps = state.persistent_state.lock().await;
            ps.sessions.get(&session_id).cloned()
        };
        let Some(record) = record else {
            return Err(format!(
                "Session {} for task !{} has no record — cannot resume",
                session_id, task_id
            ));
        };

        // Determine coworker name for resume
        let name = if !record.name.is_empty() {
            record.name.as_str()
        } else if task.agent_name.is_empty() {
            "unknown"
        } else {
            &task.agent_name
        };

        // Build LaunchConfig for resume
        let mut config = crate::launch::LaunchConfig::coworker(
            name.to_string(),
            state.paths.dir_key().to_string(),
            crate::launch::SessionMode::ResumeSession(session_id.clone()),
            Some(message.to_string()),
            Some(task_id.to_string()),
        );

        // Use the session's recorded working directory
        if !record.working_dir.is_empty() {
            config.working_dir = Some(std::path::PathBuf::from(&record.working_dir));
        }

        // Apply model: --model flag overrides, else use task's configured model
        if let Some(m) = model {
            let mut task_model_map = std::collections::HashMap::new();
            task_model_map.insert(task_id.to_string(), m.to_string());
            config.apply_task_model(&task_model_map, task_id);
        } else if let Ok(store_task) = state.task_store.load(task_id)
            && let Some(ref m) = store_task.model
        {
            let mut task_model_map = std::collections::HashMap::new();
            task_model_map.insert(task_id.to_string(), m.clone());
            config.apply_task_model(&task_model_map, task_id);
        }

        // Set channel from task
        config.channel = task.channel.clone();

        // Spawn the resumed session
        match state.spawn_coworker(&config).await {
            Ok(_) => {
                info!(
                    "Resumed session {} (coworker {}) for task !{} with prompt",
                    session_id, name, task_id
                );

                // Post to DM channel for observability (skip fork sessions)
                let is_fork = state.fork_bound_threads.lock().unwrap().contains_key(name);
                if !is_fork {
                    let dm_effect = super::effects::Effect::PostToChannel {
                        sender: from.to_string(),
                        message: format!("[resumed] {}", message),
                        channel: Some(format!("dm-{}", name)),
                        auto_output: false,
                        message_type: Some(crate::message::MessageType::Nudge),
                        nudge_type: Some("task_prompt_resume".to_string()),
                        tool_data: None,
                        provider: None,
                        tool_use_id: None,
                        parent_tool_use_id: None,
                    };
                    Box::pin(super::effects::execute_effects(vec![dm_effect], state)).await;
                }

                Ok(TaskPromptResult {
                    message: format!("Resumed {} with prompt (task !{})", name, task_id),
                    action: "resumed",
                    session_id,
                })
            }
            Err(e) => Err(format!(
                "Failed to resume session for task !{}: {}",
                task_id, e
            )),
        }
    }
}

/// Handle task.handoff RPC — swap the agent type on a task's session.
///
/// Stops the current session (if running), updates the task's agent type
/// in persistent state, then optionally resumes with a message. Claude Code's
/// `--resume <id> --agent <name>` applies the new agent's system prompt while
/// preserving conversation history.
pub(super) async fn handle_task_handoff(
    id: RequestId,
    task_id: &str,
    agent: &str,
    message: Option<&str>,
    from: &str,
    state: &DaemonState,
) -> Response {
    // Strip #/! prefix from task_id
    let task_id = task_id
        .strip_prefix('#')
        .or_else(|| task_id.strip_prefix('!'))
        .unwrap_or(task_id);

    // Validate task exists (use repo-scoped lookup so tests with
    // set_test_midtown_base_dir can find their tasks)
    let _dir_key = state.paths.dir_key();
    let tasks = state.task_store.load_all();
    if !tasks.iter().any(|t| t.id == task_id) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    }

    // Find the session for this task from persistent state
    let (session_id, coworker_name) = {
        let ps = state.persistent_state.lock().await;
        match ps.session_by_task(task_id) {
            Some(r) => (
                r.session_id.clone(),
                Some(r.name.clone()).filter(|n| !n.is_empty()),
            ),
            None => {
                return Response::error(
                    id,
                    RpcError::new(
                        -32603,
                        format!(
                            "No session found for task !{} — task may not have been dispatched yet",
                            task_id
                        ),
                    ),
                );
            }
        }
    };
    if let Some(ref name) = coworker_name
        && state.session_manager.is_alive(name).await
    {
        info!(
            "Stopping session {} (coworker {}) for task !{} handoff to agent {}",
            session_id, name, task_id, agent
        );
        state.broadcast_coworker_update(name, "stopped", None);
        if let Err(e) = state.session_manager.shutdown(name).await {
            warn!("Failed to shut down session for handoff: {}", e);
        }
        state.cleanup_coworker_state(name).await;
    }

    // Update agent_type in TaskStore
    if let Ok(mut store_task) = state.task_store.load(task_id) {
        store_task.agent_type = agent.to_string();
        if let Err(e) = state.task_store.save(&store_task) {
            warn!(
                "Failed to update TaskStore task {} agent_type: {}",
                task_id, e
            );
        } else {
            state.update_task_index(&store_task).await;
        }
    }

    info!(
        "Task !{} agent type changed to {} (session {})",
        task_id, agent, session_id
    );

    // If a message was provided, resume with the new agent and deliver the prompt.
    // deliver_task_prompt will resume the stopped session, and spawn_coworker will
    // pick up the updated task_agent_type to set --agent on the CLI.
    if let Some(msg) = message {
        match deliver_task_prompt(task_id, msg, from, None, state).await {
            Ok(result) => Response::success(
                id,
                serde_json::json!({
                    "type": "message",
                    "message": format!("Task !{} handed off to agent {} and resumed", task_id, agent),
                    "action": "handoff_resumed",
                    "session_id": result.session_id,
                }),
            ),
            Err(e) => {
                // Agent type was updated but resume failed — still report partial success
                warn!(
                    "Task !{} agent type updated to {} but resume failed: {}",
                    task_id, agent, e
                );
                Response::success(
                    id,
                    serde_json::json!({
                        "type": "message",
                        "message": format!(
                            "Task !{} agent type changed to {} but resume failed: {}",
                            task_id, agent, e
                        ),
                        "action": "handoff_no_resume",
                        "session_id": session_id,
                    }),
                )
            }
        }
    } else {
        // No message — just update the agent type, session stays stopped.
        // The next `task prompt` will resume with the new agent.
        Response::success(
            id,
            serde_json::json!({
                "type": "message",
                "message": format!("Task !{} agent type changed to {}", task_id, agent),
                "action": "handoff",
                "session_id": session_id,
            }),
        )
    }
}

/// Handle task.prompt RPC — deliver a prompt to a task's assigned session.
///
/// Looks up the task, finds its session, and either nudges (if running)
/// or resumes (if stopped) with the given message. This is the universal
/// way to interact with a task's agent from any caller (lead, coworker, CLI).
pub(super) async fn handle_task_prompt(
    id: RequestId,
    task_id: &str,
    message: &str,
    from: &str,
    model: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Validate model format if provided
    if let Some(m) = model
        && let Err(e) = validate_model_format(m)
    {
        return Response::error(id, RpcError::new(-32602, e));
    }

    match deliver_task_prompt(task_id, message, from, model, state).await {
        Ok(result) => Response::success(
            id,
            serde_json::json!({
                "type": "message",
                "message": result.message,
                "action": result.action,
                "session_id": result.session_id,
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32603, e)),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_task_tests.rs"]
#[cfg(test)]
mod tests;
