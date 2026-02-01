mod channel;
mod chat;
mod coworker;
mod daemon;
mod hooks;
mod lead;
mod pr;
mod response;
mod task;

pub use channel::ChannelCommand;
pub use coworker::CoworkerCommand;
pub use hooks::HookCommand;
// Note: daemon::get_lead_status and daemon::LEAD_SESSION available if needed
pub use pr::PrCommand;
pub use response::Response;
pub use task::TaskCommand;

use crate::client::DaemonClient;

pub fn handle_channel(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    channel::handle(cmd, client)
}

pub fn handle_coworker(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    coworker::handle(cmd, client)
}

pub fn handle_task(cmd: &TaskCommand, client: &DaemonClient) -> Result<Response, String> {
    task::handle(cmd, client)
}

pub fn handle_status(client: &DaemonClient) -> Result<Response, String> {
    client.status()
}

pub fn handle_pr(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    pr::handle(cmd, client)
}

/// Handle start command (no daemon required - it starts the daemon)
pub fn handle_start(
    daemon_only: bool,
    project: Option<String>,
    repos: Vec<std::path::PathBuf>,
) -> Result<Response, String> {
    daemon::handle_start(daemon_only, project, repos)
}

/// Handle stop command (no daemon required - it stops the daemon)
pub fn handle_stop(keep_session: bool) -> Result<Response, String> {
    daemon::handle_stop(keep_session)
}

/// Handle restart command (stop + start)
pub fn handle_restart() -> Result<Response, String> {
    daemon::handle_restart()
}

/// Handle attach command (no daemon required - just attaches to tmux)
pub fn handle_attach(project: Option<&str>) -> Result<Response, String> {
    daemon::handle_attach(project)
}

/// Handle project list command (no daemon required)
pub fn handle_project_list() -> Result<Response, String> {
    daemon::handle_project_list()
}

/// Handle lead register-session command (no daemon required)
pub fn handle_register_session() -> Result<Response, String> {
    daemon::handle_register_session()
}

/// Handle chat command (no daemon required - standalone TUI)
pub fn handle_chat() -> Result<(), String> {
    chat::run()
}

/// Handle `midtown state <phase> [--task <id>]` — writes structured coworker state.
///
/// Called explicitly by coworkers to report their workflow phase.
/// Writes a JSON state file that the daemon reads for tmux tab display.
pub fn handle_state(
    phase: midtown::coworker_state::WorkflowPhase,
    task_id: Option<u32>,
) -> Result<Response, String> {
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "unknown".to_string());
    let repo =
        hooks::detect_git_repo_public().ok_or_else(|| "Not in a git repository".to_string())?;

    let report = midtown::coworker_state::CoworkerStateReport::new(phase, task_id);
    midtown::coworker_state::write_state(&repo, &agent, &report)
        .map_err(|e| format!("Failed to write state: {}", e))?;

    Ok(Response::Message {
        message: format!("{} → {}", agent, report.display_status()),
    })
}

/// Handle hook commands (insight, idle, task, ask) - no daemon required
pub fn handle_hook(cmd: &HookCommand) -> Result<Response, String> {
    hooks::handle(cmd)
}

/// Handle `midtown lead remind` subcommands
pub fn handle_remind(
    cmd: &crate::RemindCommand,
    client: &DaemonClient,
) -> Result<Response, String> {
    lead::handle_remind(cmd, client)
}

/// Handle `midtown webserver stop` command
pub fn handle_webserver_stop() -> Result<Response, String> {
    daemon::handle_webserver_stop()
}

/// Handle `midtown webserver restart` command
pub fn handle_webserver_restart() -> Result<Response, String> {
    daemon::handle_webserver_restart()
}
