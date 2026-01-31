//! Tmux session management for coworker processes.
//!
//! Provides functions for creating, managing, and communicating with
//! tmux windows that host coworker Claude Code processes within the
//! project session.

use std::path::PathBuf;
use std::process::Command;

use crate::Error;

/// Embedded common settings shared by both Lead and coworker Claude Code sessions.
const DEFAULT_COMMON_SETTINGS: &str = include_str!("../agents/common-settings.json");

/// Embedded settings specific to Lead Claude Code sessions (merged on top of common).
const DEFAULT_LEAD_SETTINGS: &str = include_str!("../agents/lead-settings.json");

/// Embedded settings specific to coworker Claude Code sessions (merged on top of common).
const DEFAULT_COWORKER_SETTINGS: &str = include_str!("../agents/coworker-settings.json");

/// Load settings by merging common with role-specific, then replacing `{bin}` placeholders.
///
/// Role-specific keys override common keys (shallow merge of top-level keys).
/// The `{bin}` placeholder in hook commands is replaced with the actual binary command.
fn load_settings(role_settings: &str) -> serde_json::Value {
    let bin_command = crate::config::get_bin_command();

    // Merge common + role JSON as raw strings, then replace {bin} before parsing.
    // This is simpler than walking the parsed JSON tree to find command strings.
    let common = DEFAULT_COMMON_SETTINGS.replace("{bin}", &bin_command);
    let role = role_settings.replace("{bin}", &bin_command);

    let mut settings: serde_json::Value =
        serde_json::from_str(&common).expect("invalid common-settings.json");
    let role: serde_json::Value = serde_json::from_str(&role).expect("invalid role settings JSON");

    if let (Some(base), Some(overrides)) = (settings.as_object_mut(), role.as_object()) {
        for (key, value) in overrides {
            base.insert(key.clone(), value.clone());
        }
    }

    settings
}

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
    } else if status_lower.contains("reviewing") {
        // Active code review — distinct from opening/requesting a PR
        "review"
    } else if status_lower.contains("pr ")
        || status_lower.contains("pull request")
        || status_lower.starts_with("pr")
        || status_lower.contains("review")
    {
        // Match "PR " with space to avoid false positives, or "review" for requesting review
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

/// Write a coworker's initial prompt to a file and return the path.
///
/// This is the task/nudge message that the coworker should work on. It's
/// passed to claude via `-p "$(cat file)"` so it's available at startup
/// without needing to send keystrokes after the TUI initializes.
fn write_coworker_initial_prompt_file(name: &str, prompt: &str) -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join(format!("coworker-{}-initial-prompt.md", name));
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
    ("madison", "brightred"),
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
pub fn capture_pane(target: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", target, "-p"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check whether the human has typed text into the Claude Code input prompt.
///
/// Inspects the last few lines of pane content for the `❯` prompt symbol.
/// Returns `true` if there is non-whitespace text after the prompt, meaning
/// the human is currently typing. Returns `false` if the prompt is empty
/// or no prompt is visible (e.g., Claude is processing).
///
/// Used by the daemon to avoid nudging the lead while they're mid-sentence.
pub fn has_input_text(pane_content: &str) -> bool {
    // Skip trailing blank lines (tmux pads the pane with empty lines),
    // then find the MOST RECENT line containing the ❯ prompt.
    // Only check that line — older prompt lines in scrollback are irrelevant.
    let prompt_line = pane_content
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('❯'));

    if let Some(line) = prompt_line
        && let Some(pos) = line.find('❯')
    {
        let after_prompt = &line[pos + '❯'.len_utf8()..];
        return !after_prompt.trim().is_empty();
    }

    false
}

/// Wait until the lead's input prompt is empty, polling periodically.
///
/// Returns `true` if the input cleared within the timeout, `false` if
/// the timeout expired with text still present. Either way, the caller
/// should proceed with the nudge — this is a courtesy delay, not a gate.
pub fn wait_for_empty_input(target: &str, timeout: std::time::Duration) -> bool {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let poll_interval = Duration::from_secs(3);

    loop {
        if let Some(content) = capture_pane(target) {
            if !has_input_text(&content) {
                return true;
            }
        } else {
            // Can't read pane — don't block, just proceed
            return true;
        }

        if start.elapsed() >= timeout {
            tracing::info!(
                "Lead input not empty after {}s, nudging anyway",
                timeout.as_secs()
            );
            return false;
        }

        std::thread::sleep(poll_interval);
    }
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
    // Build the new window name with parsed/abbreviated status
    let new_name = match status {
        Some(s) if !s.is_empty() => {
            // Parse status to extract keywords and task numbers
            let parsed = parse_status(s);
            format!("{}:{}", name, parsed)
        }
        _ => name.to_string(),
    };

    // Find the window by base name, since it may already have a status suffix
    // (e.g., "york:dev#3" instead of "york")
    let target = match find_window_target(session, name) {
        Some(t) => t,
        None => {
            tracing::debug!("Window {} not found in session {}", name, session);
            return Ok(());
        }
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

/// Find a tmux window target by base name (stripping any status suffix).
///
/// Returns `Some("session:index")` if found, `None` otherwise.
/// Uses window index for targeting since the window name may contain colons
/// that conflict with tmux's `session:window` target syntax.
fn find_window_target(session: &str, base_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index}:#{window_name}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let base_lower = base_name.to_lowercase();

    for line in stdout.lines() {
        // Format: "index:window_name" (e.g., "2:york:done#204")
        let (index, window_name) = line.split_once(':')?;
        let window_base = window_name.split(':').next().unwrap_or(window_name);
        if window_base.to_lowercase() == base_lower {
            return Some(format!("{}:{}", session, index));
        }
    }

    None
}

/// Send a bell character (\a) to a tmux pane to trigger a terminal notification.
///
/// Uses `tmux send-keys` with the BEL control character (ASCII 7). This triggers
/// the terminal's bell/notification mechanism (audible beep, visual flash, or
/// macOS notification depending on terminal settings).
pub fn send_bell(session: &str, name: &str) -> crate::Result<()> {
    let target = format!("{}:{}", session, name);

    // Send BEL character (ASCII 7) using octal escape
    let status = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", "\x07"])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send bell to tmux window: {}", target),
        });
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

