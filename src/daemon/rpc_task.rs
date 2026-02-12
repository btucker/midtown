//! Task-related RPC handlers.
//!
//! Handles `task.create`, `task.update`, `task.done`, `task.metadata`,
//! `task.request`, and `task.claim` methods, plus their supporting helpers
//! (model/channel mapping, clustering, active form generation).

use std::collections::HashMap;

use tracing::{debug, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};

use super::{DaemonState, effects};

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

/// Invoke the clusterer to produce a ClusteringDiff for a new task.
///
/// Builds a ClustererRequest with the task ID, subject, description, and current
/// channel state, then invokes the clusterer headless session. The clusterer
/// returns a full ClusteringDiff describing channel operations (create, archive,
/// merge, assign). The clusterer accumulates context across invocations via
/// session resume.
///
/// Returns the ClusteringDiff or an error.
async fn invoke_clusterer_for_task(
    task_id: &str,
    subject: &str,
    description: &str,
    state: &DaemonState,
) -> Result<crate::clustering::ClusteringDiff, String> {
    use crate::daemon::clusterer::{
        ChannelInfo, ClustererRequest, CompletedTaskInfo, assign_channel,
    };
    use crate::tasks::TaskStatus;

    // Collect channel information: list all channels and their active task counts
    let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);
    // Exclude archived channels from task assignment clustering
    let channel_names = crate::channel::Channel::list(&base_dir, false).unwrap_or_else(|e| {
        warn!("Failed to list channels for clusterer: {}", e);
        vec!["midtown".to_string()]
    });

    // Read all tasks to compute per-channel stats and recent completions
    let all_tasks = crate::tasks::read_tasks_for_repo(Some(&state.repo_name));

    // Build map of task_id -> channel from persistent state
    let task_channel_map = {
        let ps = state.persistent_state.lock().await;
        ps.task_channel.clone()
    };

    // Group tasks by channel and collect stats
    let mut channel_info_map: std::collections::HashMap<String, ChannelInfo> = channel_names
        .iter()
        .map(|name| {
            (
                name.clone(),
                ChannelInfo {
                    name: name.clone(),
                    active_task_count: 0,
                    recent_tasks: vec![],
                },
            )
        })
        .collect();

    // Track recently completed tasks (last 10)
    let mut recent_completions = vec![];

    for task in &all_tasks {
        let task_channel = task
            .channel
            .as_ref()
            .or_else(|| task_channel_map.get(&task.id))
            .map(|s| s.as_str())
            .unwrap_or("midtown");

        match task.status {
            TaskStatus::Completed => {
                // Collect completed tasks for context
                if recent_completions.len() < 10 {
                    recent_completions.push(CompletedTaskInfo {
                        subject: task.subject.clone(),
                        channel: Some(task_channel.to_string()),
                    });
                }
            }
            TaskStatus::InProgress | TaskStatus::Pending => {
                // Count active tasks per channel and track recent subjects
                if let Some(info) = channel_info_map.get_mut(task_channel) {
                    info.active_task_count += 1;
                    if info.recent_tasks.len() < 3 {
                        info.recent_tasks.push(task.subject.clone());
                    }
                }
            }
        }
    }

    let channels: Vec<ChannelInfo> = channel_info_map.into_values().collect();

    let request = ClustererRequest {
        task_id: task_id.to_string(),
        task_subject: subject.to_string(),
        task_description: description.to_string(),
        channels,
        recent_completions,
    };

    // Get working directory (use primary repo path)
    let cwd = state
        .all_repo_paths
        .first()
        .ok_or("No repo paths configured")?
        .clone();

    // Lock persistent state to pass to clusterer
    let mut ps = state.persistent_state.lock().await;

    // Invoke clusterer
    let diff = assign_channel(request, cwd, &mut ps).await?;

    // Save persistent state with updated session ID
    if let Err(e) = ps.save_for_repo(&state.repo_name) {
        warn!("Failed to save clusterer session ID: {}", e);
    }

    Ok(diff)
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
    let channel_message = format!("@lead [Task Request] from {}: \"{}\"", from, message);

    let msg = Message::new("midtown", channel_message.clone(), MessageType::Text);

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
/// Creates the task first with a provisional channel ("midtown" or user-specified),
/// then invokes the clusterer to get channel assignments. The clusterer may create
/// new channels, archive old ones, or reassign tasks. Dispatch for the new task
/// happens on the next `TaskDispatchTick` via the canonical event loop pipeline.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_task_create(
    id: RequestId,
    subject: &str,
    description: &str,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    pr: Option<u64>,
    state: &DaemonState,
) -> Response {
    let repo_name = state.repo_name.clone();

    // Generate active_form (present continuous) from subject for task UI spinner
    let active_form = generate_active_form(subject);

    // Create the task with provisional channel (user-specified or "midtown")
    // We need the task ID before invoking the clusterer
    let provisional_channel = channel.unwrap_or("midtown");

    let task_id = match crate::tasks::create_task_for_repo(
        subject,
        description,
        &active_form,
        "",
        &repo_name,
        blocked_by,
        Some(provisional_channel),
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

    // If no explicit channel was provided, invoke clusterer to get assignments
    if channel.is_none() {
        match invoke_clusterer_for_task(&task_id.to_string(), subject, description, state).await {
            Ok(diff) => {
                // Validate the diff
                if let Err(e) = diff.validate() {
                    warn!(
                        "Clusterer returned invalid diff: {} — keeping provisional channel",
                        e
                    );
                } else {
                    // Apply the clustering diff via effects pipeline
                    info!(
                        "Clusterer returned diff: {} creates, {} archives, {} merges, {} assignments",
                        diff.create_channels.len(),
                        diff.archive_channels.len(),
                        diff.merge_channels.len(),
                        diff.assign_tasks.len()
                    );

                    // Convert clustering diff to effects and execute them
                    let effects = super::clustering::apply_clustering_diff(diff);
                    effects::execute_effects(effects, state).await;
                }
            }
            Err(e) => {
                warn!("Clusterer failed: {} — keeping provisional channel", e);
            }
        }
    } else {
        // User specified a channel explicitly — persist to task_channel mapping
        let mut ps = state.persistent_state.lock().await;
        if apply_task_channel_mapping(&mut ps.task_channel, &task_id, channel, false)
            && let Err(e) = ps.save_for_repo(&repo_name)
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

    // Post to channel so team is aware
    let msg = Message::text("lead", format!("created task: {}", subject));
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post task creation to channel: {}", e);
    }

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

        // Apply channel mapping
        if apply_task_channel_mapping(&mut ps.task_channel, task_id, channel, true) {
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
pub(super) async fn handle_task_metadata(
    id: RequestId,
    task_id: &str,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;
    let channel = ps.task_channel.get(task_id).cloned();
    let model = ps.task_model.get(task_id).cloned();

    Response::success(
        id,
        serde_json::json!({
            "channel": channel,
            "model": model,
        }),
    )
}

/// Handle task.claim RPC — a coworker claims a task by writing directly to disk.
///
/// Validates the task exists and is pending, then sets owner and status to in_progress
/// directly. No Lead proxy needed.
pub(super) fn handle_task_claim(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_task_channel_mapping_sets_channel() {
        let mut map = HashMap::new();
        let changed = apply_task_channel_mapping(&mut map, "42", Some("auth"), false);
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_overwrites_existing() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "old-channel".to_string());
        let changed = apply_task_channel_mapping(&mut map, "42", Some("new-channel"), false);
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"new-channel".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_ignores_none() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        let changed = apply_task_channel_mapping(&mut map, "42", None, false);
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_ignores_empty_without_clear() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        // On create (allow_clear=false), empty string is ignored
        let changed = apply_task_channel_mapping(&mut map, "42", Some(""), false);
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_clears_with_empty_on_update() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        // On update (allow_clear=true), empty string clears the mapping
        let changed = apply_task_channel_mapping(&mut map, "42", Some(""), true);
        assert!(changed);
        assert!(!map.contains_key("42"));
    }

    #[test]
    fn test_apply_task_channel_mapping_clear_nonexistent_is_noop() {
        let mut map = HashMap::new();
        // Clearing a mapping that doesn't exist returns false (no state modification)
        let changed = apply_task_channel_mapping(&mut map, "99", Some(""), true);
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_channel_mapping_none_on_empty_map() {
        let mut map: HashMap<String, String> = HashMap::new();
        let changed = apply_task_channel_mapping(&mut map, "42", None, true);
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_validate_model_format_valid() {
        assert!(validate_model_format("claude/opus").is_ok());
        assert!(validate_model_format("claude/sonnet").is_ok());
        assert!(validate_model_format("claude/haiku").is_ok());
        assert!(validate_model_format("codex/o3").is_ok());
        assert!(validate_model_format("codex/o4-mini").is_ok());
    }

    #[test]
    fn test_validate_model_format_invalid() {
        // Missing slash
        assert!(validate_model_format("claude-opus").is_err());
        // Multiple slashes
        assert!(validate_model_format("claude/opus/extra").is_err());
        // Empty string
        assert!(validate_model_format("").is_err());
        // Only slash
        assert!(validate_model_format("/").is_err());
        // Empty provider
        assert!(validate_model_format("/opus").is_err());
        // Empty model
        assert!(validate_model_format("claude/").is_err());
        // Unsupported provider
        assert!(validate_model_format("unknown/opus").is_err());
        assert!(validate_model_format("openai/gpt4").is_err());
        // Whitespace in model or provider
        assert!(validate_model_format("claude/ opus").is_err());
        assert!(validate_model_format("claude /opus").is_err());
        assert!(validate_model_format(" claude/opus").is_err());
        assert!(validate_model_format("claude/opus ").is_err());
    }

    #[test]
    fn test_apply_task_model_mapping_sets_model() {
        let mut map = HashMap::new();
        let changed = apply_task_model_mapping(&mut map, "42", Some("claude/opus"), false);
        assert!(changed.is_ok());
        assert!(changed.unwrap());
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_rejects_invalid_format() {
        let mut map = HashMap::new();
        let result = apply_task_model_mapping(&mut map, "42", Some("invalid-format"), false);
        assert!(result.is_err());
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_model_mapping_overwrites_existing() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        let changed =
            apply_task_model_mapping(&mut map, "42", Some("claude/sonnet"), false).unwrap();
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"claude/sonnet".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_ignores_none() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        let changed = apply_task_model_mapping(&mut map, "42", None, false).unwrap();
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_ignores_empty_without_clear() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        // On create (allow_clear=false), empty string is ignored
        let changed = apply_task_model_mapping(&mut map, "42", Some(""), false).unwrap();
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_clears_with_empty_on_update() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        // On update (allow_clear=true), empty string clears the mapping
        let changed = apply_task_model_mapping(&mut map, "42", Some(""), true).unwrap();
        assert!(changed);
        assert!(!map.contains_key("42"));
    }

    #[test]
    fn test_apply_task_model_mapping_clear_nonexistent_is_noop() {
        let mut map = HashMap::new();
        // Clearing a mapping that doesn't exist returns false (no state modification)
        let changed = apply_task_model_mapping(&mut map, "99", Some(""), true).unwrap();
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_model_mapping_none_on_empty_map() {
        let mut map: HashMap<String, String> = HashMap::new();
        let changed = apply_task_model_mapping(&mut map, "42", None, true).unwrap();
        assert!(!changed);
        assert!(map.is_empty());
    }
}
