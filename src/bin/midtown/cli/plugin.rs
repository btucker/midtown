//! CLI handlers for Zellij plugin communication.
//!
//! These commands provide a JSON interface for the WASM plugin to communicate
//! with the daemon via `run_command()`. Each command calls the corresponding
//! `plugin.*` RPC endpoint and outputs the result as JSON to stdout.

use crate::cli::Response;
use crate::client::DaemonClient;

/// Plugin subcommands for daemon communication.
#[derive(Clone, clap::Subcommand)]
pub enum PluginCommand {
    /// Get complete dashboard state (tasks, coworkers, channel, nudges)
    Dashboard,
    /// Attach to a coworker's session (pause headless, return session ID)
    Attach {
        /// Coworker name to attach to
        name: String,
        /// Force immediate shutdown (don't wait for turn completion)
        #[arg(long)]
        force: bool,
    },
    /// Detach from a coworker's session (resume headless)
    Detach {
        /// Coworker name to detach from
        name: String,
    },
    /// Get recent streaming events from a headless coworker
    CoworkerStream {
        /// Coworker name to view
        name: String,
    },
}

pub fn handle(cmd: &PluginCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        PluginCommand::Dashboard => {
            let result = client.plugin_dashboard()?;
            Ok(Response::Json { value: result })
        }
        PluginCommand::Attach { name, force } => {
            let result = client.plugin_attach(name, *force)?;
            Ok(Response::Json { value: result })
        }
        PluginCommand::Detach { name } => {
            let result = client.plugin_detach(name)?;
            Ok(Response::Json { value: result })
        }
        PluginCommand::CoworkerStream { name } => {
            let result = client.plugin_coworker_stream(name)?;
            Ok(Response::Json { value: result })
        }
    }
}
