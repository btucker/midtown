mod channel;
mod coworker;
mod pr;
mod response;
mod task;

pub use channel::ChannelCommand;
pub use coworker::CoworkerCommand;
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

pub fn handle_status(client: &DaemonClient) -> Result<Response, String> {
    client.status()
}

pub fn handle_pr(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    pr::handle(cmd, client)
}
