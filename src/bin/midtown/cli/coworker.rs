use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderArg {
    Claude,
    Codex,
    Zai,
}

impl From<ProviderArg> for midtown::auth::AuthProvider {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            ProviderArg::Codex => midtown::auth::AuthProvider::Codex,
            ProviderArg::Zai => midtown::auth::AuthProvider::Zai,
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
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
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
        } => {
            let resolved_provider = provider.map(Into::into).unwrap_or_else(|| {
                let project_name = midtown::paths::detect_repo_name().unwrap_or_default();
                midtown::config::get_execution_provider_for_role(
                    &project_name,
                    midtown::config::ExecutionRole::Coworker,
                )
            });
            client.coworker_spawn(*resume, prompt.as_deref(), resolved_provider)
        }
        CoworkerCommand::Break { name } => client.coworker_break(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::View { name } => handle_view(name, client),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
    }
}

fn handle_view(name: &str, client: &DaemonClient) -> Result<Response, String> {
    // Get the rich-text output from the daemon, then render it to ANSI for the
    // terminal so users see formatted output instead of raw markdown syntax.
    let response = client.coworker_view(name)?;
    let raw = match response {
        Response::Message { message } => message,
        other => return Ok(other),
    };
    let rendered = super::session_render::render_ansi(&raw);
    Ok(Response::message(rendered.trim_end().to_string()))
}
