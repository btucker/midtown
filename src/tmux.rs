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

/// Capture the current content of a tmux pane.
///
/// Returns the pane content as a string, or None if capture fails.
fn capture_pane(target: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", target, "-p"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if the nudge text is still sitting in the input line (not submitted).
///
/// Returns true if the nudge appears to be stuck (Enter didn't work).
/// Looks for the text after the prompt symbol (❯) on any recent line.
fn is_nudge_stuck(pane_content: &str, nudge_text: &str) -> bool {
    // Get the last few lines (the prompt line might not be the very last)
    let lines: Vec<&str> = pane_content.lines().rev().take(5).collect();

    // Look for lines containing the prompt symbol with our nudge text after it
    for line in lines {
        if let Some(pos) = line.find('❯') {
            let after_prompt = &line[pos + '❯'.len_utf8()..];
            // Check if our nudge text (or a significant portion) is in the input
            // Use first 20 chars to avoid issues with line wrapping
            let check_text = if nudge_text.len() > 20 {
                &nudge_text[..20]
            } else {
                nudge_text
            };
            if after_prompt.contains(check_text) {
                return true;
            }
        }
    }

    false
}

/// Send keys (input) to a tmux window.
///
/// This is used to "nudge" a coworker by sending keyboard input.
/// Follows gastown's NudgeSession pattern exactly for reliability:
/// 1. Send text literally (with -l flag)
/// 2. Wait 500ms for paste to complete
/// 3. Send Escape (exits vim INSERT mode if enabled - safe since text is already pasted)
/// 4. Wait 100ms
/// 5. Send Enter with retry and verification (up to 3 attempts, 200ms between)
///
/// The Escape is safe AFTER the text is pasted because the text is already
/// in the input buffer. This handles vim mode users while not affecting
/// normal mode users.
///
/// After sending Enter, verifies the nudge was submitted by checking if the
/// text is still on the input line. If stuck, retries Enter.
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

    // 3. Wait 100ms before sending Enter
    thread::sleep(Duration::from_millis(100));

    // 4. Send Enter with retry and verification (up to 3 attempts, 200ms between)
    for attempt in 0..3 {
        if attempt > 0 {
            tracing::debug!(
                "Nudge verification: retrying Enter for {} (attempt {})",
                target,
                attempt + 1
            );
            thread::sleep(Duration::from_millis(200));
        }

        let status = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .status()
            .map_err(Error::Io)?;

        if !status.success() {
            continue;
        }

        // Wait a moment for the Enter to be processed, then verify
        thread::sleep(Duration::from_millis(100));

        // Check if the nudge is stuck (text still on input line)
        if let Some(content) = capture_pane(&target) {
            if !is_nudge_stuck(&content, keys) {
                // Success - nudge was submitted
                return Ok(());
            }
            // Nudge is stuck, will retry on next iteration
            tracing::debug!(
                "Nudge verification: detected stuck nudge for {}, will retry",
                target
            );
        } else {
            // Couldn't capture pane, assume success
            return Ok(());
        }
    }

    // All retries exhausted
    Err(Error::Rpc {
        code: -32603,
        message: format!(
            "Nudge failed after 3 attempts - text may still be on input line: {}",
            target
        ),
    })
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
///
/// Note: Window names may include a status suffix (e.g., "lexington:dev#3"),
/// so this checks if any window's base name matches the given name.
pub fn window_exists(session: &str, name: &str) -> crate::Result<bool> {
    // Use list-windows to get actual window names
    // has-session only checks if the SESSION exists, not the WINDOW
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()
        .map_err(Error::Io)?;

    // If tmux returns non-zero, session doesn't exist (so window doesn't either)
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let name_lower = name.to_lowercase();

    // Check if any window matches the name
    // Window names might have status suffix like "lexington:dev#3", so check base name
    Ok(stdout.lines().any(|window| {
        let base_name = window.split(':').next().unwrap_or(window).to_lowercase();
        base_name == name_lower
    }))
}

/// Read plugins from user's ~/.claude/settings.json
fn read_user_plugins() -> Option<serde_json::Value> {
    let home = std::env::var("HOME").ok()?;
    let settings_path = std::path::PathBuf::from(home)
        .join(".claude")
        .join("settings.json");
    let content = std::fs::read_to_string(settings_path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&content).ok()?;
    settings.get("enabledPlugins").cloned()
}

/// JSON settings for coworker Claude Code sessions.
///
/// Configures hooks for:
/// - Stop: Sync channel, check for unclaimed tasks, block if more work available
/// - PostToolUse: Broadcast task operations (claim, complete, create) to channel
/// - PostToolUse: Post insights to channel
/// - Notification: Post idle status when waiting for input
fn coworker_settings_json(bin_command: &str) -> serde_json::Value {
    // Read user's plugins from ~/.claude/settings.json
    let user_plugins = read_user_plugins().unwrap_or_default();

    serde_json::json!({
        "editorMode": "normal",
        "enabledPlugins": user_plugins,
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
///
/// If `resume` is true, passes `--continue` to claude to resume the previous
/// session from this worktree, preserving context from the last session.
pub fn spawn_claude(
    session: &str,
    name: &str,
    working_dir: &str,
    repo_name: Option<&str>,
    resume: bool,
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

    // Get the shared task list ID for this repo (all coworkers use the same task list)
    let task_list_id = repo_name
        .map(crate::paths::task_list_id_for_repo)
        .unwrap_or_else(crate::paths::task_list_id);

    // Build the claude command with session ID for task persistence
    // Use file paths for settings and prompt to avoid shell quoting issues
    // Set MIDTOWN_AGENT env var so the coworker's name appears in messages
    // Set CLAUDE_CODE_TASK_LIST_ID so all coworkers share the same task list
    // Use --setting-sources project,local (plugins are now in --settings file)
    // Add --continue flag if resuming a previous session
    let continue_flag = if resume { " --continue" } else { "" };
    let command = format!(
        "export MIDTOWN_AGENT='{}' CLAUDE_CODE_TASK_LIST_ID='{}'; claude --dangerously-skip-permissions --session-id {}{} --setting-sources project,local --settings {} --append-system-prompt \"$(cat {})\"",
        name,
        task_list_id,
        coworker_session_id,
        continue_flag,
        settings_file.display(),
        prompt_file.display()
    );

    // Create window with claude command running directly
    create_window(session, name, working_dir, Some(&command))?;

    // Set window tab color to match chat TUI team panel
    set_window_color(session, name)?;

    // Brief delay to let the window start up and potentially fail
    // This is necessary because tmux new-window returns success immediately,
    // even if the command inside fails and the window closes right away
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Verify the window actually exists (it may have died if the command failed)
    if !window_exists(session, name)? {
        return Err(Error::Rpc {
            code: -32603,
            message: format!(
                "Tmux window {}:{} was created but immediately closed (command likely failed)",
                session, name
            ),
        });
    }

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

/// Get the width of a tmux session's terminal in columns.
///
/// Uses `tmux display-message` to query the client width.
/// Returns None if the session doesn't exist or width can't be determined.
pub fn get_session_width(session: &str) -> Option<u16> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", session, "-p", "#{client_width}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let width_str = String::from_utf8_lossy(&output.stdout);
    width_str.trim().parse().ok()
}

/// Create a new window in the session for the chat TUI.
///
/// This is used when the terminal is too narrow for a split layout.
/// The window is named "chat" and starts the chat command.
pub fn create_chat_window(session: &str, bin_command: &str) -> crate::Result<()> {
    let chat_cmd = format!("{} chat", bin_command);

    // Create a new window named "chat"
    let status = Command::new("tmux")
        .args([
            "new-window",
            "-d", // Don't switch to it
            "-t",
            session,
            "-n",
            "chat",
        ])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to create chat window in session {}", session),
        });
    }

    // Start chat TUI in the new window
    let chat_target = format!("{}:chat", session);
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &chat_target, &chat_cmd, "Enter"])
        .status();

    Ok(())
}

