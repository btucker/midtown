//! Claude Code task storage integration.
//!
//! Claude Code stores tasks in `~/.claude/tasks/<session_id>/` as JSON files.
//! This module provides utilities to read and query these tasks without
//! requiring external CLI tools.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A task from Claude Code's task storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "blockedBy")]
    pub blocked_by: Vec<String>,
    /// File creation time, populated from filesystem metadata (not serialized).
    #[serde(skip)]
    pub created_at: Option<std::time::SystemTime>,
}

/// Task status matching Claude Code's TaskList tool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Read all tasks from Claude Code's task storage for the current session.
///
/// Uses `CLAUDE_CODE_TASK_LIST_ID=midtown-<repo>` to locate the shared task storage
/// at `~/.claude/tasks/midtown-<repo>/`.
pub fn read_tasks() -> Vec<Task> {
    read_tasks_for_repo(None)
}

/// Read all tasks for a specific repository.
///
/// If `repo_name` is None, attempts to detect the current repository.
/// Tasks are stored in `~/.claude/tasks/midtown-<repo>/`.
pub fn read_tasks_for_repo(repo_name: Option<&str>) -> Vec<Task> {
    let repo = repo_name
        .map(String::from)
        .or_else(crate::paths::detect_repo_name)
        .unwrap_or_else(|| "default".to_string());

    // Use the shared task list ID (midtown-<repo>)
    let task_list_id = crate::paths::task_list_id_for_repo(&repo);
    read_tasks_for_session(&task_list_id)
}

/// Read all tasks for a specific Claude Code session ID.
pub fn read_tasks_for_session(session_id: &str) -> Vec<Task> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let tasks_dir = home.join(".claude").join("tasks").join(session_id);
    read_tasks_from_dir(&tasks_dir)
}

/// Read all tasks from a directory containing task JSON files.
fn read_tasks_from_dir(tasks_dir: &PathBuf) -> Vec<Task> {
    let Ok(entries) = std::fs::read_dir(tasks_dir) else {
        return Vec::new();
    };

    let mut tasks = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut task) = parse_task_json(&content)
        {
            // Populate created_at from file metadata
            if let Ok(metadata) = path.metadata() {
                task.created_at = metadata.created().ok();
            }
            tasks.push(task);
        }
    }

    // Sort by ID for consistent ordering
    tasks.sort_by(|a, b| {
        let a_num: i32 = a.id.parse().unwrap_or(i32::MAX);
        let b_num: i32 = b.id.parse().unwrap_or(i32::MAX);
        a_num.cmp(&b_num)
    });

    tasks
}

/// Parse a task from JSON, handling various ID formats.
fn parse_task_json(content: &str) -> Result<Task, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(content)?;

    // Handle ID as either string or number
    let id = value
        .get("id")
        .and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_default();

    let subject = value
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let status_str = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");

    let status = match status_str {
        "in_progress" => TaskStatus::InProgress,
        "completed" => TaskStatus::Completed,
        _ => TaskStatus::Pending,
    };

    let owner = value
        .get("owner")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(String::from);

    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);

    let blocked_by = value
        .get("blockedBy")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Task {
        id,
        subject,
        status,
        owner,
        description,
        blocked_by,
        created_at: None,
    })
}

// Convenience query functions

/// Get names of coworkers who have in_progress tasks.
pub fn get_busy_coworkers() -> Vec<String> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .filter_map(|t| t.owner)
        .collect()
}

/// Get names of coworkers who have in_progress tasks for a specific repo.
///
/// This is the preferred version for daemon usage where the repo name is already known,
/// avoiding the need to detect it via git commands which may fail in background processes.
pub fn get_busy_coworkers_for_repo(repo_name: &str) -> Vec<String> {
    read_tasks_for_repo(Some(repo_name))
        .into_iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .filter_map(|t| t.owner)
        .collect()
}

/// Get in_progress tasks with their owners.
///
/// Returns tuples of (task_id, owner_name).
pub fn get_in_progress_tasks() -> Vec<(String, String)> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .map(|t| (t.id, t.owner.unwrap_or_default()))
        .collect()
}

/// Get in_progress tasks with full details.
///
/// Returns tuples of (task_id, subject, owner_name).
pub fn get_in_progress_tasks_with_subjects() -> Vec<(String, String, String)> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .map(|t| (t.id, t.subject, t.owner.unwrap_or_default()))
        .collect()
}

/// Get pending tasks (ready to work on).
pub fn get_pending_tasks() -> Vec<Task> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .collect()
}

/// Extract a PR number from a text string.
///
/// Looks for patterns like "PR #123" in the text.
/// Returns the PR number as a string if found.
pub fn extract_pr_number(text: &str) -> Option<String> {
    // Find "PR #" (case-insensitive) followed by digits
    let lower = text.to_lowercase();
    let idx = lower.find("pr #")?;
    let after = &text[idx + 4..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Extract a PR number from a task by checking both subject and description.
///
/// Returns the PR number as a string if found in either field.
pub fn extract_pr_number_from_task(task: &Task) -> Option<String> {
    extract_pr_number(&task.subject)
        .or_else(|| task.description.as_deref().and_then(extract_pr_number))
}

/// Find the owner of an existing task (in_progress or pending-with-owner) that references the same PR.
///
/// Checks both subject and description for the PR number pattern.
/// Used to group PR sub-tasks under the same coworker.
pub fn find_pr_owner(pr_number: &str) -> Option<String> {
    let tasks = read_tasks();
    find_pr_owner_in_tasks(pr_number, &tasks)
}

/// Find the owner of a task referencing the given PR number within a provided task list.
///
/// This avoids re-reading tasks from disk when the caller already has them.
pub fn find_pr_owner_in_tasks(pr_number: &str, tasks: &[Task]) -> Option<String> {
    let pr_pattern = format!("PR #{}", pr_number);
    for task in tasks {
        if (task.status == TaskStatus::InProgress || task.status == TaskStatus::Pending)
            && task.owner.is_some()
        {
            // Check subject
            if task.subject.contains(&pr_pattern) {
                return task.owner.clone();
            }
            // Check description
            if task
                .description
                .as_ref()
                .is_some_and(|desc| desc.contains(&pr_pattern))
            {
                return task.owner.clone();
            }
        }
    }
    None
}

/// Find the owner of a related task via blockedBy relationships.
///
/// If this task is blocked by another task that has an owner, return that owner.
/// This groups sub-tasks under the same coworker even when they don't mention the PR number.
pub fn find_owner_via_blocked_by(task: &Task, all_tasks: &[Task]) -> Option<String> {
    for blocked_by_id in &task.blocked_by {
        if let Some(parent) = all_tasks.iter().find(|t| &t.id == blocked_by_id)
            && let Some(ref owner) = parent.owner
            && !owner.is_empty()
        {
            return Some(owner.clone());
        }
    }
    None
}

/// Get pending tasks that have an owner assigned.
///
/// Returns tuples of (task_id, subject, owner_name).
pub fn get_pending_tasks_with_owners() -> Vec<(String, String, String)> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending && t.owner.is_some())
        .map(|t| (t.id, t.subject, t.owner.unwrap_or_default()))
        .collect()
}

/// Get pending tasks that have no owner (unclaimed and not started).
///
/// Applies a grace period to skip recently-created tasks, giving the creating
/// coworker time to claim ownership via `midtown task claim` before the daemon assigns them.
pub fn get_pending_tasks_without_owners() -> Vec<Task> {
    get_pending_tasks_without_owners_with_grace(TASK_CREATION_GRACE_SECS)
}

/// Grace period in seconds for newly created tasks.
///
/// Tasks created within this window are skipped by `get_pending_tasks_without_owners`
/// to prevent the daemon from claiming tasks before the creating coworker sets ownership.
const TASK_CREATION_GRACE_SECS: u64 = 45;

