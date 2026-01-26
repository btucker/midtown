use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum CoworkerCommand {
    /// Spawn a new coworker
    Spawn,
    /// Shutdown a coworker
    Shutdown {
        /// Name of the coworker to shutdown
        name: String,
    },
    /// List all coworkers
    List,
}

pub fn handle(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        CoworkerCommand::Spawn => client.coworker_spawn(),
        CoworkerCommand::Shutdown { name } => client.coworker_shutdown(name),
        CoworkerCommand::List => client.coworker_list(),
    }
}
