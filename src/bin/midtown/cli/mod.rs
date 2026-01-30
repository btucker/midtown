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
pub use task::{HookEvent, TaskCommand};

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

/// Handle task hook events directly (no daemon required)
pub fn handle_task_hook(event: &HookEvent) -> Result<Response, String> {
    task::handle_hook_standalone(event)
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

/// Handle hook commands (insight, idle) - no daemon required
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
