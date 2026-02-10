use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderArg {
    Claude,
    Codex,
}

impl From<ProviderArg> for midtown::auth::AuthProvider {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            ProviderArg::Codex => midtown::auth::AuthProvider::Codex,
        }
    }
}

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
        /// Execution provider for this coworker
        #[arg(long, value_enum, default_value = "claude")]
        provider: ProviderArg,
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
        CoworkerCommand::CallIn {
            resume,
            prompt,
            provider,
        } => client.coworker_spawn(*resume, prompt.as_deref(), (*provider).into()),
        CoworkerCommand::Break { name } => client.coworker_break(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::View { name } => handle_view(name, client),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
    }
}

fn handle_view(name: &str, client: &DaemonClient) -> Result<Response, String> {
    let repo_name =
        midtown::paths::detect_repo_name().ok_or_else(|| "Not in a git repository".to_string())?;
    let session = format!("{}{}", midtown::tmux::SESSION_PREFIX, repo_name);
    let target = format!("{}:{}", session, name);

    // Try tmux pane capture first (for headed coworkers)
    if let Some(content) = midtown::tmux::capture_pane(&target) {
        return Ok(Response::message(content));
    }

    // Fall back to headless session output via RPC
    client.coworker_view(name)
}
