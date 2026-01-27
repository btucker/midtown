//! Tmux session management for coworker processes.
//!
//! Provides functions for creating, managing, and communicating with
//! tmux windows that host coworker Claude Code processes within the
//! project session.

use std::path::PathBuf;
use std::process::Command;

use crate::Error;

/// Parse a status message and return an abbreviated version for tmux tab display.
///
/// Extracts status keywords and task numbers to create concise tab names.
///
/// # Examples
/// - "claiming task #1" → "claim#1"
/// - "developing task #1" → "dev#1"
/// - "running tests" → "test"
/// - "opening PR for task #1" → "PR#1"
/// - "waiting for review" → "idle"
/// - "investigating the auth bug" → "investigating the au..."
pub fn parse_status(status: &str) -> String {
    let status_lower = status.to_lowercase();

    // Extract task number if present (matches "#1", "task 1", "task #1", etc.)
    let task_num = extract_task_number(status);

    // Match status keywords and map to abbreviations
    // Order matters: more specific/priority states come first
    let abbrev = if status_lower.contains("claim") {
        "claim"
    } else if status_lower.contains("complet") || status_lower.contains("finish") {
        // Check "completed/finished" before "implement" which could match "implementation"
        "done"
    } else if status_lower.contains("idle")
        || status_lower.contains("waiting")
        || status_lower.contains("blocked")
    {
        // Check "waiting/blocked" before "review" which could match "waiting for review"
        "idle"
    } else if status_lower.contains("pr ")
        || status_lower.contains("pull request")
        || status_lower.starts_with("pr")
        || status_lower.contains("review")
    {
        // Match "PR " with space to avoid false positives, or "review" for code review
        "PR"
    } else if status_lower.contains("develop")
        || status_lower.contains("working")
        || status_lower.contains("coding")
        || status_lower.contains("implement")
    {
        "dev"
    } else if status_lower.contains("test") {
        "test"
    } else if status_lower.contains("debug") || status_lower.contains("investigating") {
        "debug"
    } else {
        // No keyword match - truncate the original status
        return truncate_status(status, 20);
    };

    // Combine abbreviation with task number if present
    match task_num {
        Some(num) => format!("{}#{}", abbrev, num),
        None => abbrev.to_string(),
    }
}

