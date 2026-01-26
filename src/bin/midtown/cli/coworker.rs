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
    /// Nudge a coworker to check in
    Nudge {
        /// Name of the coworker to nudge
        name: String,
        /// Custom message (optional)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Configure nudging settings
    NudgeConfig {
        #[command(subcommand)]
        command: NudgeConfigCommand,
    },
    /// Handle Claude Code stop hook (checks for unclaimed tasks)
    StopHook,
    /// Link this session's tasks to the Lead's task directory (SessionStart hook)
    LinkTasks,
}

#[derive(Subcommand, Debug, Clone)]
pub enum NudgeConfigCommand {
    /// Show current nudge configuration
    Show,
    /// Set nudge interval (in seconds)
    Interval {
        /// Interval in seconds (0 to disable periodic nudging)
        seconds: u64,
    },
    /// Set nudge message template
    Template {
        /// Message template with {task} placeholder
        template: String,
    },
    /// Enable nudging
    Enable,
    /// Disable nudging
    Disable,
}

pub fn handle(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        CoworkerCommand::Spawn => client.coworker_spawn(),
        CoworkerCommand::Shutdown { name } => client.coworker_shutdown(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
        CoworkerCommand::NudgeConfig { command } => handle_nudge_config(command, client),
        CoworkerCommand::StopHook => handle_stop_hook_standalone(),
        CoworkerCommand::LinkTasks => handle_link_tasks_standalone(),
    }
}

/// Handle the stop hook for Claude Code (standalone, no daemon required).
///
/// This command is designed to be used as a Claude Code stop hook. It:
/// 1. Reads channel messages (syncs any pending messages)
/// 2. Checks for unclaimed tasks via `bd ready`
/// 3. Returns JSON to indicate whether Claude should continue or stop
///
/// If unclaimed tasks exist, returns `{"decision": "block", "reason": "..."}` to
/// prevent stopping and allow the coworker to pick up the next task.
pub fn handle_stop_hook_standalone() -> Result<Response, String> {
    // First, read channel messages to sync any pending updates
    // We do this silently - errors here shouldn't block the stop hook
    let _ = read_channel_messages();

    // Check for unclaimed tasks
    let unclaimed_count = count_unclaimed_tasks();

    if unclaimed_count > 0 {
        // There are unclaimed tasks - block stopping so coworker continues
        let reason = if unclaimed_count == 1 {
            "1 unclaimed task available".to_string()
        } else {
            format!("{} unclaimed tasks available", unclaimed_count)
        };

        Ok(Response::StopHookDecision {
            decision: "block".to_string(),
            reason,
        })
    } else {
        // No unclaimed tasks - allow stopping
        Ok(Response::StopHookDecision {
            decision: "".to_string(),
            reason: "No unclaimed tasks".to_string(),
        })
    }
}

/// Read channel messages silently (for stop hook sync).
fn read_channel_messages() -> Result<(), String> {
    // Try to detect repo and read channel
    if let Some(repo) = detect_git_repo() {
        let channel = midtown::Channel::for_repo(&repo)
            .map_err(|e| format!("Failed to open channel: {}", e))?;

        // Get agent name from environment or use default
        let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

        // Read new messages since cursor (advances cursor position)
        let _ = channel.read_since_cursor(&agent);
    }
    Ok(())
}

/// Count unclaimed tasks from the beads system.
fn count_unclaimed_tasks() -> usize {
    let output = std::process::Command::new("bd")
        .args(["ready", "--json"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(tasks) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                // Filter to tasks that don't have an owner/assignee
                tasks
                    .iter()
                    .filter(|task| {
                        let owner = task.get("owner").and_then(|o| o.as_str());
                        let assignee = task.get("assignee").and_then(|a| a.as_str());
                        owner.is_none() && assignee.is_none()
                    })
                    .count()
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Try to detect the current git repository name.
fn detect_git_repo() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                std::path::Path::new(path.trim())
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
}

fn handle_nudge_config(
    cmd: &NudgeConfigCommand,
    client: &DaemonClient,
) -> Result<Response, String> {
    match cmd {
        NudgeConfigCommand::Show => client.nudge_config_show(),
        NudgeConfigCommand::Interval { seconds } => client.nudge_config_interval(*seconds),
        NudgeConfigCommand::Template { template } => client.nudge_config_template(template),
        NudgeConfigCommand::Enable => client.nudge_config_enable(true),
        NudgeConfigCommand::Disable => client.nudge_config_enable(false),
    }
}

/// Link this coworker's task directory to the Lead's.
///
/// Called by SessionStart hook to share tasks across sessions.
pub fn handle_link_tasks_standalone() -> Result<Response, String> {
    use std::fs;
    use std::os::unix::fs::symlink;

    // Get repo name to find Lead's session file
    let repo = detect_git_repo().ok_or("Not in a git repository")?;

    // Read Lead's session UUID from ~/.midtown/<repo>/lead-session
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let lead_session_file = home.join(".midtown").join(&repo).join("lead-session");

    let lead_uuid = fs::read_to_string(&lead_session_file)
        .map_err(|_| {
            format!(
                "Lead session not found at {:?}. Is midtown running?",
                lead_session_file
            )
        })?
        .trim()
        .to_string();

    // Find this session's task directory (newest in ~/.claude/tasks/)
    let tasks_dir = home.join(".claude").join("tasks");
    let my_uuid = find_newest_task_dir(&tasks_dir)?;

    // Don't link to ourselves
    if my_uuid == lead_uuid {
        return Ok(Response::Message {
            message: "Already using Lead's task directory".to_string(),
        });
    }

    let my_task_dir = tasks_dir.join(&my_uuid);
    let lead_task_dir = tasks_dir.join(&lead_uuid);

    // Verify Lead's task dir exists
    if !lead_task_dir.exists() {
        return Err(format!(
            "Lead's task directory not found: {:?}",
            lead_task_dir
        ));
    }

    // Remove our task dir and replace with symlink
    if my_task_dir.exists() {
        fs::remove_dir_all(&my_task_dir)
            .map_err(|e| format!("Failed to remove task directory: {}", e))?;
    }

    symlink(&lead_task_dir, &my_task_dir)
        .map_err(|e| format!("Failed to create symlink: {}", e))?;

    Ok(Response::Message {
        message: format!("Linked tasks: {} -> {}", my_uuid, lead_uuid),
    })
}

/// Find the most recently created directory in the given path.
fn find_newest_task_dir(tasks_dir: &std::path::Path) -> Result<String, String> {
    use std::fs;

    let entries: Vec<_> = fs::read_dir(tasks_dir)
        .map_err(|e| format!("Cannot read tasks directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    entries
        .iter()
        .max_by_key(|e| e.metadata().and_then(|m| m.created()).ok())
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        .ok_or_else(|| "No task directories found".to_string())
}