/// List all tmux windows in the project session (including "lead").
///
/// Returns a vector of base window names (status suffixes stripped).
/// Used by the web UI to let users pick which window to view.
pub fn list_all_windows(session: &str) -> crate::Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()
        .map_err(Error::Io)?;

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
    let mut seen = std::collections::HashSet::new();
    let windows: Vec<String> = stdout
        .lines()
        .map(|s| {
            // Strip status suffix (e.g., "york:done#204" -> "york")
            s.split(':').next().unwrap_or(s).to_string()
        })
        .filter(|name| seen.insert(name.clone()))
        .collect();

    Ok(windows)
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
        .map(|s| {
            // Strip status suffix (e.g., "york:done#204" -> "york")
            // Windows get renamed via rename_window() with status info after a colon
            s.split(':').next().unwrap_or(s).to_string()
        })
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

/// Poll a tmux window to see if it survives startup.
///
/// Checks every 500ms for up to 3 seconds. Returns `true` if the window
/// is still alive at the end. This catches commands that fail shortly after
/// launch (e.g., `claude --continue` with no session to resume, which can
/// take 1-2 seconds to exit).
fn wait_for_window_stable(session: &str, name: &str) -> bool {
    for _ in 0..6 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        match window_exists(session, name) {
            Ok(true) => {}             // still alive, keep checking
            Ok(false) => return false, // died
            Err(_) => return false,
        }
    }
    true // survived 3 seconds of polling
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

/// Build coworker settings from agents/common-settings.json + agents/coworker-settings.json.
///
/// Merges base settings, replaces `{bin}` placeholders, and adds user's enabled plugins.
fn coworker_settings_json() -> serde_json::Value {
    let mut settings = load_settings(DEFAULT_COWORKER_SETTINGS);

    // Add user's plugins from ~/.claude/settings.json
    let user_plugins = read_user_plugins().unwrap_or_default();
    settings["enabledPlugins"] = user_plugins;

    settings
}

