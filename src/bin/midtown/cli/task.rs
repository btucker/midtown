use clap::Subcommand;

use super::Response;
use super::response::TaskInfo;
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
        /// Set blocked-by task IDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        blocked_by: Option<Vec<String>>,
        /// Optional channel to route coworker messages for this task
        #[arg(long)]
        channel: Option<String>,
        /// Optional model for coworker (e.g., claude/opus, claude/sonnet)
        #[arg(long)]
        model: Option<String>,
        /// Explicit PR number associated with this task
        #[arg(long)]
        pr: Option<u64>,
        /// Path to an implementation plan file
        #[arg(long)]
        plan: Option<String>,
        /// Execution skill for the coworker (e.g., subagent-driven-development, executing-plans)
        #[arg(long)]
        execution_skill: Option<String>,
        /// Thread ID to route coworker updates back to the fork session that created this task
        #[arg(long)]
        thread_id: Option<String>,
    },
    /// Claim a task
    Claim {
        /// Task ID to claim
        id: String,
    },
    /// Update a task's fields
    Update {
        /// Task ID to update
        id: String,
        /// Set task owner
        #[arg(long)]
        owner: Option<String>,
        /// Set task status (pending, in_progress, completed)
        #[arg(long)]
        status: Option<String>,
        /// Set task description
        #[arg(long)]
        description: Option<String>,
        /// Set blocked-by task IDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        blocked_by: Option<Vec<String>>,
        /// Set channel for coworker messages
        #[arg(long)]
        channel: Option<String>,
        /// Set model for coworker (e.g., claude/opus, claude/sonnet)
        #[arg(long)]
        model: Option<String>,
        /// Set explicit PR number associated with this task
        #[arg(long)]
        pr: Option<u64>,
    },
    /// Mark a task as done
    Done {
        /// Task ID to mark done
        id: String,
    },
    /// Request a new task (posts to channel for the lead to review)
    Request {
        /// Description of the work needed
        description: String,
    },
    /// List all tasks
    List {
        /// Show all tasks including completed (default: only pending/in_progress)
        #[arg(long)]
        all: bool,
    },
    /// View a task's details
    View {
        /// Task ID to view
        id: String,
    },
}

/// Handle task subcommands that don't require the daemon (list, view).
/// Returns `Some` if handled locally, `None` if the command needs the daemon.
pub fn handle_local(cmd: &TaskCommand) -> Option<Result<Response, String>> {
    match cmd {
        TaskCommand::List { all } => Some(handle_list(*all)),
        TaskCommand::View { id } => Some(handle_view(id)),
        _ => None,
    }
}

pub fn handle(cmd: &TaskCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        TaskCommand::Create {
            subject,
            description,
            blocked_by,
            channel,
            model,
            pr,
            plan,
            execution_skill,
            thread_id,
        } => {
            let env_thread_id = std::env::var("MIDTOWN_BOUND_THREAD_ID").ok();
            let effective_thread_id =
                derive_thread_id(thread_id.as_deref(), env_thread_id.as_deref());
            client.task_create(
                subject,
                description,
                blocked_by.as_deref(),
                channel.as_deref(),
                model.as_deref(),
                *pr,
                plan.as_deref(),
                execution_skill.as_deref(),
                effective_thread_id.as_deref(),
            )
        }
        TaskCommand::Update {
            id,
            owner,
            status,
            description,
            blocked_by,
            channel,
            model,
            pr,
        } => client.task_update(
            id,
            owner.as_deref(),
            status.as_deref(),
            description.as_deref(),
            blocked_by.as_deref(),
            channel.as_deref(),
            model.as_deref(),
            *pr,
        ),
        TaskCommand::Claim { id } => client.task_claim(id),
        TaskCommand::Done { id } => client.task_done(id),
        TaskCommand::Request { description } => client.task_request(description),
        TaskCommand::List { all } => handle_list(*all),
        TaskCommand::View { id } => handle_view(id),
    }
}

