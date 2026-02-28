use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum PrCommand {
    /// List pull requests
    List,
    /// Request a reviewer for the given PR number
    Review {
        /// The PR number to review
        pr_number: u64,
    },
    /// Merge a PR (daemon-gated: checks review, CI, and addressed feedback)
    Merge {
        /// The PR number to merge
        #[arg(long)]
        pr: u64,
    },
}

pub fn handle(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        PrCommand::List => client.pr_list(),
        PrCommand::Review { pr_number } => client.pr_review(*pr_number),
        PrCommand::Merge { pr } => client.pr_merge(*pr),
    }
}
