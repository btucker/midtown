//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::Response;

/// Get the tmux session name based on the project name.
/// Format: midtown-{project_name}
///
/// The project name is resolved from config.toml `[project].name`,
/// falling back to the git repo directory name.
///
/// Returns an error if not in a git repository, since a tmux session
/// requires a valid project context.
fn session_name() -> Result<String, String> {
    let project_name = midtown::paths::detect_project_name().or_else(|| {
        repo_root()
            .ok()
            .and_then(|r| r.file_name().map(|s| s.to_string_lossy().to_string()))
    });

    match project_name {
        Some(name) => Ok(format!("midtown-{}", name)),
        None => Err(
            "Not in a git repository. Run midtown from within a git repo or use --repo."
                .to_string(),
        ),
    }
}

/// Get the socket path for the daemon.
fn socket_path() -> PathBuf {
    midtown::paths::daemon_socket()
}

/// Check if the daemon is running by attempting to connect to its socket.
fn daemon_is_running() -> bool {
    let path = socket_path();
    if !path.exists() {
        return false;
    }
    // Try to connect - if successful, daemon is running
    UnixStream::connect(&path).is_ok()
}

/// Wait for the daemon socket to become available with retries.
///
/// Polls the socket every `interval_ms` milliseconds, up to `max_attempts` times.
/// Returns true if the socket became available, false if we timed out.
fn wait_for_daemon_socket(max_attempts: u32, interval_ms: u64) -> bool {
    for _ in 0..max_attempts {
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        if daemon_is_running() {
            return true;
        }
    }
    false
}

/// Clean up stale daemon state before starting a new daemon.
///
/// This handles the case where a previous daemon crashed or was killed
/// without cleaning up its PID file. It reads the PID file, checks if
/// that process is still running, and kills it if so.
fn cleanup_stale_daemon() {
    let pid_path = midtown::paths::daemon_pid_file();

    // Read the PID file if it exists
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => return, // No PID file, nothing to clean up
    };

    // Parse the PID
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            // Invalid PID file, remove it
            let _ = std::fs::remove_file(&pid_path);
            return;
        }
    };

    // Check if the process is still running using kill -0 (suppress stderr)
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status();

    if status.map(|s| s.success()).unwrap_or(false) {
        // Process is still running, try to kill it gracefully
        eprintln!("Cleaning up stale daemon process (PID {})", pid);
        let _ = Command::new("kill")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status();

        // Wait briefly for it to exit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Force kill if still running
        let still_running = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if still_running {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
    }

    // Clean up stale files
    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(socket_path());
}

/// Check if the project's tmux session exists.
fn session_exists(session: &str) -> bool {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Find the git repository root by walking up the directory tree.
///
/// Looks for a `.git` directory or file (worktrees use a file) starting
/// from the current directory and climbing up until found or hitting root.
fn find_git_root() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;

    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            // Found it - could be a directory (normal repo) or file (worktree)
            return Some(current);
        }

        // Try parent directory
        if !current.pop() {
            // Reached filesystem root
            return None;
        }
    }
}

/// Get the repository root directory.
///
/// Uses git rev-parse for accuracy (handles worktrees correctly),
/// but falls back to manual detection if git command fails.
fn repo_root() -> Result<PathBuf, String> {
    // First, check if we're even in a git repo by walking up
    if find_git_root().is_none() {
        return Err(
            "Not in a git repository. Run midtown from within a git repo, or use --repo <path>."
                .to_string(),
        );
    }

    // Use git rev-parse for accurate root detection (handles worktrees)
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(
            "Not in a git repository. Run midtown from within a git repo, or use --repo <path>."
                .to_string(),
        );
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();

    Ok(PathBuf::from(path))
}

/// Get the state directory for midtown.
fn state_dir() -> PathBuf {
    let state_dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        });
    state_dir.join("midtown")
}

