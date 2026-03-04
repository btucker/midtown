use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum PrCommand {
    /// List pull requests
    List,
    /// Review-related subcommands
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    /// Merge a PR (daemon-gated: checks review, CI, and addressed feedback)
    Merge {
        /// The PR number to merge
        #[arg(long)]
        pr: u64,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ReviewCommand {
    /// Request a reviewer for the given PR number
    Request {
        /// The PR number to review
        pr_number: u64,
    },
    /// Post review findings (updates the placeholder comment)
    Post {
        /// The PR number being reviewed
        #[arg(long)]
        pr: u64,
        /// Path to file containing review body (markdown)
        #[arg(long)]
        body_file: String,
    },
}

pub fn handle(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        PrCommand::List => client.pr_list(),
        PrCommand::Review { command } => match command {
            ReviewCommand::Request { pr_number } => client.pr_review(*pr_number),
            ReviewCommand::Post { pr, body_file } => {
                let body = std::fs::read_to_string(body_file)
                    .map_err(|e| format!("Failed to read {}: {}", body_file, e))?;
                client.pr_review_post(*pr, &body)
            }
        },
        PrCommand::Merge { pr } => client.pr_merge(*pr),
    }
}