/// Write coworker settings to a shared file and return the path.
/// All coworkers use the same settings file.
fn write_coworker_settings_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("coworker-settings.json");
    let settings = coworker_settings_json();
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
/// Build the shell command string for launching a claude coworker.
///
/// Returns the full command including env vars, claude flags, and file references.
/// Extracted for testability — the actual tmux interaction happens in `spawn_claude`.
///
/// If `initial_prompt_file` is provided, the prompt is passed as a positional
/// argument via `$(cat file)` so Claude receives it at startup — no need to
/// send keystrokes after the TUI initializes.
fn build_claude_command(
    env_vars: &str,
    session_flag: &str,
    add_dir_flags: &str,
    settings_file: &std::path::Path,
    prompt_file: &std::path::Path,
    initial_prompt_file: Option<&std::path::Path>,
) -> String {
    let initial_prompt_arg = match initial_prompt_file {
        Some(path) => format!(" -p \"$(cat {})\"", path.display()),
        None => String::new(),
    };

    format!(
        "{}; claude --dangerously-skip-permissions{}{} --setting-sources project,local --settings {} --append-system-prompt \"$(cat {})\"{}",
        env_vars,
        session_flag,
        add_dir_flags,
        settings_file.display(),
        prompt_file.display(),
        initial_prompt_arg,
    )
}

