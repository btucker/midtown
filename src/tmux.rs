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
        let end = status.floor_char_boundary(max_len.saturating_sub(3));
        format!("{}...", &status[..end])
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
///
/// Sends SIGTERM to the pane process before killing the window, because
/// Claude Code survives the SIGHUP that tmux sends on window destruction.
///
/// SAFETY: Refuses to kill the last window in a session, as that would
/// terminate the session (and potentially the tmux server if it's the only session).
pub fn kill_window(session: &str, name: &str) -> crate::Result<()> {
    // Idempotent: if window doesn't exist, it's already "killed"
    if !window_exists(session, name).unwrap_or(false) {
        tracing::debug!("Window {}:{} doesn't exist, nothing to kill", session, name);
        return Ok(());
    }

    // Safety check: don't kill the last window in the session
    let window_count = count_session_windows(session);
    if window_count <= 1 {
        tracing::warn!(
            "Refusing to kill window {}:{} - it's the last window in the session (count={})",
            session,
            name,
            window_count
        );
        return Ok(());
    }

    let target = format!("{}:{}", session, name);
    kill_window_by_target_unchecked(&target)
}

/// Count the number of windows in a tmux session.
fn count_session_windows(session: &str) -> usize {
    let output = match Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_id}"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return 0,
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count()
}

/// Kill a tmux window by its fully-qualified target string (e.g., "session:@0").
///
/// SIGTERMs pane processes first (Claude Code ignores SIGHUP), then kills the window.
///
/// SAFETY: Refuses to kill the last window in a session, as that would
/// terminate the session (and potentially the tmux server if it's the only session).
pub fn kill_window_by_target(target: &str) -> crate::Result<()> {
    // Extract session name from target (format: "session:window" or "session:@id")
    if let Some(session) = target.split(':').next() {
        let window_count = count_session_windows(session);
        if window_count <= 1 {
            tracing::warn!(
                "Refusing to kill window {} - it's the last window in session {} (count={})",
                target,
                session,
                window_count
            );
            return Ok(());
        }
    }

    kill_window_by_target_unchecked(target)
}