/// Write the Lead system prompt to a file and return the path.
fn write_lead_prompt_file() -> Result<PathBuf, String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory: {}", e))?;

    let path = dir.join("lead-prompt.md");
    std::fs::write(&path, midtown::agents::lead_system_prompt())
        .map_err(|e| format!("Failed to write lead prompt file: {}", e))?;

    Ok(path)
}

/// Generate Lead settings JSON with hooks for channel sync, insights, and orphan detection.
fn lead_settings_json() -> serde_json::Value {
    let bin_command = midtown::config::get_bin_command();
    serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} hook lead-stop", bin_command)
                }]
            }],
            "PostToolUse": [{
                // No matcher = runs on every tool use for insight posting
                "hooks": [{
                    "type": "command",
                    "command": format!("{} hook insight", bin_command)
                }]
            }]
        }
    })
}

/// Write Lead settings to a file and return the path.
fn write_lead_settings_file() -> Result<PathBuf, String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory: {}", e))?;

    let path = dir.join("lead-settings.json");
    let settings = lead_settings_json();
    std::fs::write(&path, settings.to_string())
        .map_err(|e| format!("Failed to write lead settings file: {}", e))?;

    Ok(path)
}

/// Get the path to the Lead session ID file for a project.
fn lead_session_file(repo: &Path) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s: &std::ffi::OsStr| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    midtown::paths::lead_session_file_for_repo(&repo_name)
}

/// Build the claude command for the Lead session.
///
/// Returns the full command string to launch Claude Code with appropriate flags.
/// Always includes the system prompt via --append-system-prompt, whether new or resuming.
/// Also includes settings file with stop hook for channel sync.
/// Sets CLAUDE_CODE_TASK_LIST_ID so Lead shares tasks with coworkers.
fn build_lead_claude_command(
    session_id: &str,
    is_existing: bool,
    task_list_id: &str,
) -> Result<String, String> {
    let prompt_file = write_lead_prompt_file()?;
    let settings_file = write_lead_settings_file()?;

    if is_existing {
        // Resume existing session, but still inject system prompt and settings
        Ok(format!(
            "export CLAUDE_CODE_TASK_LIST_ID='{}'; claude --dangerously-skip-permissions --resume {} --settings {} --append-system-prompt \"$(cat {})\"",
            task_list_id,
            session_id,
            settings_file.display(),
            prompt_file.display()
        ))
    } else {
        // New session: use specific session ID, settings, and inject system prompt
        Ok(format!(
            "export CLAUDE_CODE_TASK_LIST_ID='{}'; claude --dangerously-skip-permissions --session-id {} --settings {} --append-system-prompt \"$(cat {})\"",
            task_list_id,
            session_id,
            settings_file.display(),
            prompt_file.display()
        ))
    }
}