/// If `resume` is true, passes `--continue` to claude to resume the previous
/// session from this worktree, preserving context from the last session.
///
/// If `initial_prompt` is provided, it's passed to claude as a `-p` argument
/// so the coworker receives the task at startup without relying on tmux
/// keystrokes (which can be lost if sent before the TUI is ready).
#[allow(clippy::too_many_arguments)]
pub fn spawn_claude(
    session: &str,
    name: &str,
    working_dir: &str,
    repo_name: Option<&str>,
    resume: bool,
    isolated_tasks: bool,
    additional_dirs: &[PathBuf],
    initial_prompt: Option<&str>,
) -> crate::Result<String> {
    // Build the claude command with settings for channel synchronization
    // and a system prompt for coworker identity and instructions
    let system_prompt = crate::agents::coworker_system_prompt(name);

    // Write system prompt and settings to files (avoids quoting issues)
    let prompt_file = write_coworker_prompt_file(name, &system_prompt)?;
    let settings_file = write_coworker_settings_file()?;

    // Generate a unique session ID for this coworker
    let coworker_session_id = uuid::Uuid::new_v4().to_string();

    // Build the claude command with session ID for task persistence
    // Use file paths for settings and prompt to avoid shell quoting issues
    // Set MIDTOWN_AGENT env var so the coworker's name appears in messages
    // Use --setting-sources project,local (plugins are now in --settings file)
    // Add --continue flag if resuming a previous session
    let session_flag = if resume {
        " --continue".to_string()
    } else {
        format!(" --session-id {}", coworker_session_id)
    };

    // Build environment variables - shared task list unless isolated
    // Isolated coworkers (e.g., reviewers) get their own task list so their
    // sub-tasks don't pollute the shared task list
    let env_vars = if isolated_tasks {
        format!("export MIDTOWN_AGENT='{}'", name)
    } else {
        let task_list_id = repo_name
            .map(crate::paths::task_list_id_for_repo)
            .unwrap_or_else(crate::paths::task_list_id);
        format!(
            "export MIDTOWN_AGENT='{}' CLAUDE_CODE_TASK_LIST_ID='{}'",
            name, task_list_id
        )
    };

    // Build --add-dir flags for multi-repo projects
    let add_dir_flags: String = additional_dirs
        .iter()
        .filter_map(|d| d.to_str())
        .map(|d| format!(" --add-dir {}", d))
        .collect();

    // Write initial prompt to file if provided (avoids shell quoting issues
    // and eliminates the timing race of sending keystrokes after spawn)
    let initial_prompt_file = initial_prompt
        .map(|p| write_coworker_initial_prompt_file(name, p))
        .transpose()?;

    let command = build_claude_command(
        &env_vars,
        &session_flag,
        &add_dir_flags,
        &settings_file,
        &prompt_file,
        initial_prompt_file.as_deref(),
    );

    // Create window with claude command running directly
    create_window(session, name, working_dir, Some(&command))?;

    // Set window tab color to match chat TUI team panel
    set_window_color(session, name)?;

    // Poll for window survival. tmux new-window returns success immediately
    // even if the command inside fails. `claude --continue` can take 1-2 seconds
    // to discover there's no session to resume and exit, so we poll repeatedly
    // over 3 seconds to catch failures the old 500ms check missed.
    let window_survived = wait_for_window_stable(session, name);

    if !window_survived {
        if resume {
            // --continue failed (likely no conversation to resume) — retry with fresh session
            tracing::warn!(
                "Tmux window {}:{} died with --continue, retrying with fresh session",
                session,
                name
            );

            let fresh_session_id = uuid::Uuid::new_v4().to_string();
            let fresh_session_flag = format!(" --session-id {}", fresh_session_id);
            let fresh_command = build_claude_command(
                &env_vars,
                &fresh_session_flag,
                &add_dir_flags,
                &settings_file,
                &prompt_file,
                initial_prompt_file.as_deref(),
            );

            create_window(session, name, working_dir, Some(&fresh_command))?;
            set_window_color(session, name)?;

            if !wait_for_window_stable(session, name) {
                return Err(Error::Rpc {
                    code: -32603,
                    message: format!(
                        "Tmux window {}:{} was created but immediately closed (retry without --continue also failed)",
                        session, name
                    ),
                });
            }

            return Ok(fresh_session_id);
        }

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

/// Build the shell command string for launching the lead Claude instance.
fn build_lead_command(
    task_list_id: &str,
    settings_file: &std::path::Path,
    prompt_file: &std::path::Path,
    add_dir_flags: &str,
) -> String {
    format!(
        "export CLAUDE_CODE_TASK_LIST_ID='{}'; claude --dangerously-skip-permissions --settings {} --append-system-prompt \"$(cat {})\"{}",
        task_list_id,
        settings_file.display(),
        prompt_file.display(),
        add_dir_flags,
    )
}

/// Write the Lead system prompt to a file and return the path.
pub fn write_lead_prompt_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("lead-prompt.md");
    std::fs::write(&path, crate::agents::lead_system_prompt()).map_err(Error::Io)?;

    Ok(path)
}

/// Build lead settings from agents/common-settings.json + agents/lead-settings.json.
///
/// Merges base settings and replaces `{bin}` placeholders.
pub fn lead_settings_json() -> serde_json::Value {
    load_settings(DEFAULT_LEAD_SETTINGS)
}

/// Write Lead settings to a file and return the path.
pub fn write_lead_settings_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("lead-settings.json");
    let settings = lead_settings_json();
    std::fs::write(&path, settings.to_string()).map_err(Error::Io)?;

    Ok(path)
}

