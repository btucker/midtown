mod channel;
mod coworker;
mod daemon;
mod pr;
mod response;
mod task;

pub use channel::ChannelCommand;
pub use coworker::CoworkerCommand;
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

/// Handle coworker stop hook directly (no daemon required)
pub fn handle_coworker_stop_hook() -> Result<Response, String> {
    coworker::handle_stop_hook_standalone()
}

/// Handle coworker link-tasks directly (no daemon required)
pub fn handle_coworker_link_tasks() -> Result<Response, String> {
    coworker::handle_link_tasks_standalone()
}

pub fn handle_status(client: &DaemonClient) -> Result<Response, String> {
    client.status()
}

pub fn handle_pr(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    pr::handle(cmd, client)
}

/// Handle start command (no daemon required - it starts the daemon)
pub fn handle_start(daemon_only: bool) -> Result<Response, String> {
    daemon::handle_start(daemon_only)
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
pub fn handle_attach() -> Result<Response, String> {
    daemon::handle_attach()
}

/// Handle lead register-session command (no daemon required)
pub fn handle_register_session() -> Result<Response, String> {
    daemon::handle_register_session()
}
