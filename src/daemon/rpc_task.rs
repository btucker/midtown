//! Task-related RPC handlers.
//!
//! Handles `task.create`, `task.update`, `task.done`, `task.metadata`,
//! `task.request`, and `task.claim` methods, plus their supporting helpers
//! (model/channel mapping, active form generation).

use std::collections::HashMap;

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
fn apply_task_model_mapping(
    task_model: &mut HashMap<String, String>,
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
fn apply_task_channel_mapping(
    task_channel: &mut HashMap<String, String>,
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
        state.repo_name, from, message
    );

    let msg = Message::new(&state.repo_name, channel_message.clone(), MessageType::Text);

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
/// Creates the task with the specified channel (or the default "midtown" channel).
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
    execution_skill: Option<&str>,
    thread_id: Option<&str>,
    state: &DaemonState,
) -> Response {
    let repo_name = state.repo_name.clone();

    // Generate active_form (present continuous) from subject for task UI spinner
    let active_form = generate_active_form(subject);

    // Determine the requested channel, then resolve to an effective channel.
    // Archived channels (e.g., "daemon") cannot receive messages and have no
    // active channel lead, so we redirect to ops. The effective channel is stored
    // in both the task JSON and ps.task_channel so all downstream routing
    // (MIDTOWN_CHANNEL injection, insight posting, thread routing) is consistent.
    let requested_channel = channel.unwrap_or(&repo_name);
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

    let task_id = match crate::tasks::create_task_for_repo(
        subject,
        description,
        &active_form,
        "",
        &repo_name,
        blocked_by,
        Some(effective_channel),
        pr,
    ) {
        Ok(id) => id,
        Err(e) => {
            return Response::error(
                id,
                RpcError::new(-32603, format!("Failed to create task: {}", e)),
            );
        }
    };

    // Persist channel mapping using the effective channel so downstream reads
    // (handle_task_metadata, WorldSnapshot.task_channel, MIDTOWN_CHANNEL) all
    // see the routable channel.
    {
        let mut ps = state.persistent_state.lock().await;
        if apply_task_channel_mapping(
            &mut ps.task_channel,
            &task_id,
            Some(effective_channel),
            false,
        ) && let Err(e) = ps.save_for_repo(&repo_name)
        {
            warn!("Failed to save task channel mapping: {}", e);
        }
    }

    // Apply model mapping if provided
    {
        let mut ps = state.persistent_state.lock().await;
        match apply_task_model_mapping(&mut ps.task_model, &task_id, model, false) {
            Ok(changed) => {
                if changed && let Err(e) = ps.save_for_repo(&repo_name) {
                    warn!("Failed to save task model mapping: {}", e);
                }
            }
            Err(e) => {
                // Model format validation failed - return error
                return Response::error(id, RpcError::new(-32602, e));
            }
        }
    }

    // Apply plan, execution_skill, and thread_id mappings if provided (stored in daemon state,
    // NOT in task JSON, to keep task JSON compatible with Claude Code's native format)
    {
        let mut ps = state.persistent_state.lock().await;
        let mut changed = false;
        if let Some(plan_path) = plan {
            ps.task_plan.insert(task_id.clone(), plan_path.to_string());
            changed = true;
        }
        if let Some(skill) = execution_skill {
            ps.task_execution_skill
                .insert(task_id.clone(), skill.to_string());
            changed = true;
        }
        if let Some(tid) = thread_id {
            ps.task_thread_id.insert(task_id.clone(), tid.to_string());
            changed = true;
        }
        if changed && let Err(e) = ps.save_for_repo(&repo_name) {
            warn!(
                "Failed to save task plan/execution_skill/thread_id mapping: {}",
                e
            );
        }
    }

    // Post to the effective channel so the right team sees it, attributed to the
    // channel lead. Capture message ID for task-as-thread feature.
    // Only store the mapping if the write succeeds — a failed write means no channel
    // message exists, so storing the ID would create an orphan thread root.
    let author = task_created_message_author(effective_channel, state.default_channel_name());
    let msg = task_announcement_message(effective_channel, &author, subject, thread_id);
    let message_id = msg.id.clone();
    match state.send_and_broadcast_async(&msg).await {
        Ok(()) => {
            let mut ps = state.persistent_state.lock().await;
            ps.task_message_id
                .insert(task_id.clone(), message_id.clone());
            // Default task_thread_id to the announcement message ID when no
            // explicit --thread-id was provided. This ensures SpawnSession
            // picks up a bound_thread_id so coworker messages auto-route to
            // the task announcement thread.
            if !ps.task_thread_id.contains_key(&task_id) {
                ps.task_thread_id.insert(task_id.clone(), message_id);
            }
            if let Err(e) = ps.save_for_repo(&repo_name) {
                warn!("Failed to save task message_id mapping: {}", e);
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
    let workflow_effect = crate::daemon::effects::Effect::EmitWorkflowEvent(
        crate::workflow::WorkflowEvent::TaskCreated {
            channel: effective_channel.to_string(),
            task_id: task_id.clone(),
            subject: subject.to_string(),
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
    owner: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    pr: Option<u64>,
    state: &DaemonState,
) -> Response {
    // Validate status if provided
    if let Some(s) = status
        && !["pending", "in_progress", "completed"].contains(&s)
    {
        return Response::error(id, RpcError::new(-32602, format!("Invalid status: {}", s)));
    }

    let repo_name = state.repo_name.clone();

    if let Err(e) = crate::tasks::update_task_fields_for_repo(
        task_id,
        &repo_name,
        owner,
        status,
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

    // Update in-memory assignment tracking
    if let Some(new_owner) = owner {
        // Clear old assignment before recording new one (prevents stale entries
        // when a task is reassigned from coworker A to coworker B)
        state.clear_task_assignment_by_task(task_id);
        state.record_task_assignment(new_owner, task_id);
    }

    // Clear assignment when task is completed or reset to pending
    if matches!(status, Some("completed") | Some("pending")) {
        state.clear_task_assignment_by_task(task_id);
    }

    // Update daemon-side task-to-channel and task-to-model mappings
    {
        let mut ps = state.persistent_state.lock().await;
        let mut needs_save = false;

        // Apply channel mapping — when the channel changes, clear the stale
        // task_thread_id since it points to a message in the old channel.
        if apply_task_channel_mapping(&mut ps.task_channel, task_id, channel, true) {
            ps.task_thread_id.remove(task_id);
            needs_save = true;
        }

        // Apply model mapping
        match apply_task_model_mapping(&mut ps.task_model, task_id, model, true) {
            Ok(changed) => {
                if changed {
                    needs_save = true;
                }
            }
            Err(e) => {
                // Model format validation failed - return error
                return Response::error(id, RpcError::new(-32602, e));
            }
        }

        // Save if any mapping changed
        if needs_save && let Err(e) = ps.save_for_repo(&repo_name) {
            warn!("Failed to save task mappings: {}", e);
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
    let repo_name = state.repo_name.clone();

    if let Err(e) = crate::tasks::complete_task_for_repo(task_id, &repo_name) {
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
            if let Err(e) = ps.save_for_repo(&repo_name) {
                warn!("Failed to save worktree completion timestamp: {}", e);
            }
        }
    }

    // Clear in-memory tracking
    state.clear_task_assignment_by_task(task_id);

    // Unblock dependent tasks
    if let Err(e) = crate::tasks::clear_blocked_by_for_repo(task_id, &repo_name) {
        warn!("Failed to clear blockedBy for task !{}: {}", task_id, e);
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
    // Verify the task exists in native task storage before returning metadata.
    let tasks = crate::tasks::read_tasks();
    if !tasks.iter().any(|t| t.id == task_id) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    }

    let ps = state.persistent_state.lock().await;
    let channel = ps.task_channel.get(task_id).cloned();
    let model = ps.task_model.get(task_id).cloned();
    let plan = ps.task_plan.get(task_id).cloned();
    let execution_skill = ps.task_execution_skill.get(task_id).cloned();
    let message_id = ps.task_message_id.get(task_id).cloned();
    let thread_id = ps.task_thread_id.get(task_id).cloned();

    Response::success(
        id,
        serde_json::json!({
            "channel": channel,
            "model": model,
            "plan": plan,
            "execution_skill": execution_skill,
            "message_id": message_id,
            "thread_id": thread_id,
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
    let tasks = crate::tasks::read_tasks();
    let task = tasks.iter().find(|t| t.id == task_id);

    let Some(task) = task else {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    };

    if task.status != crate::tasks::TaskStatus::Pending {
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
    let repo_name = state.repo_name.clone();

    // Write owner and status directly to disk (with retry on transient failures).
    // Disk write happens BEFORE in-memory recording so that a failure leaves
    // no stale in-memory state. Without reconcile_stale_claims, consistency
    // depends on this ordering.
    let mut last_err = None;
    for attempt in 0..3 {
        match crate::tasks::update_task_fields_for_repo(
            task_id,
            &repo_name,
            Some(from),
            Some("in_progress"),
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
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    if let Some(e) = last_err {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to claim task after retries: {}", e)),
        );
    }

    // Record in-memory assignment for busy tracking (only after disk write succeeds)
    state.record_task_assignment(from, task_id);

    // Post task divider to the coworker's DM channel.
    // Reuse build_dm_separator_effect (already tested in effects_tests.rs) to
    // produce the PostSystemMessage effect, then execute it.
    let subject_opt = if task_subject.is_empty() {
        None
    } else {
        Some(task_subject.as_str())
    };
    if let Some(separator_effect) =
        super::effects::build_dm_separator_effect(from, task_id, subject_opt, false)
    {
        super::effects::execute_effects(vec![separator_effect], state).await;
    }

    info!("Task claim: {} claimed task !{} directly", from, task_id);

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Claimed task !{}", task_id),
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_task_tests.rs"]
#[cfg(test)]
mod tests;