/// Spawn the Lead Claude Code instance in a tmux window.
///
/// Creates (or recreates) the `lead` window in the given tmux session.
/// Always starts a fresh session — users can `/resume` interactively if desired.
pub fn spawn_lead(
    session: &str,
    working_dir: &str,
    project_name: &str,
    additional_dirs: &[PathBuf],
) -> crate::Result<()> {
    let task_list_id = crate::paths::task_list_id_for_repo(project_name);

    let prompt_file = write_lead_prompt_file()?;
    let settings_file = write_lead_settings_file()?;

    let add_dir_flags: String = additional_dirs
        .iter()
        .filter_map(|d| d.to_str())
        .map(|d| format!(" --add-dir {}", d))
        .collect();

    let command = build_lead_command(&task_list_id, &settings_file, &prompt_file, &add_dir_flags);

    create_window(session, "lead", working_dir, Some(&command))?;
    set_window_color(session, "lead")?;

    if !wait_for_window_stable(session, "lead") {
        return Err(Error::Rpc {
            code: -32603,
            message: format!(
                "Lead window {}:lead was created but immediately closed (command likely failed)",
                session,
            ),
        });
    }

    Ok(())
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

/// Set up the chat pane for the lead session.
///
/// Determines whether to use a split pane or separate window based on
/// the chat layout configuration and terminal width, then creates the
/// appropriate chat interface.
///
/// This is the single entry point for chat pane setup — used both during
/// initial session creation and when recreating the lead window.
pub fn setup_chat_pane(session: &str) {
    let bin_command = crate::config::get_bin_command();
    let (chat_layout, chat_min_width) = crate::config::get_chat_layout();

    let use_split = match chat_layout {
        crate::config::ChatLayout::Split => true,
        crate::config::ChatLayout::Window => false,
        crate::config::ChatLayout::Auto => get_session_width(session)
            .map(|w| w >= chat_min_width)
            .unwrap_or(true),
    };

    if use_split {
        if let Err(e) = create_chat_split(session, &bin_command) {
            eprintln!("Warning: Failed to create chat split: {}", e);
        }
    } else if let Err(e) = create_chat_window(session, &bin_command) {
        eprintln!("Warning: Failed to create chat window: {}", e);
    }
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
        let settings = coworker_settings_json();

        // Verify common settings are merged in
        assert_eq!(settings["autoUpdates"], false);

        // Verify coworker-specific settings
        assert_eq!(settings["editorMode"], "normal");

        // Verify no Stop hook (removed - daemon handles work assignment)
        assert!(settings["hooks"]["Stop"].is_null());

        // Verify PostToolUse hooks for task operations, questions, and insights
        let post_tool_hooks = &settings["hooks"]["PostToolUse"];
        assert!(post_tool_hooks.is_array());
        assert_eq!(post_tool_hooks.as_array().unwrap().len(), 4);

        // TaskUpdate hook
        assert_eq!(post_tool_hooks[0]["matcher"], "TaskUpdate");
        assert_eq!(
            post_tool_hooks[0]["hooks"][0]["command"],
            "midtown hook task"
        );

        // TaskCreate hook
        assert_eq!(post_tool_hooks[1]["matcher"], "TaskCreate");
        assert_eq!(
            post_tool_hooks[1]["hooks"][0]["command"],
            "midtown hook task"
        );

        // AskUserQuestion hook
        assert_eq!(post_tool_hooks[2]["matcher"], "AskUserQuestion");
        assert_eq!(
            post_tool_hooks[2]["hooks"][0]["command"],
            "midtown hook ask"
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

        // Verify {bin} placeholders were replaced (no literal "{bin}" in output)
        let serialized = settings.to_string();
        assert!(
            !serialized.contains("{bin}"),
            "settings should not contain unreplaced {{bin}} placeholders"
        );
    }

    #[test]
    fn test_lead_settings_json_is_valid() {
        let settings = lead_settings_json();

        // Verify common settings are merged in
        assert_eq!(settings["autoUpdates"], false);

        // Verify lead-specific hooks
        let stop_hooks = &settings["hooks"]["Stop"];
        assert!(stop_hooks.is_array());
        assert!(
            stop_hooks[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook lead-stop")
        );

        let post_tool_hooks = &settings["hooks"]["PostToolUse"];
        assert!(post_tool_hooks.is_array());
        assert!(
            post_tool_hooks[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook insight")
        );

        // Verify {bin} placeholders were replaced
        let serialized = settings.to_string();
        assert!(
            !serialized.contains("{bin}"),
            "settings should not contain unreplaced {{bin}} placeholders"
        );
    }

    // Note: coworker_system_prompt tests moved to src/agents.rs

    #[test]
    fn test_get_coworker_color_known_names() {
        assert_eq!(get_coworker_color("lead"), Some("brightyellow"));
        assert_eq!(get_coworker_color("lexington"), Some("cyan"));
        assert_eq!(get_coworker_color("park"), Some("green"));
        assert_eq!(get_coworker_color("madison"), Some("brightred"));
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
    fn test_parse_status_review() {
        assert_eq!(parse_status("reviewing PR #42"), "review#42");
        assert_eq!(parse_status("reviewing PR #5: Add auth"), "review#5");
        assert_eq!(parse_status("reviewing code"), "review");
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

    #[test]
    fn test_has_input_text_empty_prompt() {
        // Just a prompt with nothing after it — input is empty
        let pane = "Some previous output\n❯ ";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_prompt_no_space() {
        // Prompt with no trailing space — still empty
        let pane = "Some output\n❯";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_with_typed_text() {
        // User has typed something after the prompt
        let pane = "Some output\n❯ please add a feature that";
        assert!(has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_no_prompt_at_all() {
        // No prompt visible — treat as not having input text (safe to nudge)
        let pane = "Claude is working on your request...\nProcessing...";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_prompt_on_earlier_line() {
        // Most recent non-blank line without ❯ means the prompt with text
        // is from a PREVIOUS interaction. Only the most recent prompt matters.
        // Here "Output line 2" is the most recent non-blank line (no prompt),
        // so we look for the nearest prompt line — it has text.
        let pane = "Output line 1\n❯ some typed text\nOutput line 2";
        assert!(has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_old_prompt_text_new_prompt_empty() {
        // Old prompt had text, new prompt is empty — should be false
        // (the human already submitted, now on a fresh prompt)
        let pane = "❯ old command that was submitted\nSome output\n❯ ";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_blank_lines_after_prompt() {
        // Prompt followed by blank lines — still empty input
        let pane = "Output\n❯ \n\n";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_has_input_text_only_whitespace_after_prompt() {
        let pane = "Output\n❯    ";
        assert!(!has_input_text(pane));
    }

    #[test]
    fn test_build_claude_command_with_session_id() {
        let cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --session-id abc-123",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );
        assert!(cmd.contains("--session-id abc-123"));
        assert!(!cmd.contains("--continue"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--settings /tmp/settings.json"));
        assert!(cmd.contains("--append-system-prompt \"$(cat /tmp/prompt.txt)\""));
        assert!(cmd.starts_with("export MIDTOWN_AGENT='lex'; claude"));
        assert!(!cmd.contains("-p "));
    }

    #[test]
    fn test_build_claude_command_with_continue() {
        let cmd = build_claude_command(
            "export MIDTOWN_AGENT='park'",
            " --continue",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );
        assert!(cmd.contains("--continue"));
        assert!(!cmd.contains("--session-id"));
    }

    #[test]
    fn test_build_claude_command_with_add_dir_flags() {
        let cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --session-id abc",
            " --add-dir /other/repo --add-dir /another/repo",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );
        assert!(cmd.contains("--add-dir /other/repo"));
        assert!(cmd.contains("--add-dir /another/repo"));
    }

    #[test]
    fn test_build_claude_command_fresh_session_differs_from_continue() {
        // Simulates the fallback: original command uses --continue,
        // retry uses --session-id with a fresh UUID
        let continue_cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --continue",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );
        let fresh_cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --session-id fresh-uuid-here",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );

        // The continue command should have --continue, not --session-id
        assert!(continue_cmd.contains("--continue"));
        assert!(!continue_cmd.contains("--session-id"));

        // The fresh command should have --session-id, not --continue
        assert!(fresh_cmd.contains("--session-id fresh-uuid-here"));
        assert!(!fresh_cmd.contains("--continue"));

        // Both should share the same structure otherwise
        assert!(continue_cmd.contains("--setting-sources project,local"));
        assert!(fresh_cmd.contains("--setting-sources project,local"));
    }

    #[test]
    fn test_build_claude_command_with_initial_prompt() {
        let cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --session-id abc-123",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            Some(std::path::Path::new("/tmp/initial-prompt.md")),
        );
        assert!(cmd.contains("-p \"$(cat /tmp/initial-prompt.md)\""));
        // The -p flag should come after the other flags
        assert!(cmd.contains("--append-system-prompt"));
    }

    #[test]
    fn test_build_claude_command_without_initial_prompt() {
        let cmd = build_claude_command(
            "export MIDTOWN_AGENT='lex'",
            " --session-id abc-123",
            "",
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.txt"),
            None,
        );
        assert!(!cmd.contains("-p "));
    }
}
