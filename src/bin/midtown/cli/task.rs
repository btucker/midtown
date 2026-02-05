use clap::Subcommand;

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
    /// Request a new task (posts to channel for the lead to review)
    Request {
        /// Description of the work needed
        description: String,
    },
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
    }
}
