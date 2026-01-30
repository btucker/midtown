use std::io::{self, BufRead};

use clap::Subcommand;
use serde::Deserialize;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum TaskCommand {
    /// Create a new task
    Create {
        /// Task subject/title
        subject: String,
        /// Task description
        #[arg(long)]
        description: String,
    },
    /// Claim a task
    Claim {
        /// Task ID to claim
        id: String,
    },
    /// Mark a task as done
    Done {
        /// Task ID to mark done
        id: String,
    },
    /// Handle Claude Code task hooks (posts to channel)
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
}

/// Claude Code task hook events
#[derive(Subcommand, Debug, Clone)]
pub enum HookEvent {
    /// Handle TaskCreate hook
    Create {
        /// Repository/channel name (defaults to current git repo)
        #[arg(long, short)]
        repo: Option<String>,
        /// Agent name (defaults to MIDTOWN_AGENT env var)
        #[arg(long, short)]
        agent: Option<String>,
    },
    /// Handle TaskUpdate hook
    Update {
        /// Repository/channel name (defaults to current git repo)
        #[arg(long, short)]
        repo: Option<String>,
        /// Agent name (defaults to MIDTOWN_AGENT env var)
        #[arg(long, short)]
        agent: Option<String>,
    },
}

/// Claude Code TaskCreate tool input
#[derive(Debug, Deserialize)]
struct TaskCreateInput {
    subject: String,
    #[serde(default)]
    description: Option<String>,
}

/// Claude Code TaskUpdate tool input
#[derive(Debug, Deserialize)]
struct TaskUpdateInput {
    #[serde(rename = "taskId")]
    task_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    subject: Option<String>,
}

pub fn handle(cmd: &TaskCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        TaskCommand::Create {
            subject,
            description,
        } => client.task_create(subject, description),
        TaskCommand::Claim { id } => client.task_claim(id),
        TaskCommand::Done { id } => client.task_done(id),
        TaskCommand::Hook { event } => handle_hook_standalone(event),
    }
}

/// Handle Claude Code task hooks by reading from stdin and posting to channel.
/// This function works standalone without requiring a daemon connection.
pub fn handle_hook_standalone(event: &HookEvent) -> Result<Response, String> {
    // Read JSON from stdin
    let stdin = io::stdin();
    let mut input = String::new();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("Failed to read stdin: {}", e))?;
        input.push_str(&line);
    }

    if input.trim().is_empty() {
        return Err("No input received on stdin".to_string());
    }

    // Determine repo name
    let repo = match event {
        HookEvent::Create { repo, .. } | HookEvent::Update { repo, .. } => repo
            .clone()
            .or_else(|| std::env::var("MIDTOWN_REPO").ok())
            .or_else(detect_git_repo)
            .ok_or("Could not determine repository. Use --repo or set MIDTOWN_REPO")?,
    };

    // Determine agent name
    let agent = match event {
        HookEvent::Create { agent, .. } | HookEvent::Update { agent, .. } => agent
            .clone()
            .or_else(|| std::env::var("MIDTOWN_AGENT").ok())
            .unwrap_or_else(|| "unknown".to_string()),
    };

    // Process based on event type
    let message_content = match event {
        HookEvent::Create { .. } => {
            let task: TaskCreateInput = serde_json::from_str(&input)
                .map_err(|e| format!("Failed to parse TaskCreate input: {}", e))?;
            match &task.description {
                Some(desc) if !desc.is_empty() => {
                    format!("{} created task: {} - {}", agent, task.subject, desc)
                }
                _ => format!("{} created task: {}", agent, task.subject),
            }
        }
        HookEvent::Update { .. } => {
            let task: TaskUpdateInput = serde_json::from_str(&input)
                .map_err(|e| format!("Failed to parse TaskUpdate input: {}", e))?;
            format_task_update(&agent, &task)
        }
    };

    // Post to channel
    post_to_channel(&repo, &agent, &message_content)?;

    Ok(Response::Message {
        message: format!("Posted to channel: {}", message_content),
    })
}