/// Get or create the Lead session ID for a project.
///
/// If a session ID exists, returns it. Otherwise generates a new UUID and stores it.
fn get_or_create_lead_session_id(repo: &Path) -> Result<(String, bool), String> {
    let session_file = lead_session_file(repo);

    // Try to read existing session ID
    if session_file.exists() {
        let session_id = std::fs::read_to_string(&session_file)
            .map_err(|e| format!("Failed to read session ID: {}", e))?
            .trim()
            .to_string();
        if !session_id.is_empty() {
            return Ok((session_id, true)); // true = existing session
        }
    }

    // Generate new session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Ensure directory exists
    if let Some(parent) = session_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create session directory: {}", e))?;
    }

    // Store the session ID
    std::fs::write(&session_file, &session_id)
        .map_err(|e| format!("Failed to write session ID: {}", e))?;

    Ok((session_id, false)) // false = new session
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates tmux session for the project
/// 3. Launches Claude Code with Lead config in that session
pub fn handle_start(daemon_only: bool) -> Result<Response, String> {
    // Verify we're in a git repo first
    let repo = repo_root()?;
    let session = session_name()?;

    let mut messages = Vec::new();

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
    } else {
        // Clean up any stale PID file or orphaned daemon before starting
        cleanup_stale_daemon();

        // Start the daemon in the background using `midtown daemon`
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable: {}", e))?;

        let mut cmd = Command::new(&exe);
        cmd.arg("daemon");
        cmd.current_dir(&repo);
        cmd.arg("--workdir").arg(&repo);

        // Spawn detached
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        cmd.spawn()
            .map_err(|e| format!("Failed to start daemon: {}", e))?;

        // Wait for daemon to start, polling the socket with retries
        let started = wait_for_daemon_socket(5, 200);

        if started {
            messages.push("Started daemon".to_string());
        } else {
            return Err("Daemon failed to start".to_string());
        }
    }

    // Step 2: Launch tmux session (unless --daemon-only)
    if daemon_only {
        messages.push("Skipping tmux session (--daemon-only)".to_string());
    } else if session_exists(&session) {
        messages.push(format!("Session '{}' already exists", session));
    } else {
        // Get or create the Lead's persistent session ID
        let (lead_session_id, is_existing) = get_or_create_lead_session_id(&repo)?;

        // Get the shared task list ID for this repo
        let repo_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        let task_list_id = midtown::paths::task_list_id_for_repo(&repo_name);

        // Build the claude command (always includes system prompt)
        let claude_cmd = build_lead_claude_command(&lead_session_id, is_existing, &task_list_id)?;

        // Get project name for status bar (uppercase)
        let project_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "PROJECT".to_string());

        // Create tmux session with claude command directly
        // -n sets the window name to "lead"
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-n",
                "lead",
                "-c",
                &repo.to_string_lossy(),
                "sh",
                "-c",
                &claude_cmd,
            ])
            .status()
            .map_err(|e| format!("Failed to create session: {}", e))?;

        if !status.success() {
            return Err(format!("Failed to create session '{}'", session));
        }

        // Configure status bar with dark gray background and yellow foreground (Lead's color)
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "status-style",
                "bg=colour236,fg=yellow",
            ])
            .status();

        // Set status-left with project name
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "status-left",
                &format!(" {} ", project_name),
            ])
            .status();

        // Set terminal title to "Midtown: <project>" instead of showing the command
        let _ = Command::new("tmux")
            .args(["set-option", "-t", &session, "set-titles", "on"])
            .status();
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "set-titles-string",
                &format!("Midtown: {}", project_name),
            ])
            .status();

        // Set Lead window tab color (yellow to match chat TUI team panel)
        let lead_window = format!("{}:lead", session);
        let _ = Command::new("tmux")
            .args([
                "set-window-option",
                "-t",
                &lead_window,
                "window-status-style",
                "fg=yellow",
            ])
            .status();
        let _ = Command::new("tmux")
            .args([
                "set-window-option",
                "-t",
                &lead_window,
                "window-status-current-style",
                "fg=yellow,bold",
            ])
            .status();

        // Set up hook to update status bar color based on active window
        let _ = midtown::tmux::setup_status_bar_hook(&session);

        // Set up chat TUI based on layout configuration
        let bin_command = midtown::config::get_bin_command();
        let (chat_layout, chat_min_width) = midtown::config::get_chat_layout();

        // Determine whether to use split or window layout
        let use_split = match chat_layout {
            midtown::config::ChatLayout::Split => true,
            midtown::config::ChatLayout::Window => false,
            midtown::config::ChatLayout::Auto => {
                // Check terminal width and use split if wide enough
                midtown::tmux::get_session_width(&session)
                    .map(|w| w >= chat_min_width)
                    .unwrap_or(true) // Default to split if can't determine width
            }
        };

        if use_split {
            // Split layout: chat pane on the right (30% width)
            if let Err(e) = midtown::tmux::create_chat_split(&session, &bin_command) {
                eprintln!("Warning: Failed to create chat split: {}", e);
            }
        } else {
            // Window layout: chat in separate window
            if let Err(e) = midtown::tmux::create_chat_window(&session, &bin_command) {
                eprintln!("Warning: Failed to create chat window: {}", e);
            }
        }

        // Write marker file indicating Lead was initialized by midtown
        let marker_path = lead_initialized_marker(&repo);
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

        if is_existing {
            messages.push(format!("Resumed Lead session in '{}'", session));
        } else {
            messages.push(format!("Started new Lead session in '{}'", session));
        }
    }

    // Build response message
    let attach_hint = "Attach with: midtown attach".to_string();
    messages.push(attach_hint);

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Handle `midtown stop` command.
///
/// Stops the daemon and optionally the tmux session.
pub fn handle_stop(keep_session: bool) -> Result<Response, String> {
    let mut messages = Vec::new();

    // Get session name (if in a git repo)
    if let Ok(session) = session_name() {
        // Stop tmux session (unless --keep-session)
        if !keep_session && session_exists(&session) {
            let status = Command::new("tmux")
                .args(["kill-session", "-t", &session])
                .status()
                .map_err(|e| format!("Failed to kill session: {}", e))?;

            if status.success() {
                messages.push(format!("Stopped session '{}'", session));
            } else {
                messages.push(format!("Warning: Failed to stop session '{}'", session));
            }
        } else if session_exists(&session) {
            messages.push(format!(
                "Keeping session '{}' (use without --keep-session to stop)",
                session
            ));
        }
    }

    // Step 2: Stop daemon
    if daemon_is_running() {
        // Read the PID and send SIGTERM for a clean shutdown
        let pid_path = midtown::paths::daemon_pid_file();
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
            && let Ok(pid) = pid_str.trim().parse::<u32>()
        {
            // Send SIGTERM for graceful shutdown
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stderr(Stdio::null())
                .status();

            // Wait briefly for daemon to exit and clean up
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        // Also remove socket file as a fallback
        let path = socket_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        messages.push("Stopped daemon".to_string());
    } else {
        messages.push("Daemon was not running".to_string());
    }

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Handle `midtown restart` command.
///
/// Gracefully restarts the daemon while preserving the tmux session and all
/// running Claude processes (Lead and coworkers). Only the daemon process is
/// restarted, which allows updating daemon code without losing work in progress.
/// Also restarts the chat pane to pick up any code changes.
///
/// For a full fresh start, use `midtown stop && midtown start`.
pub fn handle_restart() -> Result<Response, String> {
    // Stop daemon only, keep the tmux session running
    let _ = handle_stop(true);

    // Brief pause for cleanup
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Start daemon (session already exists, so it will re-discover coworkers)
    let result = handle_start(false)?;

    // Restart the chat pane to pick up code changes.
    // Use respawn-pane -k to atomically kill the old process and start a new
    // one, avoiding a race where send-keys characters (like 'i' in "midtown")
    // are intercepted by the still-running TUI's input mode handler.
    if let Ok(session) = session_name() {
        let chat_pane = format!("{}:lead.1", session);
        let bin_command = midtown::config::get_bin_command();
        let chat_cmd = format!("{} chat", bin_command);
        let _ = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &chat_pane, &chat_cmd])
            .status();
    }

    // Enhance the message to clarify graceful restart
    match result {
        Response::Message { message } => Ok(Response::Message {
            message: format!(
                "{}. Resumed Lead session in '{}'. Attach with: midtown attach",
                message,
                session_name().unwrap_or_else(|_| "midtown".to_string())
            ),
        }),
        other => Ok(other),
    }
}

