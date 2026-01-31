//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::Response;

/// Validate that a project name contains only safe characters.
///
/// Allowed: alphanumeric, hyphens, underscores, dots.
/// This prevents shell injection when the name is embedded in shell commands
/// (e.g., `export CLAUDE_CODE_TASK_LIST_ID='midtown-{name}'`).
fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(format!(
            "Invalid project name '{}': only alphanumeric characters, hyphens, underscores, and dots are allowed",
            name
        ));
    }
    Ok(())
}

/// Resolve the project name from an explicit flag, config, or repo directory name.
///
/// Priority: explicit flag > config.toml `[project].name` > git repo directory name.
/// Validates user-provided names to prevent shell injection.
fn resolve_project_name(project: &Option<String>) -> Option<String> {
    if let Some(name) = project {
        return Some(name.clone());
    }
    midtown::paths::detect_project_name().or_else(|| {
        repo_root()
            .ok()
            .and_then(|r| r.file_name().map(|s| s.to_string_lossy().to_string()))
    })
}

/// Get the tmux session name for an explicit or inferred project.
/// Format: midtown-{project_name}
///
/// Returns an error if no project name can be determined.
fn session_name_for(project: &Option<String>) -> Result<String, String> {
    match resolve_project_name(project) {
        Some(name) => Ok(format!("midtown-{}", name)),
        None => Err(
            "Not in a git repository. Run midtown from within a git repo or use --repo."
                .to_string(),
        ),
    }
}