/// Extract task number from status text.
///
/// Matches patterns like "#1", "task 1", "task #1", "#42"
fn extract_task_number(status: &str) -> Option<u32> {
    // Try to find "#N" pattern first
    if let Some(pos) = status.find('#') {
        let rest = &status[pos + 1..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u32>() {
            return Some(num);
        }
    }

    // Try "task N" pattern (case insensitive)
    let lower = status.to_lowercase();
    if let Some(pos) = lower.find("task ") {
        let rest = &status[pos + 5..];
        // Skip optional '#' after "task "
        let rest = rest.strip_prefix('#').unwrap_or(rest);
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u32>() {
            return Some(num);
        }
    }

    None
}

/// Truncate status to a maximum length, adding "..." if truncated.
fn truncate_status(status: &str, max_len: usize) -> String {
    if status.len() <= max_len {
        status.to_string()
    } else {
        format!("{}...", &status[..max_len.saturating_sub(3)])
    }
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

/// Write a coworker's system prompt to a file and return the path.
fn write_coworker_prompt_file(name: &str, prompt: &str) -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join(format!("coworker-{}-prompt.md", name));
    std::fs::write(&path, prompt).map_err(Error::Io)?;

    Ok(path)
}

/// Read the Lead's session ID for a repository.
///
/// Returns None if no Lead session has been started for this repo.
pub fn get_lead_session_id(repo_name: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let session_file = PathBuf::from(home)
        .join(".midtown")
        .join(repo_name)
        .join("lead-session-id");

    std::fs::read_to_string(&session_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Create a symlink so the coworker's task storage points to the Lead's tasks.
///
/// This allows coworkers to see and modify the same task list as the Lead.
/// Creates: ~/.claude/tasks/<coworker_session_id> -> ~/.claude/tasks/<lead_session_id>
///
/// This function is public so the daemon can update symlinks when the Lead session changes.
pub fn symlink_tasks_to_lead(
    coworker_session_id: &str,
    lead_session_id: &str,
) -> crate::Result<()> {
    let home = std::env::var("HOME").map_err(|e| Error::Io(std::io::Error::other(e)))?;
    let tasks_dir = PathBuf::from(&home).join(".claude").join("tasks");

    // Ensure the tasks directory exists
    std::fs::create_dir_all(&tasks_dir).map_err(Error::Io)?;

    let coworker_tasks = tasks_dir.join(coworker_session_id);
    let lead_tasks = tasks_dir.join(lead_session_id);

    // Ensure Lead's task directory exists
    std::fs::create_dir_all(&lead_tasks).map_err(Error::Io)?;

    // Remove existing symlink or directory if it exists
    if coworker_tasks.exists() || coworker_tasks.is_symlink() {
        if coworker_tasks.is_symlink() {
            std::fs::remove_file(&coworker_tasks).map_err(Error::Io)?;
        } else if coworker_tasks.is_dir() {
            std::fs::remove_dir_all(&coworker_tasks).map_err(Error::Io)?;
        }
    }

    // Create symlink: coworker -> lead
    #[cfg(unix)]
    std::os::unix::fs::symlink(&lead_tasks, &coworker_tasks).map_err(Error::Io)?;

    Ok(())
}

/// Prefix for all midtown tmux sessions.
pub const SESSION_PREFIX: &str = "midtown-";

/// Coworker name to tmux color mapping.
/// These colors match the AVENUE_COLORS in cli/chat/ui.rs for visual consistency.
/// lead uses brightyellow for visibility.
const COWORKER_COLORS: &[(&str, &str)] = &[
    ("lead", "brightyellow"),
    ("lexington", "cyan"),
    ("park", "green"),
    ("madison", "yellow"),
    ("broadway", "magenta"),
    ("amsterdam", "blue"),
    ("columbus", "red"),
    ("riverside", "brightcyan"),
    ("york", "brightgreen"),
    ("pleasant", "brightmagenta"),
    ("vernon", "brightblue"),
    // Overflow names
    ("bleecker", "colour208"), // orange
    ("houston", "colour213"),  // pink
    ("canal", "colour117"),    // light blue
    ("spring", "colour156"),   // light green
    ("prince", "colour183"),   // lavender
    ("mercer", "colour216"),   // salmon
];

/// Get the tmux color for a coworker name.
fn get_coworker_color(name: &str) -> Option<&'static str> {
    COWORKER_COLORS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, color)| *color)
}

/// Set the tmux window tab color for a coworker.
///
/// This sets the window-status-style for the window to match the coworker's
/// assigned color, providing visual consistency with the chat TUI team panel.
fn set_window_color(session: &str, name: &str) -> crate::Result<()> {
    let Some(color) = get_coworker_color(name) else {
        return Ok(()); // Unknown coworker, skip color setting
    };

    let target = format!("{}:{}", session, name);
    let style = format!("fg={}", color);

    // Set the window-status-style for this specific window
    let status = Command::new("tmux")
        .args([
            "set-window-option",
            "-t",
            &target,
            "window-status-style",
            &style,
        ])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        // Non-fatal - log but don't fail
        eprintln!("Warning: Failed to set window color for {}", name);
    }

    // Also set current-style for when the window is selected (brighter version)
    let current_style = format!("fg={},bold", color);
    let _ = Command::new("tmux")
        .args([
            "set-window-option",
            "-t",
            &target,
            "window-status-current-style",
            &current_style,
        ])
        .status();

    Ok(())
}