/// Get pending tasks without owners, skipping tasks created within `grace_secs` seconds
/// and tasks that have unresolved `blockedBy` dependencies.
pub fn get_pending_tasks_without_owners_with_grace(grace_secs: u64) -> Vec<Task> {
    let now = std::time::SystemTime::now();
    let grace = std::time::Duration::from_secs(grace_secs);
    let all_tasks = read_tasks();
    all_tasks
        .iter()
        .filter(|t| {
            t.status == TaskStatus::Pending
                && t.owner.is_none()
                && !is_within_grace_period(t, now, grace)
                && !has_unresolved_blockers(t, &all_tasks)
        })
        .cloned()
        .collect()
}

/// Get coworkers that have newly-unblocked dependent tasks.
///
/// A coworker has unblocked dependents when:
/// 1. They own a completed task
/// 2. A pending task's `blockedBy` includes that completed task
/// 3. The pending task is now fully unblocked (all its blockers are completed)
/// 4. The pending task has no owner yet (hasn't been assigned)
///
/// This is used to protect coworkers from idle shutdown when their follow-up
/// tasks are about to become assignable.
pub fn get_coworkers_with_unblocked_dependents() -> std::collections::HashSet<String> {
    let all_tasks = read_tasks();
    let mut result = std::collections::HashSet::new();

    // Find pending, unowned tasks that are fully unblocked
    let unblocked_pending: Vec<&Task> = all_tasks
        .iter()
        .filter(|t| {
            t.status == TaskStatus::Pending
                && t.owner.is_none()
                && !t.blocked_by.is_empty()
                && !has_unresolved_blockers(t, &all_tasks)
        })
        .collect();

    // For each unblocked pending task, find the owner of its blocking tasks
    for task in &unblocked_pending {
        for blocker_id in &task.blocked_by {
            if let Some(blocker) = all_tasks.iter().find(|t| t.id == *blocker_id)
                && blocker.status == TaskStatus::Completed
                && let Some(ref owner) = blocker.owner
                && !owner.is_empty()
            {
                result.insert(owner.to_lowercase());
            }
        }
    }

    result
}

/// Check whether a task has unresolved `blockedBy` dependencies.
///
/// Returns `true` if any task ID in `blocked_by` refers to a task that is not
/// `Completed`, or if the referenced task does not exist (treated as unresolved
/// to avoid assigning work whose prerequisites can't be verified).
pub fn has_unresolved_blockers(task: &Task, all_tasks: &[Task]) -> bool {
    if task.blocked_by.is_empty() {
        return false;
    }
    task.blocked_by.iter().any(|blocker_id| {
        match all_tasks.iter().find(|t| &t.id == blocker_id) {
            Some(blocker) => blocker.status != TaskStatus::Completed,
            None => true, // unknown task — treat as unresolved
        }
    })
}

/// Check if a task was created within the grace period.
///
/// Returns true if the task's `created_at` is within `grace` of `now`.
/// Returns false if `created_at` is unavailable (assumes task is old enough).
fn is_within_grace_period(
    task: &Task,
    now: std::time::SystemTime,
    grace: std::time::Duration,
) -> bool {
    match task.created_at {
        Some(created) => now
            .duration_since(created)
            .map(|age| age < grace)
            .unwrap_or(false),
        None => false,
    }
}

/// Update a task's owner, keeping status as "pending".
///
/// Writes the updated task back to disk. Called by the daemon when a coworker
/// claims a task through `midtown task claim`.
/// The task remains "pending" with an owner set — this "pending with owner"
/// state is used by dispatch.rs for spawn decisions and snapshot.rs for idle
/// shutdown protection. The status transitions to "in_progress" separately.
pub fn update_task_owner(task_id: &str, owner: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    // Use the shared task list ID (midtown-<repo>)
    let task_list_id = crate::paths::task_list_id();

    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);

    update_task_owner_in_dir(task_id, owner, &tasks_dir)
}

/// Update a task's owner in a specific directory.
///
/// This only sets the owner field, leaving status as "pending". The "pending with
/// owner" state is load-bearing: dispatch.rs uses it for spawn decisions and
/// snapshot.rs uses it for idle shutdown protection. The transition to "in_progress"
/// happens separately (the daemon sets both owner and in_progress on claim).
///
/// This is the path-injectable version used by tests and by `update_task_owner`.
fn update_task_owner_in_dir(
    task_id: &str,
    owner: &str,
    tasks_dir: &std::path::Path,
) -> Result<(), String> {
    let task_file = tasks_dir.join(format!("{}.json", task_id));

    let content =
        std::fs::read_to_string(&task_file).map_err(|e| format!("Failed to read task: {}", e))?;

    // Parse and update
    let mut task: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse task: {}", e))?;

    task["owner"] = serde_json::json!(owner);

    // Write back
    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task: {}", e))?;

    Ok(())
}

/// Update specific fields on a task.
///
/// Only fields that are `Some` are updated; `None` fields are left unchanged.
/// This is the daemon's direct write path — no Lead proxy needed.
pub fn update_task_fields_for_repo(
    task_id: &str,
    repo_name: &str,
    owner: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    blocked_by: Option<&[String]>,
) -> Result<(), String> {
    use fs2::FileExt;

    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    let task_file = tasks_dir.join(format!("{}.json", task_id));

    // Open for read+write and lock exclusively to prevent TOCTTOU races
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&task_file)
        .map_err(|e| format!("Failed to open task {}: {}", task_id, e))?;
    file.lock_exclusive()
        .map_err(|e| format!("Failed to lock task {}: {}", task_id, e))?;

    let content = std::fs::read_to_string(&task_file)
        .map_err(|e| format!("Failed to read task {}: {}", task_id, e))?;

    let mut task: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse task {}: {}", task_id, e))?;

    if let Some(o) = owner {
        task["owner"] = serde_json::json!(o);
    }
    if let Some(s) = status {
        task["status"] = serde_json::json!(s);
    }
    if let Some(d) = description {
        task["description"] = serde_json::json!(d);
    }
    if let Some(bb) = blocked_by {
        task["blockedBy"] = serde_json::json!(bb);
    }

    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task: {}", e))?;

    let _ = file.unlock();
    Ok(())
}

/// Reset a task to pending status and clear its owner.
///
/// Used when orphan recovery fails to respawn a coworker - the task is reset
/// so another coworker can claim it instead of being stuck forever.
pub fn reset_task_to_pending(task_id: &str) -> Result<(), String> {
    let repo = crate::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
    reset_task_to_pending_for_repo(task_id, &repo)
}

/// Reset a task to pending status and clear its owner for a specific repo.
///
/// This is the preferred version for daemon usage where the repo name is already known.
pub fn reset_task_to_pending_for_repo(task_id: &str, repo_name: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    // Use the shared task list ID (midtown-<repo>) — same directory that
    // update_task_owner() and Claude Code sessions use via CLAUDE_CODE_TASK_LIST_ID
    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);

    // Read the task file
    let task_file = home
        .join(".claude")
        .join("tasks")
        .join(&task_list_id)
        .join(format!("{}.json", task_id));

    let content =
        std::fs::read_to_string(&task_file).map_err(|e| format!("Failed to read task: {}", e))?;

    // Parse and update
    let mut task: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse task: {}", e))?;

    // Reset to pending and clear owner
    task["status"] = serde_json::json!("pending");
    task["owner"] = serde_json::Value::Null;

    // Write back
    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task: {}", e))?;

    Ok(())
}

/// Extract task ID from PR title using the `[Midtown #XX]` format.
pub fn extract_task_id_from_pr_title(title: &str) -> Option<u64> {
    if let Some(start) = title.find("[Midtown #") {
        let rest = &title[start + 10..]; // Skip "[Midtown #"
        if let Some(end) = rest.find(']') {
            let num_str = &rest[..end];
            return num_str.parse::<u64>().ok();
        }
    }
    None
}