/// Get the tmux session name based on the project name.
/// Format: midtown-{project_name}
///
/// The project name is resolved from config.toml `[project].name`,
/// falling back to the git repo directory name.
///
/// Returns an error if not in a git repository, since a tmux session
/// requires a valid project context.
fn session_name() -> Result<String, String> {
    session_name_for(&None)
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

/// Resolve additional repo paths from CLI flags, falling back to saved config.
///
/// If repos are provided on the CLI, they are returned directly.
/// Otherwise, reads saved repos from the project's config.toml.
fn resolve_repos(repos: &[PathBuf], project_name: &str) -> Vec<PathBuf> {
    if !repos.is_empty() {
        return repos.to_vec();
    }
    parse_saved_repos(project_name)
}

/// Parse saved repos from a project's config.toml.
///
/// Reads the `[project].repos` list and returns all entries
/// except the primary repo (which is handled separately).
fn parse_saved_repos(project_name: &str) -> Vec<PathBuf> {
    let full_config = midtown::config::load_full_project_config(project_name);
    match full_config {
        Some(config) => {
            let primary = config.project.primary_repo().map(|s| s.to_string());
            config
                .project
                .repos()
                .into_iter()
                .filter(|r| Some(r.to_string()) != primary)
                .map(PathBuf::from)
                .collect()
        }
        None => vec![],
    }
}

/// Update the project config.toml with project name, primary repo, and additional repos.
fn update_project_config(
    project_name: &str,
    primary_repo: &Path,
    additional_repos: &[PathBuf],
) -> Result<(), String> {
    let config_path = midtown::config::project_config_path(project_name);
    let mut config =
        midtown::config::FullProjectConfig::load_from(&config_path).unwrap_or_default();

    // Set project name
    config.project.name = Some(project_name.to_string());

    // Set primary repo
    let primary_str = primary_repo.to_string_lossy().to_string();
    config.project.primary_repo = Some(primary_str.clone());

    // Build full repos list: primary + additional
    let mut all_repos = vec![primary_str];
    for r in additional_repos {
        let s = r.to_string_lossy().to_string();
        if !all_repos.contains(&s) {
            all_repos.push(s);
        }
    }
    config.project.repos = all_repos;

    config
        .save_to(&config_path)
        .map_err(|e| format!("Failed to save project config: {}", e))
}

/// Build the claude command for the Lead session.
///
/// Returns the full command string to launch Claude Code with appropriate flags.
/// Always includes the system prompt via --append-system-prompt, whether new or resuming.
/// Also includes settings file with stop hook for channel sync.
/// Sets CLAUDE_CODE_TASK_LIST_ID so Lead shares tasks with coworkers.
fn build_lead_claude_command(
    task_list_id: &str,
    additional_repos: &[PathBuf],
) -> Result<String, String> {
    let prompt_file = write_lead_prompt_file()?;
    let settings_file = write_lead_settings_file()?;

    // Build --add-dir flags for additional repos
    let add_dir_flags: String = additional_repos
        .iter()
        .map(|r| format!(" --add-dir {}", r.display()))
        .collect();

    // Always start a fresh session. Users can /resume interactively if desired.
    Ok(format!(
        "export CLAUDE_CODE_TASK_LIST_ID='{}'; exec claude --dangerously-skip-permissions --settings {} --append-system-prompt \"$(cat {})\"{}",
        task_list_id,
        settings_file.display(),
        prompt_file.display(),
        add_dir_flags
    ))
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates tmux session for the project
/// 3. Launches Claude Code with Lead config in that session
pub fn handle_start(
    daemon_only: bool,
    project: Option<String>,
    repos: Vec<PathBuf>,
) -> Result<Response, String> {
    // Validate explicit project name if provided
    if let Some(ref name) = project {
        validate_project_name(name)?;
    }

    // Verify we're in a git repo first
    let primary_repo = repo_root()?;
    let project_name = resolve_project_name(&project).unwrap_or_else(|| {
        primary_repo
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });
    let additional_repos = resolve_repos(&repos, &project_name);
    let session = session_name_for(&Some(project_name.clone()))?;

    let mut messages = Vec::new();

    // Update project config with repo information
    let _ = update_project_config(&project_name, &primary_repo, &additional_repos);

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
        cmd.current_dir(&primary_repo);
        cmd.arg("--workdir").arg(&primary_repo);
        if project.is_some() {
            cmd.arg("--project").arg(&project_name);
        }

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
        // Get the shared task list ID for this project
        let task_list_id = midtown::paths::task_list_id_for_repo(&project_name);

        // Build the claude command (always starts fresh; users can /resume interactively)
        let claude_cmd = build_lead_claude_command(&task_list_id, &additional_repos)?;

        // Get project name for status bar (uppercase)
        let display_name = project_name.to_uppercase();

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
                &primary_repo.to_string_lossy(),
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
                &format!(" {} ", display_name),
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
                &format!("Midtown: {}", display_name),
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
        let marker_path = lead_initialized_marker(&primary_repo);
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

        messages.push(format!("Started Lead session in '{}'", session));
    }

    // Step 3: Auto-launch shared webserver if not running
    if !webserver_is_running() {
        match launch_webserver() {
            Ok(()) => messages.push(format!(
                "Started webserver on http://localhost:{}",
                midtown::webserver::DEFAULT_WEBSERVER_PORT
            )),
            Err(e) => messages.push(format!("Warning: Failed to start webserver: {}", e)),
        }
    } else {
        messages.push(format!(
            "Webserver running at http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        ));
    }

    // Build response message
    let attach_hint = "Attach with: midtown attach".to_string();
    messages.push(attach_hint);

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Path to the shared webserver PID file.
fn webserver_pid_file() -> PathBuf {
    midtown::paths::midtown_base_dir().join("webserver.pid")
}

/// Check if the shared webserver is running by testing its PID file lock.
fn webserver_is_running() -> bool {
    let pid_file = webserver_pid_file();
    if !pid_file.exists() {
        return false;
    }
    is_daemon_running(&pid_file)
}

/// Launch the shared webserver as a background process.
fn launch_webserver() -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {}", e))?;

    let mut cmd = Command::new(&exe);
    cmd.args(["webserver", "run"]);
    // Don't pass --foreground so it daemonizes itself
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn()
        .map_err(|e| format!("Failed to spawn webserver: {}", e))?;

    // Brief wait for it to start
    std::thread::sleep(std::time::Duration::from_millis(300));

    Ok(())
}

/// Stop the shared webserver by sending SIGTERM to its PID.
fn stop_webserver() -> Result<bool, String> {
    let pid_file = webserver_pid_file();
    if !pid_file.exists() {
        return Ok(false);
    }

    if !webserver_is_running() {
        // Stale PID file, clean up
        let _ = std::fs::remove_file(&pid_file);
        return Ok(false);
    }

    let pid_str = std::fs::read_to_string(&pid_file)
        .map_err(|e| format!("Failed to read webserver PID file: {}", e))?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|e| format!("Invalid PID in webserver PID file: {}", e))?;

    // Send SIGTERM
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status();

    // Poll until the process exits or timeout after 2 seconds
    let poll_interval = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();

    while webserver_is_running() && start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
    }

    // Force kill if still running after graceful timeout
    if webserver_is_running() {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stderr(Stdio::null())
            .status();

        // Poll again after SIGKILL
        let kill_start = std::time::Instant::now();
        let kill_timeout = std::time::Duration::from_secs(1);
        while webserver_is_running() && kill_start.elapsed() < kill_timeout {
            std::thread::sleep(poll_interval);
        }
    }

    // Clean up PID file
    let _ = std::fs::remove_file(&pid_file);

    Ok(true)
}

/// Handle `midtown webserver stop` command.
pub fn handle_webserver_stop() -> Result<Response, String> {
    match stop_webserver()? {
        true => Ok(Response::message("Stopped webserver")),
        false => Ok(Response::message("Webserver was not running")),
    }
}

/// Handle `midtown webserver restart` command.
pub fn handle_webserver_restart() -> Result<Response, String> {
    let was_running = stop_webserver()?;
    // stop_webserver() polls until confirmed dead, no extra sleep needed
    launch_webserver()?;
    if was_running {
        Ok(Response::message(format!(
            "Restarted webserver on http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    } else {
        Ok(Response::message(format!(
            "Started webserver on http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    }
}

/// Handle `midtown stop` command.
///
/// Stops the daemon, webserver, and optionally the tmux session.
/// Also cleans up any orphaned `gh webhook forward` processes.
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

    // Step 2: Stop daemon (this also signals the gh webhook forwarder to stop)
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

    // Step 3: Kill any orphaned `gh webhook forward` processes.
    // The daemon's SIGTERM handler should have already stopped these, but
    // if the daemon exited uncleanly they may be left behind.
    kill_orphaned_webhook_forwarders(&mut messages);

    // Step 4: Stop the standalone webserver
    if webserver_is_running() {
        match stop_webserver() {
            Ok(true) => messages.push("Stopped webserver".to_string()),
            Ok(false) => {}
            Err(e) => messages.push(format!("Warning: Failed to stop webserver: {}", e)),
        }
    }

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Kill any orphaned `gh webhook forward` processes for the current project.
///
/// Uses `pkill` to find and terminate processes matching the project-specific
/// webhook URL (e.g., `localhost:47023/webhook`). This avoids killing forwarders
/// belonging to other running projects in a multi-project setup.
///
/// Falls back to a broad `gh webhook forward` pattern only if the project's
/// webhook port cannot be determined.
fn kill_orphaned_webhook_forwarders(messages: &mut Vec<String>) {
    // Try to scope the pattern to this project's webhook port
    let pattern = resolve_project_name(&None)
        .map(|name| midtown::config::get_project_daemon_config(&name))
        .and_then(|daemon_cfg| daemon_cfg.webhook_port)
        .map(|port| format!("localhost:{}/webhook", port))
        .unwrap_or_else(|| "gh webhook forward".to_string());

    let output = Command::new("pgrep").args(["-f", &pattern]).output();

    match output {
        Ok(out) if out.status.success() => {
            // There are running gh webhook forward processes — kill them
            let _ = Command::new("pkill")
                .args(["-f", &pattern])
                .stderr(Stdio::null())
                .status();
            messages.push("Stopped gh webhook forwarder".to_string());
        }
        _ => {
            // No matching processes or pgrep failed — nothing to clean up
        }
    }
}

/// Handle `midtown restart` command.
///
/// Gracefully restarts the daemon and webserver while preserving the tmux
/// session and all running Claude processes (Lead and coworkers). The daemon
/// and webserver processes are restarted so they pick up new code, while
/// the chat pane is also respawned.
///
/// For a full fresh start, use `midtown stop && midtown start`.
pub fn handle_restart() -> Result<Response, String> {
    // Stop daemon and webserver, keep the tmux session running.
    // handle_stop also cleans up orphaned gh webhook forwarders.
    // stop_webserver() polls until the process is confirmed dead, so no
    // additional sleep is needed before restarting.
    let _ = handle_stop(true);

    // Confirm the webserver is fully stopped before restarting.
    // stop_webserver() should have already ensured this, but verify to
    // avoid the race where handle_start() sees a zombie process and
    // skips launching a new webserver.
    let poll_interval = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();
    while webserver_is_running() && start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
    }

    // Start daemon and webserver (session already exists, so it will
    // re-discover coworkers; handle_start also launches the webserver)
    let result = handle_start(false, None, vec![])?;

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
        handle_start(false, None, vec![])?;

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

    // Get the shared task list ID for this repo
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let task_list_id = midtown::paths::task_list_id_for_repo(&repo_name);

    // Build the claude command with settings (fresh session)
    let claude_cmd = build_lead_claude_command(&task_list_id, &[])?;

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
    fn test_build_lead_claude_command_includes_system_prompt() {
        let task_list_id = "midtown-test";

        let cmd = build_lead_claude_command(task_list_id, &[]).unwrap();

        assert!(
            cmd.contains("--append-system-prompt"),
            "Command must include --append-system-prompt, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_no_resume_flag() {
        let task_list_id = "midtown-test";

        let cmd = build_lead_claude_command(task_list_id, &[]).unwrap();

        assert!(
            !cmd.contains("--resume"),
            "Command should not include --resume (always fresh session), got: {}",
            cmd
        );
        assert!(
            !cmd.contains("--session-id"),
            "Command should not include --session-id (let claude manage it), got: {}",
            cmd
        );
    }

    #[test]
    fn test_handle_start_clears_stale_session_id() {
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

        // clear_stale_lead_session should remove the stale file
        clear_stale_lead_session(&repo_path);

        assert!(
            !session_file.exists(),
            "Stale session-id file should be deleted when no tmux session is running"
        );

        // Clean up parent dir
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn test_build_lead_claude_command_includes_task_list_id() {
        let task_list_id = "midtown-myrepo";

        let cmd = build_lead_claude_command(task_list_id, &[]).unwrap();

        assert!(
            cmd.contains("CLAUDE_CODE_TASK_LIST_ID='midtown-myrepo'"),
            "Command must set CLAUDE_CODE_TASK_LIST_ID, got: {}",
            cmd
        );
    }

    #[test]
    fn test_resolve_project_name_explicit() {
        let result = resolve_project_name(&Some("my-project".to_string()));
        assert_eq!(result, Some("my-project".to_string()));
    }

    #[test]
    fn test_resolve_project_name_none_outside_repo() {
        let temp = TempDir::new().unwrap();
        // No .git directory

        with_temp_cwd(temp.path(), || {
            let result = resolve_project_name(&None);
            // Outside a git repo, detect_project_name returns None and repo_root fails
            // so result should be None
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_session_name_for_explicit_project() {
        let result = session_name_for(&Some("myapp".to_string()));
        assert_eq!(result.unwrap(), "midtown-myapp");
    }

    #[test]
    fn test_session_name_for_none_outside_repo() {
        let temp = TempDir::new().unwrap();

        with_temp_cwd(temp.path(), || {
            let result = session_name_for(&None);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_build_lead_claude_command_with_additional_repos() {
        let task_list_id = "midtown-multi";
        let additional_repos = vec![
            PathBuf::from("/path/to/repo-a"),
            PathBuf::from("/path/to/repo-b"),
        ];

        let cmd = build_lead_claude_command(task_list_id, &additional_repos).unwrap();

        assert!(
            cmd.contains("--add-dir /path/to/repo-a"),
            "Command must include --add-dir for repo-a, got: {}",
            cmd
        );
        assert!(
            cmd.contains("--add-dir /path/to/repo-b"),
            "Command must include --add-dir for repo-b, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_no_additional_repos() {
        let task_list_id = "midtown-single";

        let cmd = build_lead_claude_command(task_list_id, &[]).unwrap();

        assert!(
            !cmd.contains("--add-dir"),
            "Command should not contain --add-dir with no additional repos, got: {}",
            cmd
        );
    }

    #[test]
    fn test_resolve_repos_uses_cli_when_provided() {
        let repos = vec![PathBuf::from("/path/a"), PathBuf::from("/path/b")];
        let result = resolve_repos(&repos, "nonexistent-project");
        assert_eq!(result, repos);
    }

    #[test]
    fn test_resolve_repos_empty_cli_returns_saved() {
        // With no CLI repos and no config, should return empty
        let result = resolve_repos(&[], &format!("no-such-project-{}", uuid::Uuid::new_v4()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_saved_repos_nonexistent_project() {
        let result = parse_saved_repos(&format!("no-such-project-{}", uuid::Uuid::new_v4()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_update_project_config_creates_config() {
        let dir = tempfile::tempdir().unwrap();
        let project_name = format!("test-update-{}", uuid::Uuid::new_v4());
        let primary_repo = dir.path().join("main-repo");
        let additional = vec![dir.path().join("extra-repo")];

        // This will try to write to ~/.midtown/projects/<name>/config.toml
        // We test that it doesn't panic/error
        let result = update_project_config(&project_name, &primary_repo, &additional);

        // Clean up
        let config_path = midtown::config::project_config_path(&project_name);
        let _ = std::fs::remove_file(&config_path);
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_project_name_valid() {
        assert!(validate_project_name("my-project").is_ok());
        assert!(validate_project_name("my_project").is_ok());
        assert!(validate_project_name("myproject123").is_ok());
        assert!(validate_project_name("my.project").is_ok());
        assert!(validate_project_name("A").is_ok());
    }

    #[test]
    fn test_validate_project_name_invalid() {
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name("my'project").is_err());
        assert!(validate_project_name("my project").is_err());
        assert!(validate_project_name("my/project").is_err());
        assert!(validate_project_name("my;project").is_err());
        assert!(validate_project_name("$(whoami)").is_err());
    }
}