/// Set up a tmux hook to update the status bar color based on the active window.
///
/// When a window gains focus, this hook:
/// 1. Gets the window name (agent name like "Lead", "lexington", etc.)
/// 2. Extracts the base name (before any ":" for status suffix)
/// 3. Looks up the agent's color from COWORKER_COLORS
/// 4. Updates the session's status-style with that color as the foreground
///
/// The default status bar background is colour236 (dark gray).
pub fn setup_status_bar_hook(session: &str) -> crate::Result<()> {
    // Build a shell case statement for color lookup
    // Window names may have status suffixes like "lexington: investigating..."
    // so we extract just the base name before any ":"
    let case_arms: Vec<String> = COWORKER_COLORS
        .iter()
        .map(|(name, color)| {
            // Case-insensitive matching using lowercase
            let lower_name = name.to_lowercase();
            format!("        {}) color=\"{}\" ;;", lower_name, color)
        })
        .collect();

    let case_statement = case_arms.join("\n");

    // Shell script that runs on window focus:
    // 1. Get the window name and extract base (before ":")
    // 2. Convert to lowercase for case-insensitive matching
    // 3. Look up color with case statement
    // 4. If found, set status-style with that color
    //
    // Note: We use double quotes for the run-shell argument and escape inner
    // double quotes. This avoids complex single-quote escaping for tr's character
    // class arguments like '[:upper:]'.
    let script = format!(
        r#"window_name=$(tmux display-message -p '#{{window_name}}'); \
base_name=$(echo \"$window_name\" | cut -d: -f1); \
lower_name=$(echo \"$base_name\" | tr [:upper:] [:lower:]); \
color=\"\"; \
case \"$lower_name\" in
{}
        *) color=\"\" ;;
esac; \
if [ -n \"$color\" ]; then \
    tmux set-option -t {} status-style bg=colour236,fg=$color; \
fi"#,
        case_statement, session
    );

    let status = Command::new("tmux")
        .args([
            "set-hook",
            "-t",
            session,
            "pane-focus-in",
            &format!("run-shell \"{}\"", script),
        ])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        // Non-fatal - log but don't fail
        eprintln!(
            "Warning: Failed to set status bar hook for session {}",
            session
        );
    }

    Ok(())
}

/// Create a new tmux window for a coworker in the project session.
///
/// Creates a window named `<name>` within the project session with the given working directory.
/// If `command` is provided, runs that command in the window instead of starting a shell.
pub fn create_window(
    session: &str,
    name: &str,
    working_dir: &str,
    command: Option<&str>,
) -> crate::Result<()> {
    let mut args = vec![
        "new-window",
        "-d",
        "-t",
        session,
        "-n",
        name,
        "-c",
        working_dir,
    ];

    // If command provided, run it via sh -c
    if let Some(cmd) = command {
        args.extend(["sh", "-c", cmd]);
    }

    let status = Command::new("tmux")
        .args(&args)
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to create tmux window: {}:{}", session, name),
        });
    }

    Ok(())
}

/// Kill a tmux window within the project session.
pub fn kill_window(session: &str, name: &str) -> crate::Result<()> {
    let target = format!("{}:{}", session, name);

    let status = Command::new("tmux")
        .args(["kill-window", "-t", &target])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to kill tmux window: {}", target),
        });
    }

    Ok(())
}

/// Send keys (input) to a tmux window.
///
/// This is used to "nudge" a coworker by sending keyboard input.
/// Sends the text literally (with -l flag), waits for paste to process,
/// then presses Enter. Based on gastown's NudgeSession implementation.
pub fn send_keys(session: &str, name: &str, keys: &str) -> crate::Result<()> {
    use std::thread;
    use std::time::Duration;

    let target = format!("{}:{}", session, name);

    // 1. Send the text literally (avoid tmux interpreting special key names)
    let status = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", keys])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send keys to tmux window: {}", target),
        });
    }

    // 2. Wait 500ms for paste to complete (critical - tested in gastown)
    thread::sleep(Duration::from_millis(500));

    // 3. Send Escape to exit vim INSERT mode if enabled (harmless in normal mode)
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &target, "Escape"])
        .status();
    thread::sleep(Duration::from_millis(100));

    // 4. Send Enter key
    let status = Command::new("tmux")
        .args(["send-keys", "-t", &target, "Enter"])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send Enter to tmux window: {}", target),
        });
    }

    Ok(())
}