/// Create a split pane for the chat TUI in the lead window.
///
/// Splits the lead window horizontally with chat on the right (30% width).
pub fn create_chat_split(session: &str, bin_command: &str) -> crate::Result<()> {
    let lead_target = format!("{}:lead", session);
    let chat_cmd = format!("{} chat", bin_command);

    // Split the lead window horizontally with 30% for chat
    let status = Command::new("tmux")
        .args(["split-window", "-h", "-t", &lead_target, "-p", "30"])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to split lead window in session {}", session),
        });
    }

    // Start chat TUI in the new pane (pane .1)
    let chat_pane = format!("{}:lead.1", session);
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &chat_pane, &chat_cmd, "Enter"])
        .status();

    // Keep focus on the main pane (Claude Code, pane .0)
    let main_pane = format!("{}:lead.0", session);
    let _ = Command::new("tmux")
        .args(["select-pane", "-t", &main_pane])
        .status();

    Ok(())
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

    // Integration tests for window_exists require actual tmux
    // The key fix: window_exists now uses list-windows instead of has-session
    // This ensures we check for the actual WINDOW, not just the SESSION
    //
    // Bug scenario that was fixed:
    // 1. spawn_claude creates a window with a command that immediately fails
    // 2. The window dies, but the SESSION still exists (Lead is using it)
    // 3. OLD: has-session returns success (session exists) -> false positive
    // 4. NEW: list-windows returns no match -> correctly detects window is gone

    #[test]
    fn test_is_nudge_stuck_detects_text_after_prompt() {
        let pane_content = r#"
Some previous output
More output
❯ You've been assigned task #36: Chat TUI still showing...
"#;
        let nudge_text = "You've been assigned task #36: Chat TUI still showing old messages";
        assert!(is_nudge_stuck(pane_content, nudge_text));
    }

    #[test]
    fn test_is_nudge_stuck_no_match_when_submitted() {
        let pane_content = r#"
You've been assigned task #36: Chat TUI still showing...
Claude is now processing the request
❯
"#;
        let nudge_text = "You've been assigned task #36: Chat TUI still showing old messages";
        // The nudge text appears earlier but NOT after the prompt
        assert!(!is_nudge_stuck(pane_content, nudge_text));
    }

    #[test]
    fn test_is_nudge_stuck_empty_prompt() {
        let pane_content = "❯ ";
        let nudge_text = "Some nudge message";
        assert!(!is_nudge_stuck(pane_content, nudge_text));
    }

    #[test]
    fn test_is_nudge_stuck_no_prompt() {
        let pane_content = "Just some output without a prompt";
        let nudge_text = "Some nudge message";
        assert!(!is_nudge_stuck(pane_content, nudge_text));
    }

    #[test]
    fn test_is_nudge_stuck_partial_match() {
        // Tests that we match on first 20 chars of long messages
        let pane_content = "❯ You've been assigned";
        let nudge_text = "You've been assigned task #36: Chat TUI still showing old messages";
        assert!(is_nudge_stuck(pane_content, nudge_text));
    }
}