/// Clear stale Lead session ID file for a project.
///
/// When no tmux session is running, the session-id file from a previous
/// session is stale. Removing it ensures a fresh Claude Code session
/// is created instead of resuming the old one.
fn clear_stale_lead_session(repo: &Path) {
    let session_file = lead_session_file(repo);
    if session_file.exists() {
        let _ = std::fs::remove_file(&session_file);
    }
}

/// Handle `midtown attach` command.
///
/// Attaches to the project's tmux session.
/// If the session doesn't exist, it is automatically created first.
/// If the session exists but Lead wasn't started with midtown settings, reinitialize it.
pub fn handle_attach(project: Option<&str>) -> Result<Response, String> {
    let session = match project {
        // Explicit project name: construct session name directly
        Some(name) => format!("midtown-{}", name),
        // No project: infer from cwd
        None => session_name()?,
    };

    // For cwd-based attach, we can get the repo root for stale session cleanup
    let repo = if project.is_none() {
        repo_root().ok()
    } else {
        None
    };

    // Auto-create session if it doesn't exist
    if !session_exists(&session) {
        if project.is_some() {
            // Named project: don't auto-create, just error
            return Err(format!(
                "No tmux session '{}' found. Start the project first with 'midtown start'.",
                session
            ));
        }

        // No active tmux session means Lead is not running.
        // Clear any stale session-id file so we start a fresh
        // Claude Code session instead of resuming the old one.
        if let Some(ref repo) = repo {
            clear_stale_lead_session(repo);
        }

        // Start midtown (daemon + tmux session)
        handle_start(false)?;

        // Wait briefly for the session to be ready
        std::thread::sleep(std::time::Duration::from_millis(200));
    } else if let Some(ref repo) = repo {
        // Session exists - ensure Lead has proper settings
        ensure_lead_has_settings(&session, repo)?;
    }

    // Execute tmux attach - this replaces the current process
    let err = Command::new("tmux").args(["attach", "-t", &session]).exec();

    // If we get here, exec failed
    Err(format!("Failed to attach to session: {}", err))
}

