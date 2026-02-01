use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum CoworkerCommand {
    /// Call in a new coworker
    #[command(alias = "spawn")]
    CallIn {
        /// Resume the previous Claude session (passes --continue to claude)
        #[arg(long)]
        resume: bool,
        /// Initial prompt to send after calling in (avoids separate nudge step)
        #[arg(long, short)]
        prompt: Option<String>,
    },
    /// Send a coworker on a break
    Break {
        /// Name of the coworker to send on a break
        name: String,
    },
    /// List all coworkers
    List,
    /// View a coworker's current terminal output
    View {
        /// Name of the coworker to view
        name: String,
    },
    /// Nudge a coworker to check in
    Nudge {
        /// Name of the coworker to nudge
        name: String,
        /// Custom message (optional)
        #[arg(short, long)]
        message: Option<String>,
    },
}

pub fn handle(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        CoworkerCommand::CallIn { resume, prompt } => {
            client.coworker_spawn(*resume, prompt.as_deref())
        }
        CoworkerCommand::Break { name } => client.coworker_break(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::View { name } => handle_view(name),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
    }
}

fn handle_view(name: &str) -> Result<Response, String> {
    let repo_name =
        midtown::paths::detect_repo_name().ok_or_else(|| "Not in a git repository".to_string())?;
    let session = format!("{}{}", midtown::tmux::SESSION_PREFIX, repo_name);
    let target = format!("{}:{}", session, name);

    match midtown::tmux::capture_pane(&target) {
        Some(content) => Ok(Response::message(content)),
        None => Err(format!(
            "Could not capture pane for coworker '{}'. Is the coworker running?",
            name
        )),
    }
}
