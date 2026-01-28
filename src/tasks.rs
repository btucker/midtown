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
/// Looks up the lead session ID from `~/.midtown/lead/<repo>/session-id` and
/// reads all task JSON files from `~/.claude/tasks/<session_id>/`.
pub fn read_tasks() -> Vec<Task> {
    read_tasks_for_repo(None)
}

/// Read all tasks for a specific repository.
///
/// If `repo_name` is None, attempts to detect the current repository.
pub fn read_tasks_for_repo(repo_name: Option<&str>) -> Vec<Task> {
    let repo = repo_name
        .map(String::from)
        .or_else(crate::paths::detect_repo_name)
        .unwrap_or_else(|| "default".to_string());

    // Read the lead session ID from ~/.midtown/lead/<repo>/session-id
    let lead_session_file = crate::paths::lead_session_file_for_repo(&repo);
    let Ok(lead_session_id) = std::fs::read_to_string(&lead_session_file) else {
        return Vec::new();
    };
    let lead_session_id = lead_session_id.trim();

    if lead_session_id.is_empty() {
        return Vec::new();
    }

    read_tasks_for_session(lead_session_id)
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
            && let Ok(task) = parse_task_json(&content)
        {
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
        .filter(|s| !s.is_empty())
        .map(String::from);

    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(Task {
        id,
        subject,
        status,
        owner,
        description,
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

/// Get pending tasks that have no owner (unclaimed).
pub fn get_unclaimed_tasks() -> Vec<Task> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending && t.owner.is_none())
        .collect()
}

/// Count unclaimed pending tasks.
pub fn count_unclaimed_tasks() -> usize {
    get_unclaimed_tasks().len()
}

/// Get pending tasks (ready to work on).
pub fn get_pending_tasks() -> Vec<Task> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .collect()
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
pub fn get_pending_tasks_without_owners() -> Vec<Task> {
    read_tasks()
        .into_iter()
        .filter(|t| t.status == TaskStatus::Pending && t.owner.is_none())
        .collect()
}

/// Update a task's owner and optionally status.
///
/// Writes the updated task back to disk.
pub fn update_task_owner(task_id: &str, owner: &str) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Could not determine home directory".to_string());
    };

    // Get the lead session ID
    let repo = crate::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
    let lead_session_file = crate::paths::lead_session_file_for_repo(&repo);
    let lead_session_id = std::fs::read_to_string(&lead_session_file)
        .map_err(|e| format!("Failed to read lead session ID: {}", e))?
        .trim()
        .to_string();

    if lead_session_id.is_empty() {
        return Err("Lead session ID is empty".to_string());
    }

    // Read the task file
    let task_file = home
        .join(".claude")
        .join("tasks")
        .join(&lead_session_id)
        .join(format!("{}.json", task_id));

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

    // Get the lead session ID
    let lead_session_file = crate::paths::lead_session_file_for_repo(repo_name);
    let lead_session_id = std::fs::read_to_string(&lead_session_file)
        .map_err(|e| format!("Failed to read lead session ID: {}", e))?
        .trim()
        .to_string();

    if lead_session_id.is_empty() {
        return Err("Lead session ID is empty".to_string());
    }

    // Read the task file
    let task_file = home
        .join(".claude")
        .join("tasks")
        .join(&lead_session_id)
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
}
