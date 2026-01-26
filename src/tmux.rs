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

/// Spawn Claude Code in a tmux session.
///
/// This creates the session and starts `claude` in it.
pub fn spawn_claude(name: &str, working_dir: &str) -> crate::Result<()> {
    // First create the session
    create_session(name, working_dir)?;

    // Then send the command to start claude
    send_keys(name, "claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_prefix() {
        assert_eq!(SESSION_PREFIX, "midtown-");
    }

    // Integration tests would require actual tmux, so we keep unit tests minimal
}
