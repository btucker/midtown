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
/// Looks up the lead session ID from `~/.midtown/<repo>/lead-session` and
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

    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    // Read the lead session ID from ~/.midtown/<repo>/lead-session
    let lead_session_file = home.join(".midtown").join(&repo).join("lead-session");
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
}