/// List all known projects and their running status.
pub fn handle_project_list() -> Result<Response, String> {
    let projects_dir = midtown::paths::midtown_base_dir().join("projects");

    if !projects_dir.exists() {
        return Ok(Response::message("No projects found."));
    }

    let mut entries: Vec<_> = std::fs::read_dir(&projects_dir)
        .map_err(|e| format!("Failed to read projects directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Ok(Response::message("No projects found."));
    }

    let mut lines = Vec::new();
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let pid_file = entry.path().join("daemon.pid");

        // Check if daemon is running by testing PID file lock
        let status = if pid_file.exists() {
            match is_daemon_running(&pid_file) {
                true => "running",
                false => "stopped",
            }
        } else {
            "stopped"
        };

        // Check tmux session
        let session = format!("midtown-{}", name);
        let has_session = session_exists(&session);

        let status_display = if status == "running" && has_session {
            "running"
        } else if status == "running" {
            "daemon only"
        } else {
            "stopped"
        };

        lines.push(format!("{:<20} {}", name, status_display));
    }

    Ok(Response::message(lines.join("\n")))
}

/// Check if the daemon is running by testing the PID file lock.
fn is_daemon_running(pid_file: &Path) -> bool {
    use std::fs::OpenOptions;

    let file = match OpenOptions::new().read(true).open(pid_file) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // Try to get an exclusive lock. If we can't, the daemon holds it.
    use fs2::FileExt;
    match file.try_lock_exclusive() {
        Ok(_) => {
            // We got the lock => daemon is NOT running
            let _ = file.unlock();
            false
        }
        Err(_) => {
            // Lock held => daemon IS running
            true
        }
    }
}