/// Mark a task as completed in a specific tasks directory.
///
/// This is the path-injectable version used by tests.
fn complete_task_in_dir(task_id: &str, tasks_dir: &std::path::Path) -> Result<(), String> {
    let task_file = tasks_dir.join(format!("{}.json", task_id));

    let content =
        std::fs::read_to_string(&task_file).map_err(|e| format!("Failed to read task: {}", e))?;

    let mut task: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse task: {}", e))?;

    // Mark as completed
    task["status"] = serde_json::json!("completed");

    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task: {}", e))?;

    Ok(())
}

/// Mark a task as completed for a specific repo.
///
/// This is called when a PR is opened with `[Midtown #XX]` in the title.
/// Opening a PR means the implementation work is done; the task is complete.
pub fn complete_task_for_repo(task_id: &str, repo_name: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);

    complete_task_in_dir(task_id, &tasks_dir)
}

/// Set a task's status to in_progress in a specific tasks directory.
///
/// This is the path-injectable version used by tests.
fn set_task_in_progress_in_dir(task_id: &str, tasks_dir: &std::path::Path) -> Result<(), String> {
    let task_file = tasks_dir.join(format!("{}.json", task_id));

    let content =
        std::fs::read_to_string(&task_file).map_err(|e| format!("Failed to read task: {}", e))?;

    let mut task: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse task: {}", e))?;

    task["status"] = serde_json::json!("in_progress");

    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task: {}", e))?;

    Ok(())
}

/// Set a task's status to in_progress for a specific repo.
///
/// Called after a coworker successfully spawns to reflect that work has started.
pub fn set_task_in_progress_for_repo(task_id: &str, repo_name: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);

    set_task_in_progress_in_dir(task_id, &tasks_dir)
}

/// Clear a completed task ID from all dependent tasks' `blockedBy` arrays in a specific directory.
///
/// This is the path-injectable version used by tests.
fn clear_blocked_by_in_dir(
    completed_task_id: &str,
    tasks_dir: &std::path::Path,
) -> Result<(), String> {
    let entries = std::fs::read_dir(tasks_dir)
        .map_err(|e| format!("Failed to read tasks directory: {}", e))?;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut task: serde_json::Value = match serde_json::from_str(&content) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Check if this task has the completed task in its blockedBy
        if let Some(blocked_by) = task.get_mut("blockedBy")
            && let Some(arr) = blocked_by.as_array_mut()
        {
            let original_len = arr.len();
            arr.retain(|v| {
                let id = v
                    .as_str()
                    .map(String::from)
                    .or_else(|| v.as_u64().map(|n| n.to_string()));
                id.as_deref() != Some(completed_task_id)
            });

            // If we removed anything, write the file back
            if arr.len() < original_len {
                let updated_content = match serde_json::to_string_pretty(&task) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let _ = std::fs::write(&path, updated_content);
            }
        }
    }

    Ok(())
}

/// Clear a completed task ID from all dependent tasks' `blockedBy` arrays.
///
/// When a task is completed, any tasks that were blocked by it should have
/// that ID removed from their `blockedBy` list. This allows dependent tasks
/// to become unblocked and be assigned.
pub fn clear_blocked_by_for_repo(completed_task_id: &str, repo_name: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);

    clear_blocked_by_in_dir(completed_task_id, &tasks_dir)
}

/// Ensure a task with the given subject exists in the shared task directory.
///
/// Used by the PostToolUse hook to mirror Lead-created tasks that Claude Code
/// failed to persist (e.g., after `/resume`). Deduplicates by subject against
/// non-completed tasks (not subject+owner) since Lead tasks are created without
/// an owner. Completed tasks with the same subject are ignored, allowing new
/// tasks for fresh review cycles.
///
/// Uses a directory-level lock file to prevent TOCTOU races when multiple
/// processes create tasks concurrently.
///
/// Returns `(shared_id, was_created)` — the ID of the existing or newly created
/// task, and whether a new file was written.
pub fn ensure_task_in_shared_dir(
    tasks_dir: &std::path::Path,
    subject: &str,
    description: &str,
) -> Result<(String, bool), String> {
    use fs2::FileExt;

    let tasks_dir_buf = tasks_dir.to_path_buf();

    // Ensure directory exists
    if !tasks_dir.exists() {
        std::fs::create_dir_all(tasks_dir)
            .map_err(|e| format!("Failed to create tasks directory: {}", e))?;
    }

    // Acquire directory-level lock to prevent concurrent ID assignment races
    let lock_path = tasks_dir.join(".tasks.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open lock file: {}", e))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| format!("Failed to acquire task dir lock: {}", e))?;

    let existing = read_tasks_from_dir(&tasks_dir_buf);

    // Check for existing non-completed task with same subject
    for task in &existing {
        if task.subject == subject && task.status != TaskStatus::Completed {
            let _ = lock_file.unlock();
            return Ok((task.id.clone(), false));
        }
    }

    // Create new task with next sequential ID
    let next_id = existing
        .iter()
        .filter_map(|t| t.id.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;

    let task_id = next_id.to_string();
    let task = serde_json::json!({
        "id": task_id,
        "subject": subject,
        "description": description,
        "status": "pending",
        "blockedBy": [],
        "blocks": [],
    });

    let path = tasks_dir.join(format!("{}.json", task_id));
    let content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;
    std::fs::write(&path, content).map_err(|e| format!("Failed to write task file: {}", e))?;

    let _ = lock_file.unlock();
    Ok((task_id, true))
}

/// Update specific fields of a task file in the given directory.
///
/// Used by the PostToolUse hook to mirror TaskUpdate operations to the shared
/// directory when Claude Code's internal task IDs don't match the shared IDs
/// (e.g., after `/resume`).
///
/// Uses file-level locking to prevent lost updates when concurrent processes
/// (daemon, hooks) modify the same task file simultaneously.
///
/// Supports updating: status, owner, subject, description.
pub fn update_task_fields_in_dir(
    tasks_dir: &std::path::Path,
    task_id: &str,
    updates: &serde_json::Value,
) -> Result<(), String> {
    use fs2::FileExt;

    let task_file = tasks_dir.join(format!("{}.json", task_id));

    // Open the file for read+write and lock exclusively
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&task_file)
        .map_err(|e| format!("Failed to open task {}: {}", task_id, e))?;
    file.lock_exclusive()
        .map_err(|e| format!("Failed to lock task {}: {}", task_id, e))?;

    let content = std::fs::read_to_string(&task_file)
        .map_err(|e| format!("Failed to read task {}: {}", task_id, e))?;

    let mut task: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse task {}: {}", task_id, e))?;

    // Apply updates for known fields
    if let Some(status) = updates.get("status").and_then(|v| v.as_str()) {
        task["status"] = serde_json::json!(status);
    }
    if let Some(owner) = updates.get("owner").and_then(|v| v.as_str()) {
        task["owner"] = serde_json::json!(owner);
    }
    if let Some(subject) = updates.get("subject").and_then(|v| v.as_str()) {
        task["subject"] = serde_json::json!(subject);
    }
    if let Some(description) = updates.get("description").and_then(|v| v.as_str()) {
        task["description"] = serde_json::json!(description);
    }

    let updated_content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    std::fs::write(&task_file, updated_content)
        .map_err(|e| format!("Failed to write task file: {}", e))?;

    let _ = file.unlock();
    Ok(())
}

/// Read the Lead task ID mapping file for a given repo.
///
/// The mapping file bridges Claude Code's internal task IDs (which may start from 1
/// after `/resume`) to the shared sequential IDs in `~/.claude/tasks/midtown-<repo>/`.
///
/// Format: `{"1": "805", "2": "806", ...}`
pub fn read_lead_task_id_map(repo: &str) -> std::collections::HashMap<String, String> {
    let path = crate::paths::projects_dir_for_repo(repo).join("lead-task-id-map.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Store a mapping from an internal task ID to a shared task ID.
pub fn store_lead_task_id_mapping(repo: &str, internal_id: &str, shared_id: &str) {
    let dir = crate::paths::projects_dir_for_repo(repo);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("lead-task-id-map.json");
    let mut map = read_lead_task_id_map(repo);
    map.insert(internal_id.to_string(), shared_id.to_string());
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&map).unwrap_or_default(),
    );
}

