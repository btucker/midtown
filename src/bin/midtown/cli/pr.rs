use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum PrCommand {
    /// List pull requests
    List,
}

pub fn handle(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        PrCommand::List => client.pr_list(),
    }
}