/// Ensure the Lead pane has proper midtown settings.
/// Checks for a marker file; if missing, restarts Claude with settings.
fn ensure_lead_has_settings(session: &str, repo: &Path) -> Result<(), String> {
    let marker_path = lead_initialized_marker(repo);

    // Check if Lead was properly initialized
    if marker_path.exists() {
        // Check marker version matches current
        let marker_version = std::fs::read_to_string(&marker_path).unwrap_or_default();
        if marker_version.trim() == env!("CARGO_PKG_VERSION") {
            return Ok(()); // Already initialized with current version
        }
    }

    // Need to reinitialize Lead with proper settings
    eprintln!("Reinitializing Lead with midtown settings...");

    // Get the Lead session ID
    let (lead_session_id, is_existing) = get_or_create_lead_session_id(repo)?;

    // Get the shared task list ID for this repo
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let task_list_id = midtown::paths::task_list_id_for_repo(&repo_name);

    // Build the claude command with settings
    let claude_cmd = build_lead_claude_command(&lead_session_id, is_existing, &task_list_id)?;

    // Kill the current Lead pane content and restart with proper settings
    let lead_pane = format!("{}:lead.0", session);

    // Send Ctrl-C to interrupt any running process, then exit
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &lead_pane, "C-c"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send exit command to close any shell
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &lead_pane, "exit", "Enter"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Respawn the pane with the proper claude command
    let _ = Command::new("tmux")
        .args([
            "respawn-pane",
            "-k",
            "-t",
            &lead_pane,
            "sh",
            "-c",
            &claude_cmd,
        ])
        .status();

    // Write the marker file
    if let Some(parent) = marker_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

    Ok(())
}

/// Path to the marker file indicating Lead was initialized by midtown.
fn lead_initialized_marker(repo: &Path) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    midtown::paths::lead_dir_for_repo(&repo_name).join("lead-initialized")
}

/// Get session status for status command enhancement.
#[allow(dead_code)]
pub fn get_session_status() -> (bool, bool) {
    let exists = session_name().map(|s| session_exists(&s)).unwrap_or(false);
    (daemon_is_running(), exists)
}

/// Handle `midtown lead register-session` command.
///
/// Detects the Lead's Claude Code session UUID and saves it so coworkers
/// can link their task directories to share tasks.
pub fn handle_register_session() -> Result<Response, String> {
    use std::fs;

    let repo = repo_root()?;
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    // Find the Lead's session UUID (most recently modified task directory)
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let tasks_dir = home.join(".claude").join("tasks");

    if !tasks_dir.exists() {
        return Err(
            "No Claude Code task directories found. Create a task first with TaskCreate."
                .to_string(),
        );
    }

    let lead_uuid = find_newest_dir(&tasks_dir)?;

    // Save to ~/.midtown/lead/<repo>/session-id
    let lead_dir = midtown::paths::lead_dir_for_repo(&repo_name);
    fs::create_dir_all(&lead_dir).map_err(|e| format!("Failed to create lead directory: {}", e))?;

    let session_file = midtown::paths::lead_session_file_for_repo(&repo_name);
    fs::write(&session_file, &lead_uuid)
        .map_err(|e| format!("Failed to write session file: {}", e))?;

    Ok(Response::Message {
        message: format!("Registered Lead session: {}", lead_uuid),
    })
}

