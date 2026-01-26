//! Tmux session management for coworker processes.
//!
//! Provides functions for creating, managing, and communicating with
//! tmux sessions that host coworker Claude Code processes.

use std::process::Command;

use crate::Error;

/// Prefix for all midtown tmux sessions.
pub const SESSION_PREFIX: &str = "midtown-";

/// Create a new tmux session for a coworker.
///
/// Creates a detached session named `midtown-<name>` with the given working directory.
pub fn create_session(name: &str, working_dir: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",              // Detached
            "-s", &session_name,
            "-c", working_dir, // Starting directory
        ])
        .status()
        .map_err(|e| Error::Io(e))?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to create tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// Kill a tmux session.
pub fn kill_session(name: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["kill-session", "-t", &session_name])
        .status()
        .map_err(|e| Error::Io(e))?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to kill tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// Send keys (input) to a tmux session.
///
/// This is used to "nudge" a coworker by sending keyboard input.
pub fn send_keys(name: &str, keys: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["send-keys", "-t", &session_name, keys, "Enter"])
        .status()
        .map_err(|e| Error::Io(e))?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send keys to tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// Send raw keys without appending Enter.
pub fn send_keys_raw(name: &str, keys: &str) -> crate::Result<()> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["send-keys", "-t", &session_name, keys])
        .status()
        .map_err(|e| Error::Io(e))?;

    if !status.success() {
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to send keys to tmux session: {}", session_name),
        });
    }

    Ok(())
}

/// List all midtown tmux sessions.
///
/// Returns a vector of session names (without the `midtown-` prefix).
pub fn list_sessions() -> crate::Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map_err(|e| Error::Io(e))?;

    // If tmux returns non-zero, it might mean no sessions exist
    if !output.status.success() {
        // Check if it's just "no sessions" error
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("no sessions") {
            return Ok(Vec::new());
        }
        // Some other error
        return Err(Error::Rpc {
            code: -32603,
            message: format!("Failed to list tmux sessions: {}", stderr),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            if line.starts_with(SESSION_PREFIX) {
                Some(line[SESSION_PREFIX.len()..].to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(sessions)
}

/// Check if a session exists.
pub fn session_exists(name: &str) -> crate::Result<bool> {
    let session_name = format!("{}{}", SESSION_PREFIX, name);

    let status = Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .status()
        .map_err(|e| Error::Io(e))?;

    Ok(status.success())
}

/// JSON settings for coworker Claude Code sessions.
///
/// Configures the Stop hook to:
/// 1. Read channel messages (sync pending updates)
/// 2. Check for unclaimed tasks
/// 3. Block stopping if unclaimed tasks exist (keeps coworker working)
fn coworker_settings_json() -> String {
    r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"midtown --format json coworker stop-hook"}]}]}}"#.to_string()
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
- You're in an isolated worktree - commit freely
- Create PRs when work is ready for review
- Request review from teammates via channel

## Coordination
- The Lead coordinates overall direction
- Other coworkers are peers - collaborate via channel
- If blocked, post to channel and move to another task
"#,
        name = name
    )
}

/// Spawn Claude Code in a tmux session.
///
/// This creates the session and starts `claude` in it with coworker-specific
/// settings, including a Stop hook that reads the channel whenever the agent pauses.
/// Also injects a system prompt that gives the coworker instructions for operating
/// in the midtown environment.
pub fn spawn_claude(name: &str, working_dir: &str) -> crate::Result<()> {
    // First create the session
    create_session(name, working_dir)?;

    // Build the claude command with settings for channel synchronization
    // and a system prompt for coworker identity and instructions
    let settings = coworker_settings_json();
    let system_prompt = coworker_system_prompt(name);

    // Escape single quotes in the system prompt for shell safety
    let escaped_prompt = system_prompt.replace('\'', "'\\''");

    let command = format!(
        "claude --settings '{}' --append-system-prompt '{}'",
        settings, escaped_prompt
    );

    // Then send the command to start claude with coworker settings
    send_keys(name, &command)
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
        // Parse to verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&settings)
            .expect("coworker settings should be valid JSON");

        // Verify structure
        assert!(parsed["hooks"]["Stop"].is_array());
        let stop_hooks = &parsed["hooks"]["Stop"][0]["hooks"];
        assert!(stop_hooks.is_array());
        assert_eq!(stop_hooks[0]["type"], "command");
        assert_eq!(stop_hooks[0]["command"], "midtown --format json coworker stop-hook");
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

    // Integration tests would require actual tmux, so we keep unit tests minimal
}
