//! tmux integration for sending nudges to coworker sessions

use std::process::Command;
use thiserror::Error;

/// Errors that can occur when sending nudges via tmux
#[derive(Error, Debug)]
pub enum NudgeError {
    /// tmux command execution failed
    #[error("tmux command failed: {0}")]
    TmuxError(String),

    /// tmux is not available
    #[error("tmux not found in PATH")]
    TmuxNotFound,

    /// The target session does not exist
    #[error("tmux session not found: {0}")]
    SessionNotFound(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Send a nudge message to a coworker's tmux session
///
/// Uses `tmux send-keys` to inject the message into the target session.
/// The message is sent as literal text followed by Enter to execute.
pub fn send_nudge(session: &str, message: &str) -> Result<(), NudgeError> {
    // First check if the session exists
    if !session_exists(session)? {
        return Err(NudgeError::SessionNotFound(session.to_string()));
    }

    // Send the message as a comment (prefixed with #) so it doesn't execute
    // anything harmful, followed by a newline
    let comment_message = format!("# {}", message);

    let output = Command::new("tmux")
        .args(["send-keys", "-t", session, &comment_message, "Enter"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }

    Ok(())
}

/// Check if a tmux session exists
pub fn session_exists(session: &str) -> Result<bool, NudgeError> {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()?;

    // has-session returns 0 if session exists, non-zero otherwise
    Ok(output.status.success())
}

/// List all tmux sessions
pub fn list_sessions() -> Result<Vec<String>, NudgeError> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()?;

    if !output.status.success() {
        // If tmux server isn't running, there are no sessions
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions: Vec<String> = stdout
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(sessions)
}

/// Send a nudge to a specific pane in a session
pub fn send_nudge_to_pane(
    session: &str,
    pane: &str,
    message: &str,
) -> Result<(), NudgeError> {
    let target = format!("{}:{}", session, pane);

    // First check if the session exists
    if !session_exists(session)? {
        return Err(NudgeError::SessionNotFound(session.to_string()));
    }

    let comment_message = format!("# {}", message);

    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, &comment_message, "Enter"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }

    Ok(())
}

/// Get the current pane content (for debugging/testing)
#[cfg(test)]
#[allow(dead_code)]
pub fn capture_pane(session: &str) -> Result<String, NudgeError> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require tmux to be running and may be skipped
    // in CI environments. They're marked as ignored by default.

    #[test]
    fn test_list_sessions_no_panic() {
        // Should not panic even if tmux isn't running
        let result = list_sessions();
        // Just verify it returns a result (may be empty or populated)
        assert!(result.is_ok() || matches!(result, Err(NudgeError::Io(_))));
    }

    #[test]
    fn test_session_exists_nonexistent() {
        // A randomly named session should not exist
        let result = session_exists("__nonexistent_test_session_xyz123__");
        match result {
            Ok(exists) => assert!(!exists),
            Err(NudgeError::Io(_)) => (), // tmux not available
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_send_nudge_to_nonexistent_session() {
        let result = send_nudge("__nonexistent_test_session_xyz123__", "test message");
        match result {
            Err(NudgeError::SessionNotFound(_)) => (),
            Err(NudgeError::Io(_)) => (), // tmux not available
            Ok(()) => panic!("Expected error for nonexistent session"),
            Err(e) => panic!("Unexpected error type: {}", e),
        }
    }
}