/// Find the most recently modified directory in the given path.
fn find_newest_dir(dir: &std::path::Path) -> Result<String, String> {
    use std::fs;

    let entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("Cannot read directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && !e.path().is_symlink())
        .collect();

    if entries.is_empty() {
        return Err("No directories found".to_string());
    }

    entries
        .iter()
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        .ok_or_else(|| "Failed to determine newest directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Mutex to serialize tests that change CWD
    static CWD_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to create a fake git repo in a temp directory
    fn create_git_repo(dir: &std::path::Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
    }

    /// Helper to create a git worktree marker (file instead of directory)
    fn create_git_worktree(dir: &std::path::Path) {
        fs::write(
            dir.join(".git"),
            "gitdir: /some/other/path/.git/worktrees/foo",
        )
        .unwrap();
    }

    /// Canonicalize path to handle macOS /var -> /private/var symlink
    fn canonical(path: &std::path::Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    /// Run a test with a temporary working directory, restoring CWD after
    fn with_temp_cwd<F, T>(temp_path: &std::path::Path, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _lock = CWD_MUTEX.lock().unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();
        let result = f();
        std::env::set_current_dir(original_dir).unwrap();
        result
    }

    #[test]
    fn test_find_git_root_in_repo_root() {
        let temp = TempDir::new().unwrap();
        create_git_repo(temp.path());

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_some());
            assert_eq!(canonical(&result.unwrap()), canonical(temp.path()));
        });
    }

    #[test]
    fn test_find_git_root_in_subdirectory() {
        let temp = TempDir::new().unwrap();
        create_git_repo(temp.path());

        // Create nested subdirectory
        let subdir = temp.path().join("src").join("lib").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        with_temp_cwd(&subdir, || {
            let result = find_git_root();
            assert!(result.is_some());
            // Just verify .git exists at the found root
            let found_root = result.unwrap();
            assert!(found_root.join(".git").exists());
        });
    }

    #[test]
    fn test_find_git_root_with_worktree() {
        let temp = TempDir::new().unwrap();
        // Worktrees have a .git file instead of directory
        create_git_worktree(temp.path());

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_some());
            assert_eq!(canonical(&result.unwrap()), canonical(temp.path()));
        });
    }

    #[test]
    fn test_find_git_root_not_in_repo() {
        let temp = TempDir::new().unwrap();
        // Don't create .git

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_session_name_in_fake_repo() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("my-project");
        fs::create_dir_all(&repo_dir).unwrap();
        create_git_repo(&repo_dir);

        with_temp_cwd(&repo_dir, || {
            // find_git_root should work, but session_name uses git rev-parse
            // which won't work in a fake repo. Just test find_git_root here.
            let result = find_git_root();
            assert!(result.is_some());
        });
    }

    #[test]
    fn test_session_name_not_in_repo() {
        let temp = TempDir::new().unwrap();
        // Don't create .git

        with_temp_cwd(temp.path(), || {
            let result = session_name();
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("Not in a git repository"));
        });
    }

    #[test]
    fn test_repo_root_not_in_repo() {
        let temp = TempDir::new().unwrap();

        with_temp_cwd(temp.path(), || {
            let result = repo_root();
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.contains("Not in a git repository"));
            assert!(err.contains("--repo"));
        });
    }

    #[test]
    fn test_socket_path_format() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("midtown"));
        assert!(path.to_string_lossy().ends_with("daemon.sock"));
    }

    #[test]
    fn test_state_dir_format() {
        let dir = state_dir();
        assert!(dir.to_string_lossy().contains("midtown"));
    }

    // Note: lead_system_prompt tests moved to src/agents.rs

    #[test]
    fn test_session_exists_nonexistent() {
        // Random session name that definitely doesn't exist
        assert!(!session_exists("midtown-nonexistent-test-session-12345"));
    }

    #[test]
    fn test_get_or_create_lead_session_id_creates_new() {
        let temp = TempDir::new().unwrap();
        // Use a unique name with UUID to avoid conflicts between parallel tests
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // First call should create a new session ID
        let (session_id, is_existing) = get_or_create_lead_session_id(&repo_path).unwrap();

        // Clean up the session file we created in ~/.midtown/
        let session_file = lead_session_file(&repo_path);
        let _ = std::fs::remove_file(&session_file);
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(!is_existing);
        assert!(!session_id.is_empty());

        // Verify it's a valid UUID format (36 chars with hyphens)
        assert_eq!(session_id.len(), 36);
        assert!(session_id.contains('-'));
    }

    #[test]
    fn test_get_or_create_lead_session_id_returns_existing() {
        let temp = TempDir::new().unwrap();
        // Use a unique name with UUID to avoid conflicts between parallel tests
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // First call creates
        let (session_id_1, is_existing_1) = get_or_create_lead_session_id(&repo_path).unwrap();
        assert!(!is_existing_1);

        // Second call should return the same ID
        let (session_id_2, is_existing_2) = get_or_create_lead_session_id(&repo_path).unwrap();

        // Clean up the session file we created in ~/.midtown/
        let session_file = lead_session_file(&repo_path);
        let _ = std::fs::remove_file(&session_file);
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(is_existing_2);
        assert_eq!(session_id_1, session_id_2);
    }

    #[test]
    fn test_lead_session_file_path() {
        let repo_path = PathBuf::from("/tmp/my-project");
        let session_file = lead_session_file(&repo_path);

        assert!(session_file.to_string_lossy().contains(".midtown"));
        assert!(session_file.to_string_lossy().contains("lead"));
        assert!(session_file.to_string_lossy().contains("my-project"));
        assert!(session_file.to_string_lossy().ends_with("session-id"));
    }

    #[test]
    fn test_build_lead_claude_command_resume_includes_system_prompt() {
        // Bug: When resuming, the system prompt was not being included
        // The command should ALWAYS include --append-system-prompt
        let session_id = "test-session-123";
        let is_existing = true; // Resuming an existing session
        let task_list_id = "midtown-test";

        let cmd = build_lead_claude_command(session_id, is_existing, task_list_id).unwrap();

        // Must include system prompt even when resuming
        assert!(
            cmd.contains("--append-system-prompt"),
            "Resume command must include --append-system-prompt, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_new_includes_system_prompt() {
        let session_id = "test-session-456";
        let is_existing = false; // New session
        let task_list_id = "midtown-test";

        let cmd = build_lead_claude_command(session_id, is_existing, task_list_id).unwrap();

        assert!(
            cmd.contains("--append-system-prompt"),
            "New session command must include --append-system-prompt, got: {}",
            cmd
        );
        assert!(
            cmd.contains("--session-id"),
            "New session command must include --session-id, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_resume_uses_resume_flag() {
        let session_id = "test-session-789";
        let is_existing = true;
        let task_list_id = "midtown-test";

        let cmd = build_lead_claude_command(session_id, is_existing, task_list_id).unwrap();

        assert!(
            cmd.contains("--resume"),
            "Resume command must include --resume flag, got: {}",
            cmd
        );
    }

    #[test]
    fn test_handle_start_clears_stale_session_id() {
        // Bug: When no tmux session is running but a session-id file exists
        // from a previous session, handle_start would resume the old session
        // instead of creating a new one.
        //
        // After the fix, handle_start (when called without an active tmux session)
        // should delete the stale session-id file so a fresh session is created.
        let temp = TempDir::new().unwrap();
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // Simulate a stale session-id file from a previous session
        let session_file = lead_session_file(&repo_path);
        if let Some(parent) = session_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&session_file, "old-session-id-12345").unwrap();
        assert!(session_file.exists());

        // Call clear_stale_lead_session (the function handle_attach uses
        // when no tmux session exists)
        clear_stale_lead_session(&repo_path);

        // The session file should be deleted
        assert!(
            !session_file.exists(),
            "Stale session-id file should be deleted when no tmux session is running"
        );

        // Now get_or_create should create a NEW session
        let (session_id, is_existing) = get_or_create_lead_session_id(&repo_path).unwrap();

        // Clean up
        let _ = std::fs::remove_file(&session_file);
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(!is_existing, "Should create a new session, not resume");
        assert_ne!(session_id, "old-session-id-12345");
    }

    #[test]
    fn test_build_lead_claude_command_includes_task_list_id() {
        let session_id = "test-session-abc";
        let is_existing = false;
        let task_list_id = "midtown-myrepo";

        let cmd = build_lead_claude_command(session_id, is_existing, task_list_id).unwrap();

        assert!(
            cmd.contains("CLAUDE_CODE_TASK_LIST_ID='midtown-myrepo'"),
            "Command must set CLAUDE_CODE_TASK_LIST_ID, got: {}",
            cmd
        );
    }
}
