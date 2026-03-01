mod auth;
mod channel;
mod chat;
mod config;
mod coworker;
mod daemon;
mod diagram;
pub mod e2e;
mod headed_wrapper;
mod hooks;
mod lead;
mod pr;
mod response;
mod session;
mod session_render;
mod task;
pub mod update;

pub use auth::AuthCommand;
pub use channel::ChannelCommand;
pub use config::ConfigCommand;
pub use coworker::CoworkerCommand;
pub use diagram::DiagramCommand;
pub use e2e::E2eCommand;
pub use headed_wrapper::HeadedWrapperCommand;
pub use hooks::HookCommand;
// Note: daemon::get_lead_status and daemon::LEAD_SESSION available if needed
pub use pr::PrCommand;
pub use response::Response;
pub use session::SessionCommand;
pub use task::TaskCommand;

use crate::client::DaemonClient;

pub fn handle_channel(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    channel::handle(cmd, client)
}

pub fn handle_coworker(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    coworker::handle(cmd, client)
}

pub fn handle_session(cmd: &SessionCommand, client: &DaemonClient) -> Result<Response, String> {
    session::handle(cmd, client)
}

pub fn handle_task(cmd: &TaskCommand, client: &DaemonClient) -> Result<Response, String> {
    task::handle(cmd, client)
}

/// Handle task subcommands that don't require the daemon (list, view).
/// Returns `Some` if the command was handled locally, `None` if it needs the daemon.
pub fn handle_task_local(cmd: &TaskCommand) -> Option<Result<Response, String>> {
    task::handle_local(cmd)
}

pub fn handle_status(client: &DaemonClient) -> Result<Response, String> {
    client.status()
}

pub fn handle_headed_wrapper(
    cmd: &HeadedWrapperCommand,
    client: &DaemonClient,
) -> Result<Response, String> {
    headed_wrapper::handle(cmd, client)
}

pub fn handle_pr(cmd: &PrCommand, client: &DaemonClient) -> Result<Response, String> {
    pr::handle(cmd, client)
}

/// Handle start command (no daemon required - it starts daemon/webserver services)
pub fn handle_start(
    project: Option<String>,
    repos: Vec<std::path::PathBuf>,
) -> Result<Response, String> {
    daemon::handle_start(project, repos)
}

/// Handle stop command (no daemon required - it stops the daemon)
pub fn handle_stop(keep_session: bool) -> Result<Response, String> {
    daemon::handle_stop(keep_session)
}

/// Handle restart command (stop + start)
pub fn handle_restart(force: bool) -> Result<Response, String> {
    daemon::handle_restart(force)
}

/// Handle view command (launches chat; optionally attaches Lead and opens a split)
pub fn handle_view(project: Option<&str>, attach: bool) -> Result<Response, String> {
    daemon::handle_view(project, attach)
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

/// Handle `midtown state <phase> [--task <id>] [--progress <0-100>] [--pr <number>]` — reports coworker state via daemon RPC.
///
/// Called explicitly by coworkers to report their workflow phase and optional progress.
/// Sends state to the daemon which stores it in memory and updates web UI status.
/// When `--pr` is provided, the daemon writes the PR number to `task.pr` for auto-completion on merge.
pub fn handle_state(
    phase: midtown::coworker_state::WorkflowPhase,
    task_id: Option<u32>,
    progress: Option<u8>,
    pr_number: Option<u64>,
) -> Result<Response, String> {
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "unknown".to_string());

    // Validate progress is in range 0-100
    if let Some(p) = progress
        && p > 100
    {
        return Err("Progress must be between 0 and 100".to_string());
    }

    // Convert phase to the snake_case string the RPC endpoint expects
    let phase_str = match phase {
        midtown::coworker_state::WorkflowPhase::Claiming => "claiming",
        midtown::coworker_state::WorkflowPhase::Developing => "developing",
        midtown::coworker_state::WorkflowPhase::Testing => "testing",
        midtown::coworker_state::WorkflowPhase::PullRequest => "pull_request",
        midtown::coworker_state::WorkflowPhase::Reviewing => "reviewing",
        midtown::coworker_state::WorkflowPhase::Debugging => "debugging",
        midtown::coworker_state::WorkflowPhase::Completed => "completed",
        midtown::coworker_state::WorkflowPhase::Idle => "idle",
    };

    let client = crate::client::DaemonClient::connect()
        .map_err(|_| "Daemon is not running. Start with: midtown".to_string())?;
    client.coworker_report_state(&agent, phase_str, task_id, progress, pr_number)
}

