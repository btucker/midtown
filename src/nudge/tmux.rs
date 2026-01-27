//! tmux integration for sending nudges to coworker windows

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use thiserror::Error;

use once_cell::sync::Lazy;

/// Mutex map to serialize nudges per target (session:window or session:window.pane)
/// This prevents concurrent nudges from interleaving and corrupting each other.
static TARGET_LOCKS: Lazy<Mutex<HashMap<String, std::sync::Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Get or create a mutex for the given target
fn get_target_lock(target: &str) -> std::sync::Arc<Mutex<()>> {
    let mut locks = TARGET_LOCKS.lock().unwrap();
    locks
        .entry(target.to_string())
        .or_insert_with(|| std::sync::Arc::new(Mutex::new(())))
        .clone()
}

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

    /// The target window does not exist
    #[error("tmux window not found: {0}")]
    WindowNotFound(String),

    /// Enter key failed after retries
    #[error("failed to send Enter key after retries")]
    EnterFailed,

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Send a nudge message to a coworker's tmux window
///
/// Uses a reliable pattern to inject the message:
/// 1. Send Escape to dismiss any dialogs or interruption prompts
/// 2. Wait 200ms for state to clear
/// 3. Send 'i' to enter INSERT mode
/// 4. Wait 100ms for mode transition
/// 5. Send message text with -l literal mode (prefixed with #)
/// 6. Wait 100ms for text to be received
/// 7. Send Enter with retry logic
///
/// Key insight: Escape must come BEFORE the text (to clear state),
/// not after (which would cancel the input).
/// Uses a per-target mutex to prevent concurrent nudges from interleaving.
///
/// # Arguments
/// * `session` - The tmux session name (e.g., "midtown-projectname")
/// * `window` - The window name (coworker name, e.g., "lexington")
/// * `message` - The nudge message to send
pub fn send_nudge(session: &str, window: &str, message: &str) -> Result<(), NudgeError> {
    let target = format!("{}:{}", session, window);

    // First check if the session exists
    if !session_exists(session)? {
        return Err(NudgeError::SessionNotFound(session.to_string()));
    }

    // Get mutex for this target to prevent interleaving
    let lock = get_target_lock(&target);
    let _guard = lock.lock().unwrap();

    // 1. Send Escape FIRST to dismiss any dialogs or interruption prompts
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &target, "Escape"])
        .output()?;
    thread::sleep(Duration::from_millis(200));

    // 2. Send 'i' to enter INSERT mode
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, "i"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }
    thread::sleep(Duration::from_millis(100));

    // 3. Send message in literal mode (prefixed with # to make it a comment)
    let comment = format!("# {}", message);
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", &comment])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }
    thread::sleep(Duration::from_millis(100));

    // 4. Send Enter with retry logic
    for attempt in 0..3 {
        let output = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(200));
        }
    }

    Err(NudgeError::EnterFailed)
}

/// Check if a tmux session exists
pub fn session_exists(session: &str) -> Result<bool, NudgeError> {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()?;

    // has-session returns 0 if session exists, non-zero otherwise
    Ok(output.status.success())
}

/// Check if a tmux window exists within a session
pub fn window_exists(session: &str, window: &str) -> Result<bool, NudgeError> {
    let target = format!("{}:{}", session, window);

    let output = Command::new("tmux")
        .args(["has-session", "-t", &target])
        .output()?;

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

/// List all windows in a session
pub fn list_windows(session: &str) -> Result<Vec<String>, NudgeError> {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()?;

    if !output.status.success() {
        // Session might not exist
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let windows: Vec<String> = stdout
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(windows)
}

/// Send a nudge to a specific pane in a window
///
/// Uses the same reliable multi-step pattern as `send_nudge`:
/// 1. Send 'i' to enter vim INSERT mode
/// 2. Wait 100ms for mode transition
/// 3. Send message text with -l literal mode
/// 4. Wait 500ms for paste to complete
/// 5. Send Escape to exit vim INSERT mode
/// 6. Wait 100ms for mode transition
/// 7. Send Enter with retry logic
///
/// Uses a per-target mutex to prevent concurrent nudges from interleaving.
pub fn send_nudge_to_pane(
    session: &str,
    window: &str,
    pane: &str,
    message: &str,
) -> Result<(), NudgeError> {
    let target = format!("{}:{}.{}", session, window, pane);

    // First check if the session exists
    if !session_exists(session)? {
        return Err(NudgeError::SessionNotFound(session.to_string()));
    }

    // Get mutex for this target to prevent interleaving
    let lock = get_target_lock(&target);
    let _guard = lock.lock().unwrap();

    // 1. Send Escape FIRST to dismiss any dialogs or interruption prompts
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &target, "Escape"])
        .output()?;
    thread::sleep(Duration::from_millis(200));

    // 2. Send 'i' to enter INSERT mode
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, "i"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }
    thread::sleep(Duration::from_millis(100));

    // 3. Send message in literal mode (prefixed with # to make it a comment)
    let comment = format!("# {}", message);
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", &comment])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NudgeError::TmuxError(stderr.into_owned()));
    }
    thread::sleep(Duration::from_millis(100));

    // 4. Send Enter with retry logic
    for attempt in 0..3 {
        let output = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(200));
        }
    }

    Err(NudgeError::EnterFailed)
}

/// Get the current pane content (for debugging/testing)
#[cfg(test)]
#[allow(dead_code)]
pub fn capture_pane(session: &str, window: &str) -> Result<String, NudgeError> {
    let target = format!("{}:{}", session, window);

    let output = Command::new("tmux")
        .args(["capture-pane", "-t", &target, "-p"])
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
        let result = send_nudge(
            "__nonexistent_test_session_xyz123__",
            "window",
            "test message",
        );
        match result {
            Err(NudgeError::SessionNotFound(_)) => (),
            Err(NudgeError::Io(_)) => (), // tmux not available
            Ok(()) => panic!("Expected error for nonexistent session"),
            Err(e) => panic!("Unexpected error type: {}", e),
        }
    }
}
