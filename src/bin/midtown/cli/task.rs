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
        } => client.task_create(subject, description),
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
            id: format!("#{}", t.id),
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

/// View a single task's details (client-side, no daemon needed).
///
/// Reads from the shared `midtown-<repo>` task list. Task IDs in nudge messages
/// (e.g., "midtown task view 777") reference the shared list, so this always reads
/// from the correct location regardless of the caller's task isolation mode.
fn handle_view(id: &str) -> Result<Response, String> {
    let id = id.strip_prefix('#').unwrap_or(id);
    let tasks = midtown::tasks::read_tasks();
    let task = tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("Task #{} not found", id))?;

    let status_str = match task.status {
        midtown::tasks::TaskStatus::Pending => "pending",
        midtown::tasks::TaskStatus::InProgress => "in_progress",
        midtown::tasks::TaskStatus::Completed => "completed",
    };

    let mut output = format!("Task #{}\n", task.id);
    output.push_str("─────────────────────────────\n");
    output.push_str(&format!("Subject:  {}\n", task.subject));
    output.push_str(&format!("Status:   {}\n", status_str));
    if let Some(ref owner) = task.owner {
        output.push_str(&format!("Owner:    {}\n", owner));
    }
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