/// Rename a tmux window to show coworker status.
///
/// Updates the window name to include the coworker's current status,
/// providing visibility even when the chat TUI is not in focus.
///
/// # Arguments
/// * `session` - The tmux session name
/// * `name` - The coworker name (window name)
/// * `status` - The status to display (e.g., "investigating auth bug")
///
/// # Window Name Format
/// - With status: "lexington:dev#3"
/// - Without status (idle): "lexington"
///
/// Status is truncated to keep the tab readable (max 20 chars).
pub fn rename_window(session: &str, name: &str, status: Option<&str>) -> crate::Result<()> {
    let target = format!("{}:{}", session, name);

    // Build the new window name with parsed/abbreviated status
    let new_name = match status {
        Some(s) if !s.is_empty() => {
            // Parse status to extract keywords and task numbers
            let parsed = parse_status(s);
            format!("{}:{}", name, parsed)
        }
        _ => name.to_string(),
    };

    let status = Command::new("tmux")
        .args(["rename-window", "-t", &target, &new_name])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        // Non-fatal - window might not exist yet
        tracing::debug!("Failed to rename tmux window {} to {}", target, new_name);
    }

    Ok(())
}

/// Send raw keys without appending Enter.
pub fn send_keys_raw(session: &str, name: &str, keys: &str) -> crate::Result<()> {
    let target = format!("{}:{}", session, name);

    let status = Command::new("tmux")
        .args(["send-keys", "-t", &target, keys])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send keys to tmux window: {}", target),
        });
    }

    Ok(())
}

/// List all coworker windows in the project session.
///
/// Returns a vector of window names (excluding "Lead" which is the main window).
pub fn list_windows(session: &str) -> crate::Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()
        .map_err(Error::Io)?;

    // If tmux returns non-zero, it might mean no session exists
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running")
            || stderr.contains("session not found")
            || stderr.contains("can't find session")
        {
            return Ok(Vec::new());
        }
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to list tmux windows: {}", stderr),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let windows: Vec<String> = stdout
        .lines()
        .filter(|name| *name != "lead") // Exclude the lead window
        .map(|s| s.to_string())
        .collect();

    Ok(windows)
}

/// Check if a window exists in the session.
pub fn window_exists(session: &str, name: &str) -> crate::Result<bool> {
    let target = format!("{}:{}", session, name);

    let status = Command::new("tmux")
        .args(["has-session", "-t", &target])
        .status()
        .map_err(Error::Io)?;

    Ok(status.success())
}

/// JSON settings for coworker Claude Code sessions.
///
/// Configures hooks for:
/// - Stop: Sync channel, check for unclaimed tasks, block if more work available
/// - PostToolUse: Broadcast task operations (claim, complete, create) to channel
/// - PostToolUse: Post insights to channel
/// - Notification: Post idle status when waiting for input
fn coworker_settings_json(bin_command: &str) -> serde_json::Value {
    serde_json::json!({
        "editorMode": "normal",
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} --format json coworker stop-hook", bin_command)
                }]
            }],
            "PostToolUse": [{
                "matcher": "TaskUpdate",
                "hooks": [{
                    "type": "command",
                    "command": format!("{} coworker task-hook", bin_command)
                }]
            }, {
                "matcher": "TaskCreate",
                "hooks": [{
                    "type": "command",
                    "command": format!("{} coworker task-hook", bin_command)
                }]
            }, {
                "matcher": "AskUserQuestion",
                "hooks": [{
                    "type": "command",
                    "command": format!("{} coworker ask-hook", bin_command)
                }]
            }, {
                // No matcher = runs on every tool use
                "hooks": [{
                    "type": "command",
                    "command": format!("{} hook insight", bin_command)
                }]
            }],
            "Notification": [{
                "matcher": "idle_prompt",
                "hooks": [{
                    "type": "command",
                    "command": format!("{} hook idle", bin_command)
                }]
            }]
        }
    })
}

/// Write coworker settings to a shared file and return the path.
/// All coworkers use the same settings file.
fn write_coworker_settings_file(bin_command: &str) -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("coworker-settings.json");
    let settings = coworker_settings_json(bin_command);
    std::fs::write(&path, settings.to_string()).map_err(Error::Io)?;

    Ok(path)
}

