//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::cli::Response;

/// Get the tmux session name based on the repo name.
/// Format: midtown-{repo_name}
fn session_name() -> Result<String, String> {
    let root = repo_root()?;
    let repo_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    Ok(format!("midtown-{}", repo_name))
}

/// Get the socket path for the daemon.
fn socket_path() -> PathBuf {
    let state_dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        });

    state_dir.join("midtown").join("daemon.sock")
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

    let path = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();

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

/// Generate the system prompt for the Lead.
fn lead_system_prompt() -> &'static str {
    r#"# Lead System Prompt

## Identity & Role
- You are the **Lead** of the midtown team
- You are the human-facing Claude Code instance
- You coordinate direction and can spawn coworkers

## Commands
```bash
midtown status               # Check daemon and coworker status
midtown coworker spawn       # Spawn a new coworker
midtown coworker shutdown <name>  # Shutdown a coworker
midtown coworker nudge <name>     # Send message to coworker
midtown channel post "msg"   # Post to team channel
```

## Coordination
- Review work from coworkers
- Answer human questions about the project
- Delegate tasks to coworkers when appropriate
- Monitor overall progress via `midtown status`
"#
}

/// Write the Lead system prompt to a file and return the path.
fn write_lead_prompt_file() -> Result<PathBuf, String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory: {}", e))?;

    let path = dir.join("lead-prompt.md");
    std::fs::write(&path, lead_system_prompt())
        .map_err(|e| format!("Failed to write lead prompt file: {}", e))?;

    Ok(path)
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates tmux session for the project
/// 3. Launches Claude Code with Lead config in that session
pub fn handle_start(daemon_only: bool) -> Result<Response, String> {
    // Verify we're in a git repo first
    let repo = repo_root()?;
    let session = session_name()?;

    let mut messages = Vec::new();

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
    } else {
        // Start the daemon in the background using `midtown daemon`
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable: {}", e))?;

        let mut cmd = Command::new(&exe);
        cmd.arg("daemon");
        cmd.current_dir(&repo);
        cmd.arg("--workdir").arg(&repo);

        // Spawn detached
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        cmd.spawn()
            .map_err(|e| format!("Failed to start daemon: {}", e))?;

        // Wait briefly for daemon to start
        std::thread::sleep(std::time::Duration::from_millis(500));

        if daemon_is_running() {
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

        // Write the system prompt to a file
        let prompt_file = write_lead_prompt_file()?;

        // Build the claude command
        let claude_cmd = format!(
            "claude --append-system-prompt \"$(cat {})\"",
            prompt_file.display()
        );

        // Create tmux session with claude command directly
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s", &session,
                "-c", &repo.to_string_lossy(),
                "sh", "-c", &claude_cmd,
            ])
            .status()
            .map_err(|e| format!("Failed to create session: {}", e))?;

        if !status.success() {
            return Err(format!("Failed to create session '{}'", session));
        }

        messages.push(format!("Started session '{}'", session));
    }

    // Build response message
    let attach_hint = format!("Attach with: midtown attach");
    messages.push(attach_hint);

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Handle `midtown stop` command.
///
/// Stops the daemon and optionally the tmux session.
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

    // Step 2: Stop daemon
    if daemon_is_running() {
        // Remove the socket file - daemon will detect this and exit
        let path = socket_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        messages.push("Stopped daemon".to_string());
    } else {
        messages.push("Daemon was not running".to_string());
    }

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Handle `midtown restart` command.
///
/// Stops and then starts midtown.
pub fn handle_restart() -> Result<Response, String> {
    // Stop everything (ignore errors if not running)
    let _ = handle_stop(false);

    // Brief pause for cleanup
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Start fresh
    handle_start(false)
}

/// Handle `midtown attach` command.
///
/// Attaches to the project's tmux session.
pub fn handle_attach() -> Result<Response, String> {
    let session = session_name()?;

    if !session_exists(&session) {
        return Err(format!(
            "Session '{}' not found. Run 'midtown' first.",
            session
        ));
    }

    // Execute tmux attach - this replaces the current process
    let err = Command::new("tmux")
        .args(["attach", "-t", &session])
        .exec();

    // If we get here, exec failed
    Err(format!("Failed to attach to session: {}", err))
}

/// Get session status for status command enhancement.
#[allow(dead_code)]
pub fn get_session_status() -> (bool, bool) {
    let exists = session_name()
        .map(|s| session_exists(&s))
        .unwrap_or(false);
    (daemon_is_running(), exists)
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

    #[test]
    fn test_lead_system_prompt_contains_commands() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("midtown status"));
        assert!(prompt.contains("midtown coworker spawn"));
        assert!(prompt.contains("Lead"));
    }

    #[test]
    fn test_session_exists_nonexistent() {
        // Random session name that definitely doesn't exist
        assert!(!session_exists("midtown-nonexistent-test-session-12345"));
    }
}