/// List tasks from the shared task storage (client-side, no daemon needed).
///
/// Always reads from the shared `midtown-<repo>` task list, even when called by
/// isolated coworkers. This is intentional: `midtown task list` shows the daemon's
/// coordinated task list (assignments, ownership), not Claude Code's private tasks.
fn handle_list(show_all: bool) -> Result<Response, String> {
    let tasks = midtown::tasks::read_tasks();

    let task_infos: Vec<TaskInfo> = tasks
        .into_iter()
        .filter(|t| {
            show_all
                || t.status == midtown::tasks::TaskStatus::Pending
                || t.status == midtown::tasks::TaskStatus::InProgress
        })
        .map(|t| TaskInfo {
            id: format!("!{}", t.id),
            subject: t.subject,
            status: match t.status {
                midtown::tasks::TaskStatus::Pending => "pending".to_string(),
                midtown::tasks::TaskStatus::InProgress => "in_progress".to_string(),
                midtown::tasks::TaskStatus::Completed => "completed".to_string(),
            },
            assignee: t.owner,
        })
        .collect();

    Ok(Response::Tasks { tasks: task_infos })
}

/// View a single task's details (client-side for task data, queries daemon for metadata).
///
/// Reads from the shared `midtown-<repo>` task list. Task IDs in nudge messages
/// (e.g., "midtown task view 777") reference the shared list, so this always reads
/// from the correct location regardless of the caller's task isolation mode.
///
/// Queries the daemon for channel and model mappings if the daemon is running.
fn handle_view(id: &str) -> Result<Response, String> {
    let id = id
        .strip_prefix('#')
        .or_else(|| id.strip_prefix('!'))
        .unwrap_or(id);
    let tasks = midtown::tasks::read_tasks();
    let task = tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("Task !{} not found", id))?;

    let status_str = match task.status {
        midtown::tasks::TaskStatus::Pending => "pending",
        midtown::tasks::TaskStatus::InProgress => "in_progress",
        midtown::tasks::TaskStatus::Completed => "completed",
    };

    let mut output = format!("Task !{}\n", task.id);
    output.push_str("─────────────────────────────\n");
    output.push_str(&format!("Subject:  {}\n", task.subject));
    output.push_str(&format!("Status:   {}\n", status_str));
    if let Some(ref owner) = task.owner {
        output.push_str(&format!("Owner:    {}\n", owner));
    }

    // Query daemon for metadata (channel and model)
    if let Ok(client) = crate::client::DaemonClient::connect()
        && let Ok(result) = client.task_metadata(id)
    {
        if let Some(channel) = result.get("channel").and_then(|v| v.as_str()) {
            output.push_str(&format!("Channel:  {}\n", channel));
        }
        if let Some(model) = result.get("model").and_then(|v| v.as_str()) {
            output.push_str(&format!("Model:    {}\n", model));
        }
        if let Some(plan) = result.get("plan").and_then(|v| v.as_str()) {
            output.push_str(&format!("Plan:     {}\n", plan));
        }
        if let Some(skill) = result.get("execution_skill").and_then(|v| v.as_str()) {
            output.push_str(&format!("Skill:    {}\n", skill));
        }
    }
    // Silently ignore errors - daemon might not be running or metadata might not exist

    if !task.blocked_by.is_empty() {
        output.push_str(&format!("Blocked:  {}\n", task.blocked_by.join(", ")));
    }
    if let Some(ref desc) = task.description {
        output.push_str(&format!("\n{}\n", desc));
    }

    Ok(Response::Message {
        message: output.trim_end().to_string(),
    })
}

fn derive_thread_id(cli_value: Option<&str>, env_value: Option<&str>) -> Option<String> {
    fn non_empty(value: Option<&str>) -> Option<String> {
        value.and_then(|v| {
            if v.trim().is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        })
    }

    non_empty(cli_value).or_else(|| non_empty(env_value))
}

#[path = "task_tests.rs"]
#[cfg(test)]
mod tests;
