//! CLI subcommands for `midtown session` — attach/detach headless coworker sessions.
//!
//! Allows the Lead to attach to a headless coworker's session in a tmux window
//! for interactive debugging/guidance, then detach to resume headless execution.

use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommand {
    /// Attach to a headless coworker's session in a tmux window
    Attach {
        /// Target to attach to (coworker name, or use task/pr subcommands)
        #[command(subcommand)]
        target: Option<AttachTarget>,
    },
    /// Detach from an attached session (resume headless execution)
    Detach {
        /// Name of the coworker to detach
        name: String,
    },
    /// List attachable headless sessions
    List,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AttachTarget {
    /// Attach by coworker name
    Name {
        /// Coworker name (e.g., park, madison)
        name: String,
    },
    /// Attach to the coworker working on a specific task
    Task {
        /// Task ID
        id: u32,
    },
    /// Attach to the coworker working on a specific PR
    Pr {
        /// PR number
        number: u64,
    },
}

pub fn handle(cmd: &SessionCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        SessionCommand::Attach { target } => handle_attach(target, client),
        SessionCommand::Detach { name } => client.session_detach(name),
        SessionCommand::List => client.session_list(),
    }
}

/// Handle attach: resolve target → call daemon RPC → create tmux window → wait for exit → detach.
fn handle_attach(target: &Option<AttachTarget>, client: &DaemonClient) -> Result<Response, String> {
    // Resolve the target to a string the daemon understands
    let target_str = match target {
        Some(AttachTarget::Name { name }) => format!("name:{}", name),
        Some(AttachTarget::Task { id }) => format!("task:{}", id),
        Some(AttachTarget::Pr { number }) => format!("pr:{}", number),
        None => {
            return Err("Usage: midtown session attach name <coworker-name>\n\
                        midtown session attach task <id>\n\
                        midtown session attach pr <number>"
                .to_string());
        }
    };

    // Step 1: Ask daemon to pause headless session and return session info
    let info = client.session_attach(&target_str)?;

    let session_id = info
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Daemon did not return session_id")?;
    let cwd = info
        .get("cwd")
        .and_then(|v| v.as_str())
        .ok_or("Daemon did not return cwd")?;
    let name = info
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Daemon did not return name")?;

    // Step 2: Create tmux window with `claude --resume <session-id>`
    let repo_name =
        midtown::paths::detect_repo_name().ok_or_else(|| "Not in a git repository".to_string())?;
    let tmux_session = format!("{}{}", midtown::process::SESSION_PREFIX, repo_name);

    // Window name uses ~ separator since : is used for tmux targets and
    // window_exists() splits on : for base name matching
    let window_name = format!("attach~{}", name);

    // Build the claude command for interactive use in tmux.
    // Uses --resume to pick up the exact session state.
    // Wrap with sandbox-exec on macOS for filesystem write restrictions.
    let config_dir = midtown::auth::active_profile_dir_for_project(&repo_name);
    let sandbox_config = midtown::config::get_project_sandbox_config(&repo_name);
    let writable = midtown::sandbox::writable_dirs(
        std::path::Path::new(cwd),
        &[],
        &sandbox_config.allowed_paths,
    );
    let claude_part = if cfg!(target_os = "macos") {
        match midtown::sandbox::sandbox_exec_prefix(&writable) {
            Ok((_profile_path, prefix)) => {
                let sb_args = prefix.join(" ");
                format!(
                    "sandbox-exec {} claude --resume {} --dangerously-skip-permissions",
                    sb_args, session_id
                )
            }
            Err(_) => format!(
                "claude --resume {} --dangerously-skip-permissions",
                session_id
            ),
        }
    } else {
        format!(
            "claude --resume {} --dangerously-skip-permissions",
            session_id
        )
    };
    let cmd = format!(
        "export CLAUDE_CONFIG_DIR='{}' MIDTOWN_AGENT='{}' DISABLE_AUTOUPDATER=1; exec {}",
        config_dir.display(),
        name,
        claude_part
    );

    midtown::tmux::create_window(&tmux_session, &window_name, cwd, Some(&cmd)).map_err(|e| {
        // If tmux window creation fails, tell daemon to resume headless.
        // Log the detach result explicitly so a double failure is visible.
        match client.session_detach(name) {
            Ok(_) => eprintln!(
                "Tmux window creation failed; headless session resumed for {}.",
                name
            ),
            Err(detach_err) => eprintln!(
                "ERROR: Tmux window creation failed AND detach RPC failed for {}.\n\
                 Tmux error: {}\n\
                 Detach error: {}\n\
                 Manual recovery: run `midtown session detach {}`",
                name, e, detach_err, name
            ),
        }
        format!("Failed to create tmux window: {}", e)
    })?;

    eprintln!(
        "Attached to {} in tmux window '{}'. \
         Session will resume headless when the window closes.",
        name, window_name
    );

    // Step 3: Monitor tmux window — when it closes, send detach RPC.
    // Spawn a background thread that polls for window existence.
    // Uses retry with exponential backoff to handle transient RPC failures.
    let name_owned = name.to_string();
    let session_clone = tmux_session.clone();
    let window_clone = window_name.clone();

    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if !midtown::tmux::window_exists(&session_clone, &window_clone).unwrap_or(false) {
                // Window closed — send detach with retry
                let max_retries = 3;
                let mut delay = std::time::Duration::from_secs(1);

                for attempt in 1..=max_retries {
                    match DaemonClient::connect().and_then(|c| c.session_detach(&name_owned)) {
                        Ok(_) => {
                            eprintln!("Detached {} — resuming headless session.", name_owned);
                            return;
                        }
                        Err(e) => {
                            if attempt < max_retries {
                                eprintln!(
                                    "Warning: Detach attempt {}/{} failed for {}: {}. Retrying in {:?}...",
                                    attempt, max_retries, name_owned, e, delay
                                );
                                std::thread::sleep(delay);
                                delay *= 2;
                            } else {
                                eprintln!(
                                    "ERROR: All {} detach attempts failed for {}. \
                                     Last error: {}. Manual recovery: run `midtown session detach {}`",
                                    max_retries, name_owned, e, name_owned
                                );
                            }
                        }
                    }
                }
                break;
            }
        }
    });

    Ok(Response::message(format!(
        "Attached to {} (session {}). Window: {}",
        name, session_id, window_name
    )))
}