/// Kill a tmux window by target WITHOUT the last-window safety check.
///
/// Only use this when you've already verified the safety constraint.
fn kill_window_by_target_unchecked(target: &str) -> crate::Result<()> {
    // SIGTERM the pane process first — Claude Code ignores SIGHUP
    if let Ok(output) = Command::new("tmux")
        .args(["list-panes", "-t", target, "-F", "#{pane_pid}"])
        .output()
    {
        let pids: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        if !pids.is_empty() {
            let _ = Command::new("kill")
                .args(&pids)
                .stderr(std::process::Stdio::null())
                .status();
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    let status = Command::new("tmux")
        .args(["kill-window", "-t", target])
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

/// Collect PIDs of all pane processes in a session.
///
/// Returns (window_name, pid) pairs for every pane in the session.
pub fn session_pane_pids(session: &str) -> Vec<(String, u32)> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{window_name} #{pane_pid}",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(2, ' ');
                let name = parts.next()?.to_string();
                let pid = parts.next()?.parse().ok()?;
                Some((name, pid))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Send SIGTERM to all pane processes in a session, then SIGKILL any survivors.
///
/// Claude Code (node) installs a SIGHUP handler, so `tmux kill-session`
/// (which sends SIGHUP) leaves orphaned processes consuming memory and
/// potentially causing contention with other Claude instances. SIGTERM
/// triggers a clean shutdown.
///
/// Also kills child processes (Claude spawns node subprocesses) to ensure
/// complete cleanup even if the parent shell exits but children survive.
pub fn terminate_session_processes(session: &str) {
    let pids = session_pane_pids(session);
    if pids.is_empty() {
        return;
    }

    // Collect all pane PIDs and their descendants
    let mut all_pids: Vec<u32> = Vec::new();
    for (_, pid) in &pids {
        all_pids.push(*pid);
        // Also collect child processes (Claude's node subprocesses)
        all_pids.extend(get_descendant_pids(*pid));
    }
    all_pids.sort();
    all_pids.dedup();

    if all_pids.is_empty() {
        return;
    }

    // Send SIGTERM to all processes
    let pid_strings: Vec<String> = all_pids.iter().map(|p| p.to_string()).collect();
    let _ = Command::new("kill")
        .args(&pid_strings)
        .stderr(std::process::Stdio::null())
        .status();

    tracing::debug!(
        "Sent SIGTERM to {} processes in session {}",
        all_pids.len(),
        session
    );

    // Poll for processes to exit (up to 2 seconds)
    let poll_interval = std::time::Duration::from_millis(100);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
        let survivors: Vec<u32> = all_pids
            .iter()
            .copied()
            .filter(|&p| is_pid_alive(p))
            .collect();
        if survivors.is_empty() {
            tracing::debug!("All processes in session {} exited cleanly", session);
            return;
        }
    }

    // Force kill any survivors
    let survivors: Vec<u32> = all_pids
        .iter()
        .copied()
        .filter(|&p| is_pid_alive(p))
        .collect();
    if !survivors.is_empty() {
        tracing::warn!(
            "Force killing {} processes that didn't exit: {:?}",
            survivors.len(),
            survivors
        );
        let pid_strings: Vec<String> = survivors.iter().map(|p| p.to_string()).collect();
        let _ = Command::new("kill")
            .arg("-9")
            .args(&pid_strings)
            .stderr(std::process::Stdio::null())
            .status();

        // Brief wait for SIGKILL to take effect
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Get all descendant PIDs of a process (children, grandchildren, etc).
///
/// Uses `pgrep -P` to find immediate children, then recursively finds their children.
fn get_descendant_pids(parent_pid: u32) -> Vec<u32> {
    let mut descendants = Vec::new();
    let mut to_check = vec![parent_pid];

    while let Some(pid) = to_check.pop() {
        // Find immediate children of this PID
        let output = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output();

        if let Ok(o) = output
            && o.status.success()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Ok(child_pid) = line.trim().parse::<u32>()
                    && !descendants.contains(&child_pid)
                {
                    descendants.push(child_pid);
                    to_check.push(child_pid); // Check for grandchildren
                }
            }
        }
    }

    descendants
}

/// Check if a process is still alive.
pub fn is_pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get the parent PID of a process.
pub fn get_ppid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;

    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Find orphaned processes matching a pattern.
///
/// Returns PIDs of processes that:
/// 1. Match the given regex pattern in their command line
/// 2. Have PPID=1 (orphaned - no legitimate parent)
/// 3. Are NOT tmux processes (to avoid killing the tmux server)
///
/// This is conservative: only truly orphaned processes are returned.
/// The tmux exclusion is critical because `tmux new-session` commands may
/// match patterns like "claude" in their arguments, but killing the tmux
/// server would destroy all coworker windows.
pub fn find_orphaned_processes(pattern: &str) -> Vec<u32> {
    // Find PIDs matching the pattern
    let output = match Command::new("pgrep").args(["-f", pattern]).output() {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();

    // Filter to only orphaned processes (PPID=1) that are NOT tmux
    // Bug fix: The pattern may match tmux server processes because the
    // `tmux new-session` command line includes "claude" in its arguments.
    // Killing the tmux server would destroy all windows, so we must exclude it.
    pids.into_iter()
        .filter(|&pid| {
            // Must be orphaned (PPID=1)
            if get_ppid(pid) != Some(1) {
                return false;
            }
            // Must NOT be a tmux process
            let is_tmux = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output()
                .ok()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .starts_with("tmux")
                })
                .unwrap_or(false);
            if is_tmux {
                tracing::debug!(pid = pid, "Skipping tmux process in orphan cleanup");
                return false;
            }
            true
        })
        .collect()
}

/// Kill orphaned processes matching a pattern.
///
/// Sends SIGTERM first, waits briefly, then SIGKILL to any survivors.
/// Returns the number of processes killed.
///
/// Only kills processes that are truly orphaned (PPID=1) to avoid
/// killing legitimate processes the user may have started.
pub fn kill_orphaned_processes(pattern: &str) -> usize {
    let orphan_pids = find_orphaned_processes(pattern);

    if orphan_pids.is_empty() {
        return 0;
    }

    let count = orphan_pids.len();

    // Log what we're about to kill for debugging
    for &pid in &orphan_pids {
        // Get process command line for debugging
        let cmdline = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        tracing::warn!(
            pid = pid,
            cmdline = %cmdline,
            pattern = %pattern,
            "ORPHAN_CLEANUP: killing orphaned claude process"
        );
    }

    // Send SIGTERM to orphaned processes
    let pid_strings: Vec<String> = orphan_pids.iter().map(|p| p.to_string()).collect();
    let _ = Command::new("kill")
        .args(&pid_strings)
        .stderr(std::process::Stdio::null())
        .status();

    // Wait briefly for processes to exit
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Force kill any survivors
    let survivors: Vec<String> = orphan_pids
        .iter()
        .filter(|&&pid| is_pid_alive(pid))
        .map(|p| p.to_string())
        .collect();

    if !survivors.is_empty() {
        let _ = Command::new("kill")
            .arg("-9")
            .args(&survivors)
            .stderr(std::process::Stdio::null())
            .status();
    }

    count
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

/// Check whether a tmux pane has any visible output (non-whitespace content).
///
/// Calls `capture_pane()` and returns `false` if every line is empty or
/// whitespace-only. This detects "zombie" windows where the process started
/// but the TUI never rendered — the pane is entirely blank.
///
/// Used by `spawn_claude()` to detect blank-pane failures at spawn time,
/// and by the daemon health check to find zombie coworkers.
pub fn pane_has_output(target: &str) -> bool {
    match capture_pane(target) {
        Some(content) => content_has_output(&content),
        None => false,
    }
}

/// Pure content check: returns `true` if any line has non-whitespace characters.
///
/// Extracted from `pane_has_output` for unit testing without tmux.
pub fn content_has_output(content: &str) -> bool {
    content.lines().any(|line| !line.trim().is_empty())
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

/// Extract the text after the prompt symbol (❯) from pane content.
///
/// Returns None if no prompt is visible or the input is empty.
pub fn get_input_text(pane_content: &str) -> Option<String> {
    // Skip trailing blank lines and find the most recent prompt line
    let prompt_line = pane_content
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains('❯'));

    if let Some(line) = prompt_line
        && let Some(pos) = line.find('❯')
    {
        let after_prompt = line[pos + '❯'.len_utf8()..].trim();
        if !after_prompt.is_empty() {
            return Some(after_prompt.to_string());
        }
    }

    None
}

/// Wait for a safe opportunity to nudge, respecting user input.
///
/// This implements the user-input-aware nudge waiting logic:
/// 1. If input is empty → safe to nudge immediately
/// 2. If input contains (mostly) the last nudge text → safe to overwrite
/// 3. If user content detected → wait until it hasn't changed for `stable_duration`
/// 4. After `max_wait` total time, proceed anyway (don't block forever)
///
/// Returns `true` if safe to nudge, `false` if we timed out with active typing.
/// The caller should still nudge on `false` but may want to log it.
pub fn wait_for_nudge_safe(
    target: &str,
    last_nudge_text: Option<&str>,
    stable_duration: std::time::Duration,
    max_wait: std::time::Duration,
) -> bool {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let poll_interval = Duration::from_secs(3);
    let mut last_input: Option<String> = None;
    let mut last_change_time = Instant::now();

    loop {
        let content = match capture_pane(target) {
            Some(c) => c,
            None => {
                // Can't read pane — don't block, proceed
                return true;
            }
        };

        // Check 1: No input text → safe to nudge
        if !has_input_text(&content) {
            tracing::debug!("Input empty, safe to nudge");
            return true;
        }

        // Get the current input text
        let current_input = get_input_text(&content);

        // Check 2: Input is (mostly) the last nudge text → safe to overwrite
        if let (Some(input), Some(last_nudge)) = (&current_input, last_nudge_text) {
            // Check if input starts with or mostly matches the last nudge
            let check_len = last_nudge.floor_char_boundary(last_nudge.len().min(30));
            if check_len > 0 && input.starts_with(&last_nudge[..check_len]) {
                tracing::debug!("Input contains last nudge text, safe to overwrite");
                return true;
            }
        }

        // Check 3: User content detected - track stability
        if current_input != last_input {
            // Content changed, reset stability timer
            last_change_time = Instant::now();
            last_input = current_input;
            tracing::debug!("Input changed, resetting stability timer");
        } else if last_change_time.elapsed() >= stable_duration {
            // Content stable for long enough, safe to append
            tracing::debug!(
                "Input stable for {}s, safe to append",
                stable_duration.as_secs()
            );
            return true;
        }

        // Check 4: Max wait exceeded
        if start.elapsed() >= max_wait {
            tracing::info!(
                "Nudge wait timed out after {}s with active user input",
                max_wait.as_secs()
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
                &nudge_text[..nudge_text.floor_char_boundary(20)]
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
/// 1. Send text literally (with -l flag)
/// 2. Wait 500ms for paste to complete
/// 3. Wait 100ms, then send Enter with retry and verification
///    (up to 3 attempts, 200ms between)
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

    // 2. Wait 500ms for paste to complete
    thread::sleep(Duration::from_millis(500));

    // 3. Wait 100ms, then send Enter with retry and verification (up to 3 attempts, 200ms between)
    thread::sleep(Duration::from_millis(100));
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

    rename_window_raw(session, name, &new_name)
}

/// Set a tmux window name directly without parsing through `parse_status()`.
///
/// Used when the caller already has a formatted status string (e.g., from
/// a structured state file that produces "dev#42" directly).
pub fn rename_window_formatted(
    session: &str,
    name: &str,
    formatted_status: &str,
) -> crate::Result<()> {
    let new_name = if formatted_status.is_empty() {
        name.to_string()
    } else {
        format!("{}:{}", name, formatted_status)
    };

    rename_window_raw(session, name, &new_name)
}

/// Internal: set the tmux window name to `new_name`.
fn rename_window_raw(session: &str, name: &str, new_name: &str) -> crate::Result<()> {
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
        .args(["rename-window", "-t", &target, new_name])
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

/// Count how many tmux windows in a session match the given name.
///
/// Uses `#{window_id}` to uniquely identify windows, since multiple windows
/// can share the same name. Returns the count and the list of window IDs.
pub fn count_windows_by_name(session: &str, name: &str) -> crate::Result<(usize, Vec<String>)> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_id}:#{window_name}",
        ])
        .output()
        .map_err(Error::Io)?;

    if !output.status.success() {
        return Ok((0, vec![]));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let name_lower = name.to_lowercase();

    let matching_ids: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            // Format: "@0:lead" or "@3:lead:dev#5"
            let (id, rest) = line.split_once(':')?;
            let base_name = rest.split(':').next().unwrap_or(rest).to_lowercase();
            if base_name == name_lower {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect();

    let count = matching_ids.len();
    Ok((count, matching_ids))
}

/// Kill ALL tmux windows in a session that match the given name.
///
/// Unlike `kill_window` which uses `session:name` (tmux only targets the first
/// match), this function lists windows by ID and kills each one individually.
/// This prevents duplicate windows from accumulating when the same name is
/// created multiple times (e.g., during restart races).
pub fn kill_all_windows_by_name(session: &str, name: &str) -> crate::Result<usize> {
    let (count, ids) = count_windows_by_name(session, name)?;

    if count == 0 {
        return Ok(0);
    }

    // Safety check: don't kill if these are the only windows in the session
    let total_windows = count_session_windows(session);
    if total_windows <= count {
        tracing::warn!(
            "Refusing to kill all {} '{}' windows - would leave session {} empty (total={})",
            count,
            name,
            session,
            total_windows
        );
        return Ok(0);
    }

    for id in &ids {
        // Kill by window ID (e.g., "@0") which is always unique
        let target = format!("{}:{}", session, id);

        // SIGTERM pane processes first — Claude Code ignores SIGHUP
        if let Ok(output) = Command::new("tmux")
            .args(["list-panes", "-t", &target, "-F", "#{pane_pid}"])
            .output()
        {
            let pids: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            if !pids.is_empty() {
                let _ = Command::new("kill")
                    .args(&pids)
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        let _ = Command::new("tmux")
            .args(["kill-window", "-t", &target])
            .status();
    }

    // Brief pause to let tmux clean up
    if count > 0 {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    Ok(count)
}

/// Poll a tmux window to see if it survives startup.
///
/// Checks every 500ms for up to 3 seconds. Returns `true` if the window
/// is still alive at the end. This catches commands that fail shortly after
/// launch (e.g., `claude --continue` with no session to resume, which can
/// take 1-2 seconds to exit).
pub fn wait_for_window_stable(session: &str, name: &str) -> bool {
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
pub fn write_coworker_settings_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("coworker-settings.json");
    let settings = coworker_settings_json();
    std::fs::write(&path, settings.to_string()).map_err(Error::Io)?;

    Ok(path)
}

// Re-export launch types for backward compatibility.
// The canonical definitions live in `crate::launch`. These re-exports allow
// existing `crate::tmux::ClaudeLaunchConfig` references to keep working during
// the migration to `crate::launch::LaunchConfig`.
pub use crate::launch::CoworkerRole;
pub use crate::launch::LaunchCommand;
pub use crate::launch::LaunchConfig as ClaudeLaunchConfig;
pub use crate::launch::SessionMode;
pub use crate::launch::TaskMode;

/// Spawn a Claude Code coworker in a tmux window.
///
/// Takes a `ClaudeLaunchConfig` that fully describes how to launch Claude,
/// writes the system prompt and settings to files, builds the shell command,
/// and creates the tmux window. Includes retry logic for blank-pane failures.
///
/// Returns the session ID for task symlink management.
pub fn spawn_claude(
    session: &str,
    working_dir: &str,
    config: &ClaudeLaunchConfig,
) -> crate::Result<String> {
    // Build the claude command with settings for channel synchronization
    // and a system prompt for coworker identity and instructions.
    // Reviewers get a specialized prompt that includes reviewer.md instructions.
    let system_prompt = match config.role {
        CoworkerRole::Reviewer => crate::agents::reviewer_system_prompt(&config.name),
        CoworkerRole::Coworker => crate::agents::coworker_system_prompt(&config.name),
    };

    // Ensure agent-teams infrastructure exists before launch.
    // Upserts this coworker into the team config and creates the inboxes
    // directory so Claude Code can discover its team membership and
    // receive mailbox messages.
    if let Some(ref team_name) = config.team_name {
        let member = crate::mailbox::TeamMember {
            name: config.name.clone(),
            agent_id: crate::mailbox::agent_id(&config.name, team_name),
            agent_type: match config.role {
                CoworkerRole::Reviewer => "reviewer".to_string(),
                CoworkerRole::Coworker => "coworker".to_string(),
            },
        };
        if let Err(e) = crate::mailbox::upsert_team_member(team_name, member) {
            tracing::warn!("Failed to set up team config for {}: {}", config.name, e);
            // Non-fatal: coworker can still run without mailbox
        }
    }

    // Write system prompt and settings to files (avoids quoting issues)
    let prompt_file = write_coworker_prompt_file(&config.name, &system_prompt)?;
    let settings_file = write_coworker_settings_file()?;

    // Write initial prompt to file if provided (avoids shell quoting issues
    // and eliminates the timing race of sending keystrokes after spawn)
    let initial_prompt_file = config
        .initial_prompt
        .as_deref()
        .map(|p| write_coworker_initial_prompt_file(&config.name, p))
        .transpose()?;

    let launch =
        config.to_shell_command(&settings_file, &prompt_file, initial_prompt_file.as_deref());
    let coworker_session_id = launch.session_id.unwrap_or_default();

    // Kill any existing windows with this name to prevent duplicates.
    // Uses kill_all_windows_by_name (by window ID) instead of kill_window
    // (by name) because tmux's `session:name` target fails with ambiguous
    // matches. Mirrors the guard in spawn_lead().
    match kill_all_windows_by_name(session, &config.name) {
        Ok(n) if n > 0 => {
            tracing::warn!(
                "Killed {} existing '{}' window(s) before spawning",
                n,
                config.name
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                "Failed to clean up existing '{}' windows: {}",
                config.name,
                e
            );
        }
    }

    // Create window with claude command running directly
    create_window(
        session,
        &config.name,
        working_dir,
        Some(&launch.shell_command),
    )?;

    // Set window tab color to match chat TUI team panel
    set_window_color(session, &config.name)?;

    // Poll for window survival. tmux new-window returns success immediately
    // even if the command inside fails. `claude --continue` can take 1-2 seconds
    // to discover there's no session to resume and exit, so we poll repeatedly
    // over 3 seconds to catch failures the old 500ms check missed.
    let mut window_survived = wait_for_window_stable(session, &config.name);

    // If the window survived but the pane is blank (no terminal output),
    // the process started but the TUI never rendered. Kill it and let the
    // retry logic below handle respawning.
    if window_survived {
        let target = format!("{}:{}", session, config.name);
        let mut has_output = false;
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if pane_has_output(&target) {
                has_output = true;
                break;
            }
        }
        if !has_output {
            // Capture diagnostics before killing — helps trace root cause
            let pane_dims = Command::new("tmux")
                .args([
                    "list-panes",
                    "-t",
                    &target,
                    "-F",
                    "#{pane_width}x#{pane_height} pid=#{pane_pid}",
                ])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let other_windows = list_windows(session).unwrap_or_default().len();

            let pane_pid = Command::new("tmux")
                .args(["list-panes", "-t", &target, "-F", "#{pane_pid}"])
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                });

            let child_procs = pane_pid
                .map(|pid| {
                    Command::new("pgrep")
                        .args(["-P", &pid.to_string()])
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            tracing::warn!(
                "BLANK PANE DIAGNOSTIC {}:{} — session_mode={:?}, pane={}, other_windows={}, \
                 pane_pid={:?}, children=[{}], raw_content={:?}",
                session,
                config.name,
                config.session_mode,
                pane_dims,
                other_windows,
                pane_pid,
                child_procs,
                capture_pane(&target)
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>(),
            );

            let _ = kill_window(session, &config.name);
            window_survived = false;
        }
    }

    if !window_survived {
        // First attempt failed — always retry with a fresh session.
        // This handles both --continue/--resume failures (stale session) and
        // intermittent blank-pane TUI init failures on fresh sessions.
        tracing::warn!(
            "Tmux window {}:{} failed on first attempt (session_mode={:?}), retrying with fresh session",
            session,
            config.name,
            config.session_mode,
        );

        let retry_config = config.as_fresh_retry();
        let retry_launch = retry_config.to_shell_command(
            &settings_file,
            &prompt_file,
            initial_prompt_file.as_deref(),
        );
        let fresh_session_id = retry_launch.session_id.unwrap_or_default();

        create_window(
            session,
            &config.name,
            working_dir,
            Some(&retry_launch.shell_command),
        )?;
        set_window_color(session, &config.name)?;

        if !wait_for_window_stable(session, &config.name) {
            return Err(Error::Rpc {
                code: -32603,
                message: format!(
                    "Tmux window {}:{} was created but immediately closed (retry also failed)",
                    session, config.name
                ),
            });
        }

        // Set initial window status for reviewers
        if let Some(pr) = config.pr_number {
            let _ = rename_window_formatted(session, &config.name, &format!("review#{}", pr));
        }

        return Ok(fresh_session_id);
    }

    // Set initial window status for reviewers
    if let Some(pr) = config.pr_number {
        let _ = rename_window_formatted(session, &config.name, &format!("review#{}", pr));
    }

    Ok(coworker_session_id)
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
    // Kill ALL existing lead windows to prevent duplicates.
    // Using kill_all_windows_by_name instead of kill_window because tmux's
    // `kill-window -t session:name` only targets the first match when
    // multiple windows share the same name. This can happen if health check
    // races during restart create extras.
    match kill_all_windows_by_name(session, "lead") {
        Ok(n) if n > 0 => {
            tracing::warn!("Killed {} existing lead window(s) before respawn", n);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Failed to clean up existing lead windows: {}", e);
        }
    }

    // Clear stale task ID mappings from previous /resume sessions.
    // Fresh sessions use CLAUDE_CODE_TASK_LIST_ID correctly, so old mappings
    // from a /resume cycle are no longer valid and could cause mis-remapping.
    crate::tasks::clear_lead_task_id_map(project_name);

    let prompt_file = write_lead_prompt_file()?;
    let settings_file = write_lead_settings_file()?;

    let config = ClaudeLaunchConfig {
        name: "lead".to_string(),
        session_mode: SessionMode::Fresh,
        task_mode: TaskMode::Shared {
            repo_name: project_name.to_string(),
        },
        role: CoworkerRole::Coworker, // Lead uses its own prompt; role only affects coworker spawns
        initial_prompt: None,
        additional_dirs: additional_dirs.to_vec(),
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None, // Lead is human-facing, not an agent-teams member
    };

    // Allow tests/CI to override the lead command (claude isn't available in CI)
    let command = if let Ok(test_cmd) = std::env::var("MIDTOWN_LEAD_COMMAND") {
        test_cmd
    } else {
        config
            .to_shell_command(&settings_file, &prompt_file, None)
            .shell_command
    };

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

/// Minimum terminal width enforced when web viewers resize a window.
///
/// Prevents extremely narrow windows that would break terminal UIs.
pub const MIN_RESIZE_COLS: u16 = 80;

/// Resize a tmux window's width to the specified number of columns.
///
/// Used by the web UI viewer tracking system to match the window width
/// to the widest connected viewer's viewport. Enforces a minimum of
/// `MIN_RESIZE_COLS` to prevent breaking terminal UIs.
///
/// Returns `Ok(())` if the resize succeeds or the window doesn't exist.
pub fn resize_window_width(session: &str, window_name: &str, cols: u16) -> crate::Result<()> {
    let cols = cols.max(MIN_RESIZE_COLS);

    let target = match find_window_target(session, window_name) {
        Some(t) => t,
        None => {
            tracing::debug!(
                "Cannot resize window {} — not found in session {}",
                window_name,
                session
            );
            return Ok(());
        }
    };

    let status = Command::new("tmux")
        .args(["resize-window", "-t", &target, "-x", &cols.to_string()])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        tracing::debug!("Failed to resize tmux window {} to {} cols", target, cols);
    }

    Ok(())
}

/// Reset a tmux window to automatic sizing.
///
/// Uses `tmux resize-window -A` which sizes the window to the smallest
/// attached client. Called when all web viewers disconnect from a window.
pub fn reset_window_size(session: &str, window_name: &str) -> crate::Result<()> {
    let target = match find_window_target(session, window_name) {
        Some(t) => t,
        None => {
            tracing::debug!(
                "Cannot reset window {} — not found in session {}",
                window_name,
                session
            );
            return Ok(());
        }
    };

    let status = Command::new("tmux")
        .args(["resize-window", "-t", &target, "-A"])
        .status()
        .map_err(Error::Io)?;

    if !status.success() {
        tracing::debug!("Failed to reset tmux window {} size", target);
    }

    Ok(())
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
        // Check if lead window already has a chat split pane (pane .1).
        // Without this guard, each call to ensure_lead_has_settings creates
        // an additional split, progressively shrinking the lead pane until
        // the TUI can't render.
        if lead_has_chat_pane(session) {
            respawn_chat_split(session, &bin_command);
        } else if let Err(e) = create_chat_split(session, &bin_command) {
            eprintln!("Warning: Failed to create chat split: {}", e);
        }
    } else if window_exists(session, "chat").unwrap_or(false) {
        respawn_chat_window(session, &bin_command);
    } else if let Err(e) = create_chat_window(session, &bin_command) {
        eprintln!("Warning: Failed to create chat window: {}", e);
    }
}

/// Check if the lead window already has a chat split pane (more than 1 pane).
fn lead_has_chat_pane(session: &str) -> bool {
    let target = format!("{}:lead", session);
    let output = Command::new("tmux")
        .args(["list-panes", "-t", &target, "-F", "#{pane_index}"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let count = String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count();
            count > 1
        }
        _ => false,
    }
}

/// Respawn the existing chat split pane (lead.1) with fresh chat command.
fn respawn_chat_split(session: &str, bin_command: &str) {
    let chat_pane = format!("{}:lead.1", session);
    let chat_cmd = format!("{} chat", bin_command);
    let _ = Command::new("tmux")
        .args(["respawn-pane", "-k", "-t", &chat_pane, &chat_cmd])
        .status();
}

/// Respawn the existing chat window with fresh chat command.
fn respawn_chat_window(session: &str, bin_command: &str) {
    let chat_target = format!("{}:chat", session);
    let chat_cmd = format!("{} chat", bin_command);
    let _ = Command::new("tmux")
        .args(["respawn-pane", "-k", "-t", &chat_target, &chat_cmd])
        .status();
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

        // CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS is blocklisted from settings.json by Claude Code;
        // it's now exported as a real shell env var in to_shell_command() instead.
        assert!(settings["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"].is_null());

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

        // CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS is blocklisted from settings.json by Claude Code;
        // it's now exported as a real shell env var in to_shell_command() instead.
        assert!(settings["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"].is_null());

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
        // Lead has only the catch-all insight hook (task hooks removed —
        // Lead now uses `midtown task` CLI instead of TaskCreate/TaskUpdate tools)
        assert!(
            post_tool_hooks[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook insight"),
            "PostToolUse hook should be the catch-all insight hook"
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
    fn test_min_resize_cols_is_80() {
        assert_eq!(MIN_RESIZE_COLS, 80);
    }

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

    #[test]
    fn test_truncate_status_multibyte() {
        // 4-byte emoji repeated — slicing at arbitrary byte offsets panics
        let emoji_status = "😀".repeat(10); // 40 bytes
        let result = truncate_status(&emoji_status, 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("..."));
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
    fn test_is_nudge_stuck_multibyte_nudge_text() {
        // 25 emojis (each 4 bytes = 100 bytes total, but only 25 chars).
        // The function truncates to 20 chars, so the first 20 emojis are checked.
        let emojis = "🎉🎊🎈🎁🎂🎃🎄🎅🎆🎇🎋🎌🎍🎎🎏🎐🎑🎒🎓🎠🎡🎢🎣🎤🎥";
        let pane_content = &format!("❯ {}", emojis);
        assert!(is_nudge_stuck(pane_content, emojis));
    }

    #[test]
    fn test_is_nudge_stuck_multibyte_boundary_split() {
        // This specifically triggers the bug: 19 ASCII bytes + a 4-byte emoji
        // means byte position 20 lands inside the emoji
        let nudge_text = "1234567890123456789é extra text after the boundary";
        let pane_content = "❯ 1234567890123456789é extra text";
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

    // --- ClaudeLaunchConfig tests ---

    #[test]
    fn test_launch_config_lead_omits_setting_sources() {
        let config = ClaudeLaunchConfig {
            name: "lead".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Shared {
                repo_name: "myrepo".to_string(),
            },
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            !result.shell_command.contains("--setting-sources"),
            "lead must not restrict setting sources"
        );
        assert!(
            result.shell_command.contains("exec claude"),
            "lead must use exec"
        );
        assert!(
            result.shell_command.contains("CLAUDE_CODE_TASK_LIST_ID="),
            "lead must have shared task list"
        );
    }

    #[test]
    fn test_launch_config_fresh_session_produces_session_id_flag() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains("--session-id "),
            "fresh session must use --session-id"
        );
        assert!(
            !result.shell_command.contains("--continue"),
            "fresh session must not use --continue"
        );
        assert!(
            !result.shell_command.contains("--resume "),
            "fresh session must not use --resume"
        );
        assert!(
            result.session_id.is_some(),
            "fresh session must return a session ID"
        );
    }

    #[test]
    fn test_launch_config_resume_produces_continue_flag() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Resume,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains(" --continue"),
            "resume must use --continue"
        );
        assert!(
            !result.shell_command.contains("--session-id "),
            "resume must not use --session-id"
        );
        assert!(
            result.session_id.is_none(),
            "resume must not return a session ID"
        );
    }

    #[test]
    fn test_launch_config_resume_session_produces_resume_flag() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::ResumeSession("abc-123".to_string()),
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains("--resume abc-123"),
            "must use --resume with session id"
        );
        assert!(
            result.session_id.is_none(),
            "resume session must not return a new session ID"
        );
    }

    #[test]
    fn test_launch_config_isolated_omits_task_list_env() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            !result.shell_command.contains("CLAUDE_CODE_TASK_LIST_ID"),
            "isolated must not set task list ID"
        );
        assert!(
            result.shell_command.contains("MIDTOWN_AGENT='park'"),
            "must always set agent name"
        );
    }

    #[test]
    fn test_launch_config_shared_includes_task_list_env() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Shared {
                repo_name: "myrepo".to_string(),
            },
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains("CLAUDE_CODE_TASK_LIST_ID="),
            "shared must set task list ID"
        );
    }

    #[test]
    fn test_launch_config_includes_claude_config_dir() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        // CLAUDE_CONFIG_DIR must be set from auth profile for account isolation
        assert!(
            result.shell_command.contains("CLAUDE_CONFIG_DIR="),
            "must set CLAUDE_CONFIG_DIR from auth profile"
        );
        // Path should include .midtown/auth/
        assert!(
            result.shell_command.contains(".midtown/auth/"),
            "CLAUDE_CONFIG_DIR should point to auth profile directory"
        );
    }

    #[test]
    fn test_launch_config_initial_prompt_is_positional_not_flag() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: Some("Do the thing".to_string()),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            Some(std::path::Path::new("/tmp/initial-prompt.md")),
        );
        // PR #447 regression: must NEVER use -p or --print
        assert!(
            !result.shell_command.contains("-p "),
            "must not use -p flag"
        );
        assert!(
            !result.shell_command.contains("--print"),
            "must not use --print flag"
        );
        // Prompt file must appear as last positional arg
        assert!(
            result
                .shell_command
                .contains("\"$(cat /tmp/initial-prompt.md)\""),
            "prompt must be passed via $(cat file)"
        );
    }

    #[test]
    fn test_launch_config_no_prompt_has_no_trailing_cat() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        // Count occurrences of $(cat — should be exactly 1 (the system prompt)
        let cat_count = result.shell_command.matches("$(cat ").count();
        assert_eq!(
            cat_count, 1,
            "without initial_prompt, only system prompt should use $(cat)"
        );
    }

    #[test]
    fn test_launch_config_additional_dirs() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![PathBuf::from("/extra/repo1"), PathBuf::from("/extra/repo2")],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains("--add-dir /extra/repo1"),
            "must include first additional dir"
        );
        assert!(
            result.shell_command.contains("--add-dir /extra/repo2"),
            "must include second additional dir"
        );
    }

    #[test]
    fn test_launch_config_uses_exec() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result.shell_command.contains("exec claude"),
            "must use exec to replace shell process"
        );
    }

    #[test]
    fn test_launch_config_includes_dangerously_skip_permissions() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result
                .shell_command
                .contains("--dangerously-skip-permissions"),
            "coworkers must skip permissions"
        );
    }

    #[test]
    fn test_launch_config_as_fresh_retry() {
        let config = ClaudeLaunchConfig {
            name: "park".to_string(),
            session_mode: SessionMode::Resume,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::default(),
            initial_prompt: Some("task prompt".to_string()),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: None,
        };
        let retry = config.as_fresh_retry();
        assert_eq!(retry.session_mode, SessionMode::Fresh);
        assert_eq!(retry.name, "park");
        assert_eq!(retry.initial_prompt, Some("task prompt".to_string()));
    }

    #[test]
    fn test_reviewer_config_sets_pr_number() {
        let config = ClaudeLaunchConfig::reviewer("york".to_string(), 42);
        assert_eq!(config.pr_number, Some(42));
        assert_eq!(config.name, "york");
        // Initial window name would be "york:review#42" after spawn_claude runs
    }

    #[test]
    fn test_developer_configs_have_no_pr_number() {
        // Regular coworker
        let coworker = ClaudeLaunchConfig::coworker(
            "park".to_string(),
            "myrepo".to_string(),
            SessionMode::Fresh,
            None,
        );
        assert_eq!(coworker.pr_number, None);

        // PR handoff coworker (not an isolated reviewer)
        let handoff = ClaudeLaunchConfig::pr_handoff(
            "york".to_string(),
            "myrepo",
            "session-123".to_string(),
            42,
            "feature/branch",
            "original-author",
        );
        assert_eq!(handoff.pr_number, None);
    }

    #[test]
    fn test_coworker_config_includes_agent_teams_flags() {
        let config = ClaudeLaunchConfig::coworker(
            "lexington".to_string(),
            "myrepo".to_string(),
            SessionMode::Fresh,
            None,
        );
        assert_eq!(
            config.team_name,
            Some("midtown-myrepo".to_string()),
            "coworker must have team name set"
        );
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            result
                .shell_command
                .contains("--agent-id lexington@midtown-myrepo"),
            "must include --agent-id flag"
        );
        assert!(
            result.shell_command.contains("--agent-name lexington"),
            "must include --agent-name flag"
        );
        assert!(
            result.shell_command.contains("--team-name midtown-myrepo"),
            "must include --team-name flag"
        );
        assert!(
            result
                .shell_command
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1"),
            "must export CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS as shell env var"
        );
    }

    #[test]
    fn test_lead_config_omits_agent_teams_flags() {
        let config = ClaudeLaunchConfig {
            name: "lead".to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Shared {
                repo_name: "myrepo".to_string(),
            },
            role: CoworkerRole::default(),
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
        );
        assert!(
            !result.shell_command.contains("--agent-id"),
            "lead must not have --agent-id flag"
        );
        assert!(
            !result.shell_command.contains("--agent-name"),
            "lead must not have --agent-name flag"
        );
        assert!(
            !result.shell_command.contains("--team-name"),
            "lead must not have --team-name flag"
        );
        assert!(
            !result
                .shell_command
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"),
            "lead must not export CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"
        );
    }

    #[test]
    fn test_reviewer_config_omits_agent_teams_flags() {
        let config = ClaudeLaunchConfig::reviewer("york".to_string(), 42);
        assert_eq!(
            config.team_name, None,
            "reviewer must not have team name (short-lived)"
        );
    }

    #[test]
    fn test_pr_handoff_config_includes_agent_teams_flags() {
        let config = ClaudeLaunchConfig::pr_handoff(
            "york".to_string(),
            "myrepo",
            "session-123".to_string(),
            42,
            "feature/branch",
            "original-author",
        );
        assert_eq!(
            config.team_name,
            Some("midtown-myrepo".to_string()),
            "pr_handoff must have team name set"
        );
    }

    /// Regression test: orphan cleanup must not kill tmux processes.
    ///
    /// Bug context: The orphan cleanup pattern matches "claude" in command lines,
    /// but tmux servers include "claude" in their args when spawning windows.
    /// Since tmux servers run as daemons (PPID=1), they were incorrectly matched
    /// as orphans and killed, destroying all windows.
    #[test]
    fn orphan_cleanup_excludes_tmux_processes() {
        use std::process::Command;

        let session_name = "test-orphan-cleanup";

        // Clean up any leftover session from previous failed runs
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session_name])
            .output();

        // Start a tmux session with "claude --settings" in the command line
        // This simulates how midtown starts sessions
        let create_result = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name,
                "echo",
                "claude",
                "--settings",
                "/fake/midtown/test-settings.json",
            ])
            .output();

        if create_result.is_err() {
            // tmux not available, skip test
            return;
        }

        // Verify session was created
        let session_exists = Command::new("tmux")
            .args(["has-session", "-t", session_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !session_exists {
            // Could not create session (tmux server issues), skip
            return;
        }

        // Run the orphan cleanup with the pattern that matches "claude --settings"
        // This is the same pattern used by the daemon
        let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";
        let killed = super::kill_orphaned_processes(pattern);

        // The tmux session should still exist - it must NOT have been killed
        let session_still_exists = Command::new("tmux")
            .args(["has-session", "-t", session_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        // Clean up
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session_name])
            .output();

        assert!(
            session_still_exists,
            "tmux session was killed by orphan cleanup! killed={} processes. \
             The orphan cleanup pattern must exclude tmux processes.",
            killed
        );
    }
}