/// Format a TaskUpdate event into a human-readable message
fn format_task_update(agent: &str, task: &TaskUpdateInput) -> String {
    // Prioritize status changes, then owner changes
    if let Some(status) = &task.status {
        let action = match status.as_str() {
            "in_progress" => "started",
            "completed" => "completed",
            "pending" => "reset",
            _ => "updated",
        };
        let task_desc = task.subject.as_deref().unwrap_or(&task.task_id);
        format!("{} {} task: {}", agent, action, task_desc)
    } else if let Some(new_owner) = &task.owner {
        let task_desc = task.subject.as_deref().unwrap_or(&task.task_id);
        format!("{} claimed task: {}", new_owner, task_desc)
    } else {
        let task_desc = task.subject.as_deref().unwrap_or(&task.task_id);
        format!("{} updated task: {}", agent, task_desc)
    }
}

/// Post a message to the channel
fn post_to_channel(repo: &str, from: &str, content: &str) -> Result<(), String> {
    let channel =
        midtown::Channel::for_repo(repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    let message = midtown::Message::status(from, content);
    channel
        .send(&message)
        .map_err(|e| format!("Failed to send message: {}", e))?;

    Ok(())
}

/// Try to detect the current git repository name.
/// Uses the worktree-aware detect_repo_name() to avoid returning coworker
/// worktree names instead of the actual repository name.
fn detect_git_repo() -> Option<String> {
    midtown::paths::detect_repo_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_task_create_input() {
        let json = r#"{"subject": "Implement auth endpoint", "description": "Add JWT-based authentication"}"#;
        let task: TaskCreateInput = serde_json::from_str(json).unwrap();
        assert_eq!(task.subject, "Implement auth endpoint");
        assert_eq!(
            task.description,
            Some("Add JWT-based authentication".to_string())
        );
    }

    #[test]
    fn test_parse_task_create_minimal() {
        let json = r#"{"subject": "Fix bug"}"#;
        let task: TaskCreateInput = serde_json::from_str(json).unwrap();
        assert_eq!(task.subject, "Fix bug");
        assert_eq!(task.description, None);
    }

    #[test]
    fn test_parse_task_update_status() {
        let json = r#"{"taskId": "task-123", "status": "in_progress"}"#;
        let task: TaskUpdateInput = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "task-123");
        assert_eq!(task.status, Some("in_progress".to_string()));
        assert_eq!(task.owner, None);
    }

    #[test]
    fn test_parse_task_update_owner() {
        let json = r#"{"taskId": "task-456", "owner": "lexington"}"#;
        let task: TaskUpdateInput = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "task-456");
        assert_eq!(task.owner, Some("lexington".to_string()));
    }

    #[test]
    fn test_format_task_update_started() {
        let task = TaskUpdateInput {
            task_id: "task-1".to_string(),
            status: Some("in_progress".to_string()),
            owner: None,
            subject: Some("Implement feature X".to_string()),
        };
        let msg = format_task_update("lexington", &task);
        assert_eq!(msg, "lexington started task: Implement feature X");
    }

    #[test]
    fn test_format_task_update_completed() {
        let task = TaskUpdateInput {
            task_id: "task-2".to_string(),
            status: Some("completed".to_string()),
            owner: None,
            subject: Some("Fix login bug".to_string()),
        };
        let msg = format_task_update("park", &task);
        assert_eq!(msg, "park completed task: Fix login bug");
    }

    #[test]
    fn test_format_task_update_claimed() {
        let task = TaskUpdateInput {
            task_id: "task-3".to_string(),
            status: None,
            owner: Some("madison".to_string()),
            subject: Some("Review PR #42".to_string()),
        };
        let msg = format_task_update("system", &task);
        assert_eq!(msg, "madison claimed task: Review PR #42");
    }

    #[test]
    fn test_format_task_update_fallback_to_id() {
        let task = TaskUpdateInput {
            task_id: "task-789".to_string(),
            status: Some("in_progress".to_string()),
            owner: None,
            subject: None,
        };
        let msg = format_task_update("broadway", &task);
        assert_eq!(msg, "broadway started task: task-789");
    }
}
