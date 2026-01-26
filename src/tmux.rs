//! Tmux session management for coworker processes.
//!
//! Provides functions for creating, managing, and communicating with
//! tmux windows that host coworker Claude Code processes within the
//! project session.

use std::path::PathBuf;
use std::process::Command;

use crate::Error;

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
fn symlink_tasks_to_lead(coworker_session_id: &str, lead_session_id: &str) -> crate::Result<()> {
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
        .filter(|name| *name != "Lead") // Exclude the Lead window
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
/// Configures the Stop hook to:
/// 1. Read channel messages (sync pending updates)
/// 2. Check for unclaimed tasks
/// 3. Block stopping if unclaimed tasks exist (keeps coworker working)
fn coworker_settings_json() -> serde_json::Value {
    serde_json::json!({
        "editorMode": "normal",
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": "midtown --format json coworker stop-hook"
                }]
            }]
        }
    })
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

/// Generate the system prompt for a coworker.
///
/// This prompt gives the coworker instructions for operating in the midtown
/// environment, including channel usage, task workflow, and coordination.
fn coworker_system_prompt(name: &str) -> String {
    format!(
        r#"# Coworker System Prompt

## Identity & Role
- You are a coworker in a midtown team
- Your name is **{name}**
- You work in your own git worktree

## Channel Usage
Post updates to the team channel:
```bash
midtown channel post "your message here"
```

The channel is like Slack - keep teammates informed. Post when:
- Starting work on a task
- Hitting blockers
- Finishing tasks
- Needing review

## Task Workflow
```bash
midtown task list           # Check available tasks
midtown task claim <id>     # Claim a task
midtown task done <id>      # Mark task complete
```

Don't hoard tasks - claim one, finish it, then claim another.

## Git Workflow
- You're in an isolated worktree (detached HEAD at the Lead's current commit)
- First thing: create a feature branch for your task: `git checkout -b {name}/<task-description>`
- Commit frequently with clear messages
- When done, push and create a PR: `gh pr create`
- Request review from teammates via channel

## Coordination
- The Lead coordinates overall direction
- Other coworkers are peers - collaborate via channel
- If blocked, post to channel and move to another task
"#,
        name = name
    )
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
pub fn spawn_claude(
    session: &str,
    name: &str,
    working_dir: &str,
    repo_name: Option<&str>,
) -> crate::Result<()> {
    // Build the claude command with settings for channel synchronization
    // and a system prompt for coworker identity and instructions
    let system_prompt = coworker_system_prompt(name);

    // Write system prompt and settings to files (avoids quoting issues)
    let prompt_file = write_coworker_prompt_file(name, &system_prompt)?;
    let settings_file = write_coworker_settings_file()?;

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
    let command = format!(
        "claude --dangerously-skip-permissions --session-id {} --settings {} --append-system-prompt \"$(cat {})\"",
        coworker_session_id,
        settings_file.display(),
        prompt_file.display()
    );

    // Create window with claude command running directly
    create_window(session, name, working_dir, Some(&command))
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
        let settings = coworker_settings_json();

        // Verify editorMode is normal (not vim)
        assert_eq!(settings["editorMode"], "normal");

        // Verify hook structure
        assert!(settings["hooks"]["Stop"].is_array());
        let stop_hooks = &settings["hooks"]["Stop"][0]["hooks"];
        assert!(stop_hooks.is_array());
        assert_eq!(stop_hooks[0]["type"], "command");
        assert_eq!(
            stop_hooks[0]["command"],
            "midtown --format json coworker stop-hook"
        );
    }

    #[test]
    fn test_coworker_system_prompt_contains_name() {
        let prompt = coworker_system_prompt("lexington");

        // Verify name is interpolated
        assert!(prompt.contains("**lexington**"));
        assert!(prompt.contains("Your name is **lexington**"));
    }

    #[test]
    fn test_coworker_system_prompt_contains_required_sections() {
        let prompt = coworker_system_prompt("park");

        // Verify all required sections are present
        assert!(prompt.contains("## Identity & Role"));
        assert!(prompt.contains("## Channel Usage"));
        assert!(prompt.contains("## Task Workflow"));
        assert!(prompt.contains("## Git Workflow"));
        assert!(prompt.contains("## Coordination"));
    }

    #[test]
    fn test_coworker_system_prompt_contains_commands() {
        let prompt = coworker_system_prompt("madison");

        // Verify key commands are documented
        assert!(prompt.contains("midtown channel post"));
        assert!(prompt.contains("midtown task list"));
        assert!(prompt.contains("midtown task claim"));
        assert!(prompt.contains("midtown task done"));
    }

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

    // Integration tests would require actual tmux, so we keep unit tests minimal
}