/// Spawn Claude Code in a tmux window within the project session.
///
/// This creates a window and starts `claude` in it with coworker-specific
/// settings, including a Stop hook that reads the channel whenever the agent pauses.
/// Also injects a system prompt that gives the coworker instructions for operating
/// in the midtown environment.
///
/// If `repo_name` is provided, the coworker's task storage will be symlinked to
/// the Lead's task storage, enabling shared task visibility across the team.
/// Spawn Claude Code in a tmux window, returning the coworker's session ID.
///
/// Returns the generated session UUID for use in task symlink management.
pub fn spawn_claude(
    session: &str,
    name: &str,
    working_dir: &str,
    repo_name: Option<&str>,
) -> crate::Result<String> {
    // Get bin_command from project config
    let bin_command = crate::config::get_bin_command();

    // Build the claude command with settings for channel synchronization
    // and a system prompt for coworker identity and instructions
    let system_prompt = crate::agents::coworker_system_prompt(name);

    // Write system prompt and settings to files (avoids quoting issues)
    let prompt_file = write_coworker_prompt_file(name, &system_prompt)?;
    let settings_file = write_coworker_settings_file(&bin_command)?;

    // Generate a unique session ID for this coworker
    let coworker_session_id = uuid::Uuid::new_v4().to_string();

    // If we have the repo name, try to symlink tasks to the Lead's task storage
    if let Some(repo) = repo_name
        && let Some(lead_session_id) = get_lead_session_id(repo)
    {
        // Symlink coworker tasks -> lead tasks
        if let Err(e) = symlink_tasks_to_lead(&coworker_session_id, &lead_session_id) {
            // Log but don't fail - task sharing is nice-to-have
            eprintln!("Warning: Failed to symlink tasks for {}: {}", name, e);
        }
    }

    // Build the claude command with session ID for task persistence
    // Use file paths for settings and prompt to avoid shell quoting issues
    // Set MIDTOWN_AGENT env var so the coworker's name appears in messages
    // Use --setting-sources project,local to use project settings (no vim mode)
    let command = format!(
        "export MIDTOWN_AGENT={}; claude --dangerously-skip-permissions --session-id {} --setting-sources project,local --settings {} --append-system-prompt \"$(cat {})\"",
        name,
        coworker_session_id,
        settings_file.display(),
        prompt_file.display()
    );

    // Create window with claude command running directly
    create_window(session, name, working_dir, Some(&command))?;

    // Set window tab color to match chat TUI team panel
    set_window_color(session, name)?;

    Ok(coworker_session_id)
}

// Legacy functions for backward compatibility during transition
// These will be removed once all callers are updated

/// Create a new tmux session (legacy - use create_window instead).
#[deprecated(note = "Use create_window instead")]
pub fn create_session(name: &str, working_dir: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session_name, "-c", working_dir])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to create tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// Kill a tmux session (legacy - use kill_window instead).
#[deprecated(note = "Use kill_window instead")]
pub fn kill_session(name: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["kill-session", "-t", &session_name])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to kill tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// List all midtown tmux sessions (legacy - use list_windows instead).
#[deprecated(note = "Use list_windows instead")]
pub fn list_sessions() -> crate::Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map_err(Error::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("no sessions") {
            return Ok(Vec::new());
        }
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to list tmux sessions: {}", stderr),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(SESSION_PREFIX).map(|s| s.to_string()))
        .collect();

    Ok(sessions)
}