/// Look up the shared task ID for an internal task ID.
pub fn lookup_lead_task_id(repo: &str, internal_id: &str) -> Option<String> {
    read_lead_task_id_map(repo).get(internal_id).cloned()
}

/// Clear the Lead task ID mapping file.
///
/// Called when a fresh Lead session starts, since fresh sessions use the shared
/// directory directly and don't need remapping.
pub fn clear_lead_task_id_map(repo: &str) {
    let path = crate::paths::projects_dir_for_repo(repo).join("lead-task-id-map.json");
    let _ = std::fs::remove_file(&path);
}

/// Get the shared tasks directory for a repo.
///
/// Returns `~/.claude/tasks/midtown-<repo>/`.
pub fn shared_tasks_dir_for_repo(repo: &str) -> std::path::PathBuf {
    let task_list_id = crate::paths::task_list_id_for_repo(repo);
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("tasks")
        .join(&task_list_id)
}

/// Create a new task in the shared task storage for a specific repo.
///
/// Assigns the next sequential ID by scanning existing task files.
/// Returns the assigned task ID on success.
pub fn create_task_for_repo(
    subject: &str,
    description: &str,
    active_form: &str,
    owner: &str,
    repo_name: &str,
) -> Result<String, String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);

    // Ensure directory exists
    if !tasks_dir.exists() {
        std::fs::create_dir_all(&tasks_dir)
            .map_err(|e| format!("Failed to create tasks directory: {}", e))?;
    }

    create_task_in_dir(&tasks_dir, subject, description, active_form, owner)
}

