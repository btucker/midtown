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

    // Use the shared task list ID (midtown-<repo>)
    let task_list_id = crate::paths::task_list_id();

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
            },
            Task {
                id: "2".to_string(),
                subject: "Score and filter issues for PR #42".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec![],
            },
            Task {
                id: "3".to_string(),
                subject: "Post review comment on PR #99".to_string(),
                status: TaskStatus::InProgress,
                owner: Some("bob".to_string()),
                description: None,
                blocked_by: vec![],
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
            },
            Task {
                id: "11".to_string(),
                subject: "Find relevant CLAUDE.md files".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: Some("Sub-task for PR #239 review".to_string()),
                blocked_by: vec!["10".to_string()],
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
            },
            Task {
                id: "101".to_string(),
                subject: "Run 5 parallel code review agents".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["100".to_string()],
            },
            Task {
                id: "102".to_string(),
                subject: "Score and filter issues".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec!["101".to_string()],
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
}