/// Check if a session exists (legacy - use window_exists instead).
#[deprecated(note = "Use window_exists instead")]
pub fn session_exists(name: &str) -> crate::Result<bool> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .status()
        .map_err(Error::Io)?;

    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_prefix() {
        assert_eq!(SESSION_PREFIX, "midtown-");
    }

    #[test]
    fn test_coworker_settings_json_is_valid() {
        let settings = coworker_settings_json("midtown");

        // Verify editorMode is normal (not vim)
        assert_eq!(settings["editorMode"], "normal");

        // Verify Stop hook structure
        assert!(settings["hooks"]["Stop"].is_array());
        let stop_hooks = &settings["hooks"]["Stop"][0]["hooks"];
        assert!(stop_hooks.is_array());
        assert_eq!(stop_hooks[0]["type"], "command");
        assert_eq!(
            stop_hooks[0]["command"],
            "midtown --format json coworker stop-hook"
        );

        // Verify PostToolUse hooks for task operations, questions, and insights
        let post_tool_hooks = &settings["hooks"]["PostToolUse"];
        assert!(post_tool_hooks.is_array());
        assert_eq!(post_tool_hooks.as_array().unwrap().len(), 4);

        // TaskUpdate hook
        assert_eq!(post_tool_hooks[0]["matcher"], "TaskUpdate");
        assert_eq!(
            post_tool_hooks[0]["hooks"][0]["command"],
            "midtown coworker task-hook"
        );

        // TaskCreate hook
        assert_eq!(post_tool_hooks[1]["matcher"], "TaskCreate");
        assert_eq!(
            post_tool_hooks[1]["hooks"][0]["command"],
            "midtown coworker task-hook"
        );

        // AskUserQuestion hook
        assert_eq!(post_tool_hooks[2]["matcher"], "AskUserQuestion");
        assert_eq!(
            post_tool_hooks[2]["hooks"][0]["command"],
            "midtown coworker ask-hook"
        );

        // Insight hook (no matcher)
        assert!(post_tool_hooks[3]["matcher"].is_null());
        assert_eq!(
            post_tool_hooks[3]["hooks"][0]["command"],
            "midtown hook insight"
        );

        // Verify Notification hook for idle
        let notification_hooks = &settings["hooks"]["Notification"];
        assert!(notification_hooks.is_array());
        assert_eq!(notification_hooks[0]["matcher"], "idle_prompt");
        assert_eq!(
            notification_hooks[0]["hooks"][0]["command"],
            "midtown hook idle"
        );
    }

    #[test]
    fn test_coworker_settings_json_custom_bin() {
        let settings = coworker_settings_json("cargo run --release --");

        let stop_hooks = &settings["hooks"]["Stop"][0]["hooks"];
        assert_eq!(
            stop_hooks[0]["command"],
            "cargo run --release -- --format json coworker stop-hook"
        );
    }

    // Note: coworker_system_prompt tests moved to src/agents.rs

    #[test]
    fn test_get_lead_session_id_returns_none_for_missing_repo() {
        // Non-existent repo should return None
        let result = get_lead_session_id("nonexistent-test-repo-12345");
        assert!(result.is_none());
    }

    #[test]
    fn test_symlink_tasks_to_lead() {
        use tempfile::TempDir;

        // Create temp directory to simulate ~/.claude/tasks/
        let temp = TempDir::new().unwrap();
        let tasks_dir = temp.path();

        let lead_id = "lead-session-123";
        let coworker_id = "coworker-session-456";

        let lead_tasks = tasks_dir.join(lead_id);
        let coworker_tasks = tasks_dir.join(coworker_id);

        // Create lead's task directory with a test file
        std::fs::create_dir_all(&lead_tasks).unwrap();
        std::fs::write(lead_tasks.join("1.json"), "{}").unwrap();

        // Create symlink manually (since symlink_tasks_to_lead uses HOME)
        #[cfg(unix)]
        std::os::unix::fs::symlink(&lead_tasks, &coworker_tasks).unwrap();

        // Verify the symlink works - coworker can see lead's file
        assert!(coworker_tasks.join("1.json").exists());

        // Verify it's actually a symlink
        assert!(coworker_tasks.is_symlink());
    }

    #[test]
    fn test_get_coworker_color_known_names() {
        assert_eq!(get_coworker_color("lead"), Some("brightyellow"));
        assert_eq!(get_coworker_color("lexington"), Some("cyan"));
        assert_eq!(get_coworker_color("park"), Some("green"));
        assert_eq!(get_coworker_color("madison"), Some("yellow"));
        assert_eq!(get_coworker_color("broadway"), Some("magenta"));
        assert_eq!(get_coworker_color("amsterdam"), Some("blue"));
        assert_eq!(get_coworker_color("columbus"), Some("red"));
    }

    #[test]
    fn test_get_coworker_color_case_insensitive() {
        assert_eq!(get_coworker_color("LEAD"), Some("brightyellow"));
        assert_eq!(get_coworker_color("Lead"), Some("brightyellow"));
        assert_eq!(get_coworker_color("LEXINGTON"), Some("cyan"));
        assert_eq!(get_coworker_color("Lexington"), Some("cyan"));
        assert_eq!(get_coworker_color("LeXiNgToN"), Some("cyan"));
    }

    #[test]
    fn test_get_coworker_color_unknown_returns_none() {
        assert_eq!(get_coworker_color("unknown"), None);
        assert_eq!(get_coworker_color("coworker"), None);
        assert_eq!(get_coworker_color(""), None);
    }

    // Integration tests would require actual tmux, so we keep unit tests minimal

    #[test]
    fn test_parse_status_claiming() {
        assert_eq!(parse_status("claiming task #1"), "claim#1");
        assert_eq!(parse_status("Claiming task 5"), "claim#5");
        assert_eq!(parse_status("just claimed #3"), "claim#3");
    }

    #[test]
    fn test_parse_status_developing() {
        assert_eq!(parse_status("developing task #1"), "dev#1");
        assert_eq!(parse_status("working on task #2"), "dev#2");
        assert_eq!(parse_status("coding the feature"), "dev");
        assert_eq!(parse_status("implementing auth #5"), "dev#5");
    }

    #[test]
    fn test_parse_status_testing() {
        assert_eq!(parse_status("testing"), "test");
        assert_eq!(parse_status("running tests for #3"), "test#3");
        assert_eq!(parse_status("test suite running"), "test");
    }

    #[test]
    fn test_parse_status_pr() {
        assert_eq!(parse_status("opening PR for task #1"), "PR#1");
        assert_eq!(parse_status("PR ready"), "PR");
        assert_eq!(parse_status("creating pull request #4"), "PR#4");
        assert_eq!(parse_status("requesting review #2"), "PR#2");
    }

    #[test]
    fn test_parse_status_debug() {
        assert_eq!(parse_status("debugging auth bug"), "debug");
        assert_eq!(parse_status("investigating the issue #7"), "debug#7");
    }

    #[test]
    fn test_parse_status_idle() {
        assert_eq!(parse_status("idle"), "idle");
        assert_eq!(parse_status("waiting for review"), "idle");
        assert_eq!(parse_status("blocked on task #3"), "idle#3");
    }

    #[test]
    fn test_parse_status_done() {
        assert_eq!(parse_status("completed task #1"), "done#1");
        assert_eq!(parse_status("finished implementation"), "done");
    }

    #[test]
    fn test_parse_status_no_keyword_truncates() {
        assert_eq!(parse_status("doing something"), "doing something");
        assert_eq!(
            parse_status("this is a very long status message that should be truncated"),
            "this is a very lo..."
        );
    }

    #[test]
    fn test_extract_task_number_hash_format() {
        assert_eq!(extract_task_number("task #1"), Some(1));
        assert_eq!(extract_task_number("#42 is the answer"), Some(42));
        assert_eq!(extract_task_number("working on #5"), Some(5));
    }

    #[test]
    fn test_extract_task_number_task_word_format() {
        assert_eq!(extract_task_number("claiming task 3"), Some(3));
        assert_eq!(extract_task_number("TASK 7 is mine"), Some(7));
    }

    #[test]
    fn test_extract_task_number_none() {
        assert_eq!(extract_task_number("just coding"), None);
        assert_eq!(extract_task_number("no numbers here"), None);
        assert_eq!(extract_task_number("#"), None);
    }

    #[test]
    fn test_truncate_status() {
        assert_eq!(truncate_status("short", 20), "short");
        assert_eq!(
            truncate_status("exactly twenty chars", 20),
            "exactly twenty chars"
        );
        assert_eq!(
            truncate_status("this is way too long for the tab", 20),
            "this is way too l..."
        );
    }
}