/// Handle hook commands (insight, idle, task, ask) - no daemon required
pub fn handle_hook(cmd: &HookCommand) -> Result<Response, String> {
    hooks::handle(cmd)
}

/// Handle diagram commands (validate) - no daemon required
pub fn handle_diagram(cmd: &DiagramCommand) -> Result<Response, String> {
    diagram::handle(cmd)
}

/// Handle E2E test commands (auth, run) - no daemon required
pub fn handle_e2e(cmd: &E2eCommand) -> Result<(), String> {
    e2e::handle(cmd)
}

/// Handle auth profile commands (login, list, switch, remove) - no daemon required
pub fn handle_auth(
    cmd: &AuthCommand,
    provider: midtown::auth::AuthProvider,
) -> Result<Response, String> {
    auth::handle(cmd, provider)
}

/// Handle `midtown auth list` (default behavior shows all providers).
pub fn handle_auth_list_all_providers() -> Result<Response, String> {
    auth::handle_list_all_providers()
}

/// Prompt the user to select a provider from all available providers.
pub fn prompt_provider_selection_all() -> Result<midtown::auth::AuthProvider, String> {
    auth::prompt_provider_selection_all()
}

/// Handle `midtown lead remind` subcommands
pub fn handle_remind(
    cmd: &crate::RemindCommand,
    client: &DaemonClient,
) -> Result<Response, String> {
    lead::handle_remind(cmd, client)
}

/// Handle `midtown lead` (no subcommand) — boot a headed lead session.
pub fn handle_lead_boot(channel: Option<&str>) -> Result<(), String> {
    lead::handle_lead_boot(channel)
}

/// Handle `midtown coworker` (no subcommand) — boot a headed coworker session.
pub fn handle_coworker_boot(task_id: Option<&str>) -> Result<(), String> {
    coworker::handle_coworker_boot(task_id)
}

/// Handle `midtown config` subcommands (get/set/list) — no daemon required.
pub fn handle_config(cmd: &ConfigCommand) -> Result<Response, String> {
    config::handle(cmd)
}

/// Handle `midtown update` — check for and install the latest release.
pub fn handle_update(check_only: bool) -> Result<Response, String> {
    update::handle_update(check_only)
}

/// Handle `midtown webserver stop` command
pub fn handle_webserver_stop() -> Result<Response, String> {
    daemon::handle_webserver_stop()
}

/// Handle `midtown webserver restart` command
pub fn handle_webserver_restart() -> Result<Response, String> {
    daemon::handle_webserver_restart()
}

/// Handle `midtown oneshot` command — execute a one-shot Claude Code session via the daemon.
pub fn handle_oneshot(
    client: &DaemonClient,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    json_schema: Option<&str>,
    max_budget_usd: Option<f64>,
    allow_tools: bool,
) -> Result<Response, String> {
    let schema = json_schema
        .map(|s| serde_json::from_str(s).map_err(|e| format!("Invalid JSON schema: {}", e)))
        .transpose()?;

    let result = client.oneshot_execute(
        prompt,
        model,
        system_prompt,
        schema,
        max_budget_usd,
        allow_tools,
    )?;

    // Extract the result text for display
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let result_text = result.get("result").and_then(|v| v.as_str()).unwrap_or("");
    let cost = result.get("cost_usd").and_then(|v| v.as_f64());
    let duration = result.get("duration_ms").and_then(|v| v.as_u64());

    if success {
        let mut message = result_text.to_string();
        if let Some(c) = cost {
            message.push_str(&format!("\n\n(cost: ${:.4}", c));
            if let Some(d) = duration {
                message.push_str(&format!(", duration: {}ms", d));
            }
            message.push(')');
        }
        Ok(Response::Message { message })
    } else {
        Err(format!("One-shot execution failed: {}", result_text))
    }
}