/// Inner implementation: create a task in the given directory.
///
/// Reads existing tasks once to both determine the next ID and check for duplicates.
/// A task with the same subject and owner (regardless of status) is considered a duplicate.
/// Uses directory-level locking to prevent concurrent ID collisions.
fn create_task_in_dir(
    tasks_dir: &std::path::Path,
    subject: &str,
    description: &str,
    active_form: &str,
    owner: &str,
) -> Result<String, String> {
    use fs2::FileExt;

    let tasks_dir_buf = tasks_dir.to_path_buf();

    // Acquire directory-level lock to prevent concurrent ID assignment races
    let lock_path = tasks_dir.join(".tasks.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open lock file: {}", e))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| format!("Failed to acquire task dir lock: {}", e))?;

    let existing = read_tasks_from_dir(&tasks_dir_buf);

    // Check for duplicate: same subject + owner in any status
    for task in &existing {
        if task.subject == subject && task.owner.as_deref() == Some(owner) {
            let _ = lock_file.unlock();
            return Ok(task.id.clone());
        }
    }

    // Determine next ID from existing tasks
    let next_id = existing
        .iter()
        .filter_map(|t| t.id.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;

    let task_id = next_id.to_string();
    let task = serde_json::json!({
        "id": task_id,
        "subject": subject,
        "description": description,
        "status": "pending",
        "owner": owner,
        "blockedBy": [],
        "blocks": [],
        "activeForm": active_form,
    });

    let path = tasks_dir.join(format!("{}.json", task_id));
    let content = serde_json::to_string_pretty(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;
    std::fs::write(&path, content).map_err(|e| format!("Failed to write task file: {}", e))?;

    let _ = lock_file.unlock();
    Ok(task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_task_file(dir: &std::path::Path, id: &str, status: &str, owner: Option<&str>) {
        let task = serde_json::json!({
            "id": id,
            "subject": format!("Task {}", id),
            "status": status,
            "owner": owner,
        });
        let path = dir.join(format!("{}.json", id));
        let mut file = std::fs::File::create(path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&task).unwrap()).unwrap();
    }

    #[test]
    fn test_read_tasks_from_dir() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        create_task_file(&tasks_dir, "1", "pending", None);
        create_task_file(&tasks_dir, "2", "in_progress", Some("alice"));
        create_task_file(&tasks_dir, "3", "completed", Some("bob"));

        let tasks = read_tasks_from_dir(&tasks_dir);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
        assert!(tasks[0].owner.is_none());
        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[1].status, TaskStatus::InProgress);
        assert_eq!(tasks[1].owner, Some("alice".to_string()));
    }

    #[test]
    fn test_parse_numeric_id() {
        let json = r#"{"id": 42, "subject": "Test", "status": "pending"}"#;
        let task = parse_task_json(json).unwrap();
        assert_eq!(task.id, "42");
    }

    #[test]
    fn test_pending_tasks_with_owners() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        create_task_file(&tasks_dir, "1", "pending", None);
        create_task_file(&tasks_dir, "2", "pending", Some("alice"));
        create_task_file(&tasks_dir, "3", "pending", Some("bob"));
        create_task_file(&tasks_dir, "4", "in_progress", Some("carol"));

        let tasks = read_tasks_from_dir(&tasks_dir);
        let pending_with_owners: Vec<_> = tasks
            .into_iter()
            .filter(|t| t.status == TaskStatus::Pending && t.owner.is_some())
            .map(|t| (t.id, t.subject, t.owner.unwrap_or_default()))
            .collect();

        assert_eq!(pending_with_owners.len(), 2);
        assert_eq!(pending_with_owners[0].2, "alice");
        assert_eq!(pending_with_owners[1].2, "bob");
    }

    #[test]
    fn test_pending_tasks_without_owners() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        create_task_file(&tasks_dir, "1", "pending", None);
        create_task_file(&tasks_dir, "2", "pending", Some("alice"));
        create_task_file(&tasks_dir, "3", "pending", None);
        create_task_file(&tasks_dir, "4", "in_progress", None);

        let tasks = read_tasks_from_dir(&tasks_dir);
        let pending_without_owners: Vec<_> = tasks
            .into_iter()
            .filter(|t| t.status == TaskStatus::Pending && t.owner.is_none())
            .collect();

        assert_eq!(pending_without_owners.len(), 2);
        assert_eq!(pending_without_owners[0].id, "1");
        assert_eq!(pending_without_owners[1].id, "3");
    }

    #[test]
    fn test_parse_quoted_empty_owner_as_none() {
        // Bug: Claude Code sometimes sets owner to literal '""' (two quote chars)
        // which passes is_empty() check (length 2) and causes the daemon to
        // spawn a coworker with an empty name.
        let json = r#"{"id": "367", "subject": "Score and filter issues", "status": "in_progress", "owner": "\"\""}"#;
        let task = parse_task_json(json).unwrap();
        assert!(
            task.owner.is_none(),
            "owner '\"\"' (literal quotes) should be parsed as None, got {:?}",
            task.owner
        );
    }

    #[test]
    fn test_parse_whitespace_only_owner_as_none() {
        let json = r#"{"id": "1", "subject": "Test", "status": "pending", "owner": "  "}"#;
        let task = parse_task_json(json).unwrap();
        assert!(
            task.owner.is_none(),
            "whitespace-only owner should be parsed as None, got {:?}",
            task.owner
        );
    }

    #[test]
    fn test_set_task_in_progress_in_dir() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        create_task_file(&tasks_dir, "42", "pending", Some("alice"));

        // Verify task starts as pending
        let tasks = read_tasks_from_dir(&tasks_dir);
        let task = tasks.iter().find(|t| t.id == "42").unwrap();
        assert_eq!(task.status, TaskStatus::Pending);

        // Set to in_progress
        set_task_in_progress_in_dir("42", &tasks_dir).unwrap();

        // Verify status changed to in_progress and owner is preserved
        let tasks = read_tasks_from_dir(&tasks_dir);
        let task = tasks.iter().find(|t| t.id == "42").unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.owner, Some("alice".to_string()));
    }

    #[test]
    fn test_set_task_in_progress_in_dir_nonexistent_task() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        let result = set_task_in_progress_in_dir("999", &tasks_dir);
        assert!(result.is_err(), "Should error for nonexistent task");
    }

    #[test]
    fn test_busy_coworkers_from_in_progress_tasks() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create various tasks - only in_progress with owners should count as busy
        create_task_file(&tasks_dir, "1", "pending", Some("alice")); // Not busy (pending)
        create_task_file(&tasks_dir, "2", "in_progress", Some("bob")); // Busy
        create_task_file(&tasks_dir, "3", "in_progress", Some("carol")); // Busy
        create_task_file(&tasks_dir, "4", "completed", Some("dave")); // Not busy (completed)
        create_task_file(&tasks_dir, "5", "in_progress", None); // Not counted (no owner)

        let tasks = read_tasks_from_dir(&tasks_dir);

        // Simulate get_busy_coworkers logic
        let busy_coworkers: Vec<String> = tasks
            .into_iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .filter_map(|t| t.owner)
            .collect();

        assert_eq!(busy_coworkers.len(), 2);
        assert!(busy_coworkers.contains(&"bob".to_string()));
        assert!(busy_coworkers.contains(&"carol".to_string()));
        // alice, dave should NOT be in list (pending, completed respectively)
        assert!(!busy_coworkers.contains(&"alice".to_string()));
        assert!(!busy_coworkers.contains(&"dave".to_string()));
    }

    #[test]
    fn test_extract_pr_number() {
        assert_eq!(
            extract_pr_number("Score and filter issues for PR #235"),
            Some("235".to_string())
        );
        assert_eq!(
            extract_pr_number("Post review comment on PR #42"),
            Some("42".to_string())
        );
        assert_eq!(extract_pr_number("Review PR #100"), Some("100".to_string()));
        assert_eq!(
            extract_pr_number("Check PR #7 eligibility"),
            Some("7".to_string())
        );
        assert_eq!(extract_pr_number("No PR number here"), None);
        assert_eq!(extract_pr_number("PR # no digits"), None);
        assert_eq!(extract_pr_number(""), None);
    }

    #[test]
    fn test_find_pr_owner_from_tasks() {
        let tasks = vec![
            Task {
                id: "1".to_string(),
                subject: "Review PR #42".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("alice".to_string()),
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "2".to_string(),
                subject: "Score and filter issues for PR #42".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "3".to_string(),
                subject: "Post review comment on PR #99".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("bob".to_string()),
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
        ];

        // Use the new find_pr_owner_in_tasks function
        assert_eq!(
            find_pr_owner_in_tasks("42", &tasks),
            Some("alice".to_string())
        );
        assert_eq!(
            find_pr_owner_in_tasks("99", &tasks),
            Some("bob".to_string())
        );
        assert_eq!(find_pr_owner_in_tasks("55", &tasks), None);
    }

    #[test]
    fn test_find_pr_owner_from_description() {
        // Tasks where the PR number is only in the description, not the subject
        let tasks = vec![
            Task {
                id: "10".to_string(),
                subject: "Code review PR #239".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("vernon".to_string()),
                description: Some("Review PR #239 changes".to_string()),
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "11".to_string(),
                subject: "Find relevant CLAUDE.md files".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: Some("Sub-task for PR #239 review".to_string()),
                blocked_by: vec!["10".to_string()],
                created_at: None,
            },
        ];

        // The main task has PR #239 in its subject — should find vernon
        assert_eq!(
            find_pr_owner_in_tasks("239", &tasks),
            Some("vernon".to_string())
        );

        // extract_pr_number_from_task should find PR number in description
        assert_eq!(
            extract_pr_number_from_task(&tasks[1]),
            Some("239".to_string())
        );
    }

    #[test]
    fn test_extract_pr_number_from_task_subject_only() {
        let task = Task {
            id: "1".to_string(),
            subject: "Check PR #42 eligibility".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        assert_eq!(extract_pr_number_from_task(&task), Some("42".to_string()));
    }

    #[test]
    fn test_extract_pr_number_from_task_description_only() {
        let task = Task {
            id: "1".to_string(),
            subject: "Run 5 parallel code review agents".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: Some("Part of PR #239 review workflow".to_string()),
            blocked_by: vec![],
            created_at: None,
        };
        assert_eq!(extract_pr_number_from_task(&task), Some("239".to_string()));
    }

    #[test]
    fn test_extract_pr_number_from_task_no_pr() {
        let task = Task {
            id: "1".to_string(),
            subject: "Score and filter issues".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: Some("Generic scoring task".to_string()),
            blocked_by: vec![],
            created_at: None,
        };
        assert_eq!(extract_pr_number_from_task(&task), None);
    }

    #[test]
    fn test_find_owner_via_blocked_by() {
        let tasks = vec![
            Task {
                id: "100".to_string(),
                subject: "Code review PR #239".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("vernon".to_string()),
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "101".to_string(),
                subject: "Run 5 parallel code review agents".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["100".to_string()],
                created_at: None,
            },
            Task {
                id: "102".to_string(),
                subject: "Score and filter issues".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["101".to_string()],
                created_at: None,
            },
        ];

        // Task 101 is blocked by task 100 (owned by vernon)
        assert_eq!(
            find_owner_via_blocked_by(&tasks[1], &tasks),
            Some("vernon".to_string())
        );

        // Task 102 is blocked by task 101 (no owner yet) — should return None
        assert_eq!(find_owner_via_blocked_by(&tasks[2], &tasks), None);

        // Task 100 has no blockedBy — should return None
        assert_eq!(find_owner_via_blocked_by(&tasks[0], &tasks), None);
    }

    #[test]
    fn test_find_pr_owner_checks_description() {
        // Verify find_pr_owner_in_tasks checks description too
        let tasks = vec![Task {
            id: "50".to_string(),
            subject: "Some review task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("alice".to_string()),
            description: Some("Reviewing PR #88 changes".to_string()),
            blocked_by: vec![],
            created_at: None,
        }];

        assert_eq!(
            find_pr_owner_in_tasks("88", &tasks),
            Some("alice".to_string())
        );
    }

    #[test]
    fn test_parse_blocked_by_from_json() {
        let json =
            r#"{"id": "5", "subject": "Sub task", "status": "pending", "blockedBy": ["3", "4"]}"#;
        let task = parse_task_json(json).unwrap();
        assert_eq!(task.blocked_by, vec!["3".to_string(), "4".to_string()]);
    }

    #[test]
    fn test_parse_blocked_by_missing() {
        let json = r#"{"id": "5", "subject": "Sub task", "status": "pending"}"#;
        let task = parse_task_json(json).unwrap();
        assert!(task.blocked_by.is_empty());
    }

    #[test]
    fn test_grace_period_filters_recently_created_tasks() {
        use std::time::{Duration, SystemTime};

        let now = SystemTime::now();
        let grace = Duration::from_secs(45);

        // Task created 10 seconds ago — within grace period, should be filtered
        let recent_task = Task {
            id: "1".to_string(),
            subject: "Check PR #246 eligibility".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            created_at: Some(now - Duration::from_secs(10)),
        };
        assert!(is_within_grace_period(&recent_task, now, grace));

        // Task created 60 seconds ago — outside grace period, should NOT be filtered
        let old_task = Task {
            id: "2".to_string(),
            subject: "Score and filter issues".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            created_at: Some(now - Duration::from_secs(60)),
        };
        assert!(!is_within_grace_period(&old_task, now, grace));

        // Task with no created_at — should NOT be filtered (assume old)
        let no_time_task = Task {
            id: "3".to_string(),
            subject: "Some task".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            created_at: None,
        };
        assert!(!is_within_grace_period(&no_time_task, now, grace));
    }

    #[test]
    fn test_has_unresolved_blockers() {
        let all_tasks = vec![
            Task {
                id: "1".to_string(),
                subject: "Completed prerequisite".to_string(),
                status: TaskStatus::Completed,
                owner: Some("alice".to_string()),
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "2".to_string(),
                subject: "In-progress prerequisite".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("bob".to_string()),
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
            Task {
                id: "3".to_string(),
                subject: "Blocked by completed task".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["1".to_string()],
                created_at: None,
            },
            Task {
                id: "4".to_string(),
                subject: "Blocked by in-progress task".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["2".to_string()],
                created_at: None,
            },
            Task {
                id: "5".to_string(),
                subject: "Blocked by nonexistent task".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["999".to_string()],
                created_at: None,
            },
            Task {
                id: "6".to_string(),
                subject: "Blocked by mix: one completed, one pending".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["1".to_string(), "2".to_string()],
                created_at: None,
            },
            Task {
                id: "7".to_string(),
                subject: "No blockers at all".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec![],
                created_at: None,
            },
        ];

        // Task 3: blocked by completed task → no unresolved blockers
        assert!(!has_unresolved_blockers(&all_tasks[2], &all_tasks));

        // Task 4: blocked by in-progress task → has unresolved blockers
        assert!(has_unresolved_blockers(&all_tasks[3], &all_tasks));

        // Task 5: blocked by nonexistent task → treat as unresolved
        assert!(has_unresolved_blockers(&all_tasks[4], &all_tasks));

        // Task 6: blocked by both completed AND in-progress → still blocked
        assert!(has_unresolved_blockers(&all_tasks[5], &all_tasks));

        // Task 7: no blockedBy at all → not blocked
        assert!(!has_unresolved_blockers(&all_tasks[6], &all_tasks));
    }

    #[test]
    fn test_grace_period_reproduces_split_bug() {
        // Reproduces the bug: a coworker creates review sub-tasks but the daemon
        // picks them up before ownership is set, splitting them across coworkers.
        //
        // Before the fix, all three tasks would be returned by the unowned filter.
        // After the fix, only the old task is returned (recent ones are in grace period).
        use std::time::{Duration, SystemTime};

        let now = SystemTime::now();

        let tasks = [
            // Sub-task just created by reviewing coworker (5 seconds ago)
            Task {
                id: "471".to_string(),
                subject: "Check PR #246 eligibility".to_string(),
                status: TaskStatus::Pending,
                owner: None, // Owner not yet set — coworker will TaskUpdate shortly
                description: Some("Check if PR #246 is eligible".to_string()),
                blocked_by: vec![],
                created_at: Some(now - Duration::from_secs(5)),
            },
            // Another sub-task just created (3 seconds ago)
            Task {
                id: "472".to_string(),
                subject: "Run 5 parallel code review agents".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: Some("Review agents for PR #246".to_string()),
                blocked_by: vec![],
                created_at: Some(now - Duration::from_secs(3)),
            },
            // An older task that legitimately has no owner (created 2 minutes ago)
            Task {
                id: "400".to_string(),
                subject: "Fix bug in auth module".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec![],
                created_at: Some(now - Duration::from_secs(120)),
            },
        ];

        let grace = Duration::from_secs(TASK_CREATION_GRACE_SECS);
        let filtered: Vec<&Task> = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && t.owner.is_none()
                    && !is_within_grace_period(t, now, grace)
            })
            .collect();

        // Only the old task should pass the filter
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "400");
    }

    #[test]
    fn test_extract_task_id_from_pr_title() {
        assert_eq!(
            extract_task_id_from_pr_title("feat: Add auth endpoint [Midtown #42]"),
            Some(42)
        );
        assert_eq!(
            extract_task_id_from_pr_title("fix: Something [Midtown #7]"),
            Some(7)
        );
        assert_eq!(extract_task_id_from_pr_title("No task id here"), None);
        assert_eq!(extract_task_id_from_pr_title(""), None);
        assert_eq!(extract_task_id_from_pr_title("[Midtown #] empty id"), None);
        assert_eq!(
            extract_task_id_from_pr_title("prefix [Midtown #123] suffix"),
            Some(123)
        );
    }

    #[test]
    fn test_reset_task_uses_shared_task_list_path() {
        // Verify reset_task_to_pending_for_repo constructs the same path as
        // update_task_owner — both should use midtown-<repo>, NOT the lead session UUID.
        let repo_name = "testrepo";
        let expected_dir_name = format!("midtown-{}", repo_name);

        // The task_list_id_for_repo function should produce the shared directory name
        let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
        assert_eq!(
            task_list_id, expected_dir_name,
            "task_list_id_for_repo should return midtown-<repo>"
        );
    }

    #[test]
    fn test_complete_task_in_dir() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create an in_progress task
        let task = serde_json::json!({
            "id": "42",
            "subject": "Test task",
            "status": "in_progress",
            "owner": "vernon"
        });
        let task_file = tasks_dir.join("42.json");
        std::fs::write(&task_file, serde_json::to_string(&task).unwrap()).unwrap();

        // Verify initial state
        let content = std::fs::read_to_string(&task_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["status"], "in_progress");

        // Call the function under test
        complete_task_in_dir("42", &tasks_dir).unwrap();

        // Verify the task is now completed
        let content = std::fs::read_to_string(&task_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["status"], "completed");
    }

    #[test]
    fn test_complete_task_in_dir_nonexistent_task() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        let result = complete_task_in_dir("999", &tasks_dir);
        assert!(result.is_err(), "Should error for nonexistent task");
    }

    #[test]
    fn test_clear_blocked_by_removes_completed_task() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create a task that is blocked by task "1" and "3"
        let blocked_task = serde_json::json!({
            "id": "2",
            "subject": "Blocked task",
            "status": "pending",
            "blockedBy": ["1", "3"]
        });
        let blocked_file = tasks_dir.join("2.json");
        std::fs::write(&blocked_file, serde_json::to_string(&blocked_task).unwrap()).unwrap();

        // Create another task blocked only by "1"
        let blocked_task2 = serde_json::json!({
            "id": "4",
            "subject": "Another blocked task",
            "status": "pending",
            "blockedBy": ["1"]
        });
        let blocked_file2 = tasks_dir.join("4.json");
        std::fs::write(
            &blocked_file2,
            serde_json::to_string(&blocked_task2).unwrap(),
        )
        .unwrap();

        // Verify initial state
        let content = std::fs::read_to_string(&blocked_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["blockedBy"].as_array().unwrap().len(), 2);

        // Call the function under test — clear task "1" from all blockedBy arrays
        clear_blocked_by_in_dir("1", &tasks_dir).unwrap();

        // Task 2: should have only "3" remaining in blockedBy
        let content = std::fs::read_to_string(&blocked_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let blocked_by = parsed["blockedBy"].as_array().unwrap();
        assert_eq!(blocked_by.len(), 1);
        assert_eq!(blocked_by[0], "3");

        // Task 4: should have empty blockedBy
        let content2 = std::fs::read_to_string(&blocked_file2).unwrap();
        let parsed2: serde_json::Value = serde_json::from_str(&content2).unwrap();
        let blocked_by2 = parsed2["blockedBy"].as_array().unwrap();
        assert_eq!(blocked_by2.len(), 0);
    }

    #[test]
    fn test_clear_blocked_by_no_match() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create a task blocked by "5"
        let task = serde_json::json!({
            "id": "2",
            "subject": "Blocked task",
            "status": "pending",
            "blockedBy": ["5"]
        });
        let task_file = tasks_dir.join("2.json");
        std::fs::write(&task_file, serde_json::to_string(&task).unwrap()).unwrap();

        // Clear task "99" — no match, file should remain unchanged
        clear_blocked_by_in_dir("99", &tasks_dir).unwrap();

        let content = std::fs::read_to_string(&task_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["blockedBy"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_parse_blocked_by_with_numeric_ids() {
        // Test that blockedBy arrays with numeric IDs are handled correctly
        let json = r#"{"id": "5", "subject": "Test", "status": "pending", "blockedBy": [1, 2]}"#;
        let task = parse_task_json(json).unwrap();
        assert_eq!(task.blocked_by, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn test_create_task_in_dir_basic() {
        let temp_dir = TempDir::new().unwrap();
        let result = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "PR #42 has feedback",
            "Addressing review feedback on PR #42",
            "madison",
        );
        assert!(result.is_ok());
        let task_id = result.unwrap();
        assert_eq!(task_id, "1");

        let tasks = read_tasks_from_dir(&temp_dir.path().to_path_buf());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].subject, "Address review feedback on PR #42");
        assert_eq!(tasks[0].owner, Some("madison".to_string()));
    }

    #[test]
    fn test_create_task_in_dir_dedup_pending() {
        let temp_dir = TempDir::new().unwrap();

        // Create first task
        let id1 = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "desc",
            "Addressing review feedback on PR #42",
            "madison",
        )
        .unwrap();

        // Same subject+owner should return existing ID, not create a new task
        let id2 = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "desc",
            "Addressing review feedback on PR #42",
            "madison",
        )
        .unwrap();

        assert_eq!(id1, id2);
        assert_eq!(read_tasks_from_dir(&temp_dir.path().to_path_buf()).len(), 1);
    }

    #[test]
    fn test_create_task_in_dir_dedup_completed() {
        let temp_dir = TempDir::new().unwrap();

        // Create a completed task with the same subject+owner
        let task = serde_json::json!({
            "id": "10",
            "subject": "Address review feedback on PR #42",
            "status": "completed",
            "owner": "madison",
        });
        let path = temp_dir.path().join("10.json");
        std::fs::write(&path, serde_json::to_string(&task).unwrap()).unwrap();

        // Calling again with same subject+owner should return the existing ID,
        // NOT create a new task (prevents re-creation on every poll cycle)
        let result = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "desc",
            "Addressing review feedback on PR #42",
            "madison",
        )
        .unwrap();

        assert_eq!(result, "10", "should return existing completed task ID");
        assert_eq!(
            read_tasks_from_dir(&temp_dir.path().to_path_buf()).len(),
            1,
            "should not create a duplicate task"
        );
    }

    #[test]
    fn test_create_task_in_dir_different_owner_allowed() {
        let temp_dir = TempDir::new().unwrap();

        let id1 = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "desc",
            "Addressing feedback",
            "madison",
        )
        .unwrap();

        // Different owner should create a separate task
        let id2 = create_task_in_dir(
            temp_dir.path(),
            "Address review feedback on PR #42",
            "desc",
            "Addressing feedback",
            "lexington",
        )
        .unwrap();

        assert_ne!(id1, id2);
        assert_eq!(read_tasks_from_dir(&temp_dir.path().to_path_buf()).len(), 2);
    }

    #[test]
    fn test_create_task_in_dir_sequential_ids() {
        let temp_dir = TempDir::new().unwrap();

        // Pre-populate with existing tasks
        create_task_file(temp_dir.path(), "5", "completed", Some("alice"));
        create_task_file(temp_dir.path(), "10", "pending", Some("bob"));

        let id = create_task_in_dir(
            temp_dir.path(),
            "New task",
            "desc",
            "Working on new task",
            "carol",
        )
        .unwrap();

        assert_eq!(id, "11", "should use max existing ID + 1");
    }

    #[test]
    fn test_create_task_in_dir_single_read() {
        // Verify that create_task_in_dir reads the directory once, not twice.
        // The previous implementation had two separate read_tasks_from_dir calls.
        // We verify correctness by ensuring ID assignment and dedup are consistent
        // even when pre-existing tasks are present.
        let temp_dir = TempDir::new().unwrap();

        create_task_file(temp_dir.path(), "3", "pending", Some("alice"));

        // This should read once, find max_id=3, and assign id=4
        let id = create_task_in_dir(temp_dir.path(), "New task", "desc", "Working", "bob").unwrap();
        assert_eq!(id, "4");

        // Dedup should work for the newly created task
        let id2 =
            create_task_in_dir(temp_dir.path(), "New task", "desc", "Working", "bob").unwrap();
        assert_eq!(id2, "4", "should dedup against newly created task");
    }

    #[test]
    fn test_update_task_owner_preserves_pending_status() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create a pending task with no owner
        let task = serde_json::json!({
            "id": "42",
            "subject": "Test task",
            "status": "pending",
            "owner": null
        });
        let task_file = tasks_dir.join("42.json");
        std::fs::write(&task_file, serde_json::to_string(&task).unwrap()).unwrap();

        // Assign owner
        update_task_owner_in_dir("42", "vernon", &tasks_dir).unwrap();

        // Verify owner was updated but status stays "pending" (the "pending with
        // owner" state is load-bearing for dispatch and idle shutdown protection)
        let content = std::fs::read_to_string(&task_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["owner"], "vernon");
        assert_eq!(
            parsed["status"], "pending",
            "Assigning a task owner should keep status as pending"
        );
    }

    #[test]
    fn test_update_task_owner_in_dir_nonexistent_task() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        let result = update_task_owner_in_dir("nonexistent", "bob", &tasks_dir);
        assert!(result.is_err(), "Should error for nonexistent task");
    }

    /// Reproduces the reassignment loop bug: a coworker completes its task but
    /// the shared list still shows the task as pending-with-owner. The daemon
    /// reads the shared list, sees the pending task, and reassigns it in a loop.
    ///
    /// The root cause: with isolated task lists (PR #656), coworker completions
    /// write to the isolated list, not the shared list the daemon reads.
    /// The fix requires the daemon to complete the shared task when a coworker
    /// reports the "completed" phase via RPC.
    #[test]
    fn test_reassignment_loop_pending_task_stays_after_coworker_completes() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Step 1: Daemon creates a pending task
        create_task_file(&tasks_dir, "778", "pending", None);

        // Step 2: Daemon assigns ownership (task stays pending-with-owner)
        update_task_owner_in_dir("778", "vernon", &tasks_dir).unwrap();

        // Verify: task is pending with owner — daemon would try to spawn/nudge
        let tasks = read_tasks_from_dir(&tasks_dir);
        let pending_with_owners: Vec<_> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending && t.owner.is_some())
            .collect();
        assert_eq!(
            pending_with_owners.len(),
            1,
            "Task should be pending with owner"
        );

        // Step 3: Coworker completes the task. With task isolation, this writes
        // to the coworker's isolated list — NOT the shared list. The shared list
        // is unchanged.
        //
        // BUG: Without the fix, the shared list still shows 778 as pending-with-owner.
        // The daemon would see this and try to reassign it.
        //
        // FIX: The daemon's RPC handler must call complete_task_in_dir when a
        // coworker reports "completed" phase with a task_id.
        complete_task_in_dir("778", &tasks_dir).unwrap();

        // After the fix: task should be completed, not pending
        let tasks = read_tasks_from_dir(&tasks_dir);
        let pending_with_owners: Vec<_> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending && t.owner.is_some())
            .collect();
        assert_eq!(
            pending_with_owners.len(),
            0,
            "After completion, no tasks should be pending-with-owner"
        );

        // The completed task should not appear in any pending query
        let task = tasks.iter().find(|t| t.id == "778").unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn test_update_task_owner_preserves_other_fields() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        // Create a task with extra fields
        let task = serde_json::json!({
            "id": "7",
            "subject": "Complex task",
            "description": "A detailed description",
            "status": "pending",
            "owner": null,
            "blockedBy": ["5"],
            "activeForm": "Working on it"
        });
        let task_file = tasks_dir.join("7.json");
        std::fs::write(&task_file, serde_json::to_string(&task).unwrap()).unwrap();

        // Assign owner
        update_task_owner_in_dir("7", "park", &tasks_dir).unwrap();

        // Verify owner changed, status stays pending, other fields preserved
        let content = std::fs::read_to_string(&task_file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["owner"], "park");
        assert_eq!(parsed["status"], "pending");
        assert_eq!(parsed["subject"], "Complex task");
        assert_eq!(parsed["description"], "A detailed description");
        assert_eq!(parsed["blockedBy"], serde_json::json!(["5"]));
        assert_eq!(parsed["activeForm"], "Working on it");
    }

    // --- Tests for Lead task persistence mirroring (task #808) ---

    #[test]
    fn test_ensure_task_in_shared_dir_creates_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path();

        // Pre-populate with existing tasks (simulates shared dir with IDs 800-803)
        for id in 800..=803 {
            create_task_file(tasks_dir, &id.to_string(), "completed", Some("alice"));
        }

        let (shared_id, was_created) =
            ensure_task_in_shared_dir(tasks_dir, "Fix auth endpoint", "Needs investigation")
                .unwrap();

        assert!(was_created, "should have created a new task");
        assert_eq!(shared_id, "804", "should use next sequential ID after 803");

        // Verify the file was written with correct content
        let content = std::fs::read_to_string(tasks_dir.join("804.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["subject"], "Fix auth endpoint");
        assert_eq!(parsed["description"], "Needs investigation");
        assert_eq!(parsed["status"], "pending");
        assert_eq!(
            parsed["blocks"],
            serde_json::json!([]),
            "should include blocks field"
        );
    }

    #[test]
    fn test_ensure_task_in_shared_dir_returns_existing() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path();

        // Create a task with the same subject
        let task = serde_json::json!({
            "id": "805",
            "subject": "Fix auth endpoint",
            "description": "Already exists",
            "status": "in_progress",
            "owner": "lexington"
        });
        let path = tasks_dir.join("805.json");
        std::fs::write(&path, serde_json::to_string(&task).unwrap()).unwrap();

        let (shared_id, was_created) =
            ensure_task_in_shared_dir(tasks_dir, "Fix auth endpoint", "Different description")
                .unwrap();

        assert!(!was_created, "should NOT have created a new task");
        assert_eq!(shared_id, "805", "should return existing task ID");

        // Verify no new files were created
        let tasks = read_tasks_from_dir(&tasks_dir.to_path_buf());
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn test_ensure_task_in_shared_dir_skips_completed_for_dedup() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path();

        // Create a completed task with the same subject (previous cycle)
        let task = serde_json::json!({
            "id": "805",
            "subject": "Fix auth endpoint",
            "description": "Old completed task",
            "status": "completed",
            "owner": "lexington"
        });
        std::fs::write(
            tasks_dir.join("805.json"),
            serde_json::to_string(&task).unwrap(),
        )
        .unwrap();

        // Should create a new task, NOT return the completed one
        let (shared_id, was_created) =
            ensure_task_in_shared_dir(tasks_dir, "Fix auth endpoint", "New cycle").unwrap();

        assert!(
            was_created,
            "should create a new task, not match the completed one"
        );
        assert_eq!(shared_id, "806", "should use next sequential ID");

        // Verify both files exist
        let tasks = read_tasks_from_dir(&tasks_dir.to_path_buf());
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_ensure_task_in_shared_dir_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().join("nonexistent").join("subdir");

        let (shared_id, was_created) =
            ensure_task_in_shared_dir(&tasks_dir, "New task", "Description").unwrap();

        assert!(was_created);
        assert_eq!(shared_id, "1", "first task in empty dir gets ID 1");
        assert!(tasks_dir.join("1.json").exists());
    }

    #[test]
    fn test_update_task_fields_in_dir_status_and_owner() {
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path();

        // Create a pending task
        let task = serde_json::json!({
            "id": "805",
            "subject": "Fix auth endpoint",
            "status": "pending",
            "owner": null
        });
        std::fs::write(
            tasks_dir.join("805.json"),
            serde_json::to_string(&task).unwrap(),
        )
        .unwrap();

        // Update status and owner
        let updates = serde_json::json!({
            "status": "in_progress",
            "owner": "lexington"
        });
        update_task_fields_in_dir(tasks_dir, "805", &updates).unwrap();

        // Verify updates applied
        let content = std::fs::read_to_string(tasks_dir.join("805.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["status"], "in_progress");
        assert_eq!(parsed["owner"], "lexington");
        assert_eq!(parsed["subject"], "Fix auth endpoint", "subject preserved");
    }

    #[test]
    fn test_update_task_fields_in_dir_nonexistent() {
        let temp_dir = TempDir::new().unwrap();
        let updates = serde_json::json!({"status": "completed"});
        let result = update_task_fields_in_dir(temp_dir.path(), "999", &updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_lead_task_persistence_resume_scenario() {
        // End-to-end scenario: simulates what happens when the Lead /resumes
        // and Claude Code doesn't persist tasks to the shared directory.
        //
        // 1. Shared dir has existing tasks (IDs 800-803)
        // 2. Lead creates a task → Claude Code assigns internal ID "1" but doesn't write to shared dir
        // 3. Hook detects missing task, creates it in shared dir as ID 804
        // 4. Lead updates task "1" → Hook remaps to shared ID "804" and updates
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path();
        let map_dir = temp_dir.path().join("map");
        std::fs::create_dir_all(&map_dir).unwrap();
        let map_file = map_dir.join("lead-task-id-map.json");

        // Step 1: Existing tasks in shared directory
        for id in 800..=803 {
            create_task_file(tasks_dir, &id.to_string(), "completed", Some("alice"));
        }

        // Step 2: Lead's TaskCreate — task NOT in shared dir yet
        let (shared_id, was_created) =
            ensure_task_in_shared_dir(tasks_dir, "Fix auth endpoint", "Needs fixing").unwrap();
        assert!(was_created);
        assert_eq!(shared_id, "804");

        // Step 3: Store the ID mapping (internal "1" → shared "804")
        let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        map.insert("1".to_string(), shared_id.clone());
        std::fs::write(&map_file, serde_json::to_string(&map).unwrap()).unwrap();

        // Step 4: Lead's TaskUpdate with internal ID "1" → remap to "804"
        let stored_map: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&map_file).unwrap()).unwrap();
        let actual_id = stored_map.get("1").unwrap();
        assert_eq!(actual_id, "804");

        // Apply update to the remapped task
        let updates = serde_json::json!({
            "status": "in_progress",
            "owner": "lexington"
        });
        update_task_fields_in_dir(tasks_dir, actual_id, &updates).unwrap();

        // Verify the shared task was updated correctly
        let content = std::fs::read_to_string(tasks_dir.join("804.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["subject"], "Fix auth endpoint");
        assert_eq!(parsed["status"], "in_progress");
        assert_eq!(parsed["owner"], "lexington");
    }

    #[test]
    fn test_ensure_task_in_shared_dir_concurrent_no_id_collision() {
        // Verify that concurrent calls to ensure_task_in_shared_dir with different
        // subjects produce unique IDs (file locking prevents TOCTOU races).
        let temp_dir = TempDir::new().unwrap();
        let tasks_dir = temp_dir.path().to_path_buf();

        let mut handles = Vec::new();
        for i in 0..10 {
            let dir = tasks_dir.clone();
            handles.push(std::thread::spawn(move || {
                ensure_task_in_shared_dir(
                    &dir,
                    &format!("Task number {}", i),
                    &format!("Description {}", i),
                )
                .unwrap()
            }));
        }

        let results: Vec<(String, bool)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let ids: std::collections::HashSet<String> =
            results.iter().map(|(id, _)| id.clone()).collect();

        // All 10 tasks should have unique IDs
        assert_eq!(ids.len(), 10, "all concurrent tasks should get unique IDs");

        // Verify 10 task files exist on disk
        let tasks = read_tasks_from_dir(&tasks_dir);
        assert_eq!(tasks.len(), 10);
    }
}
