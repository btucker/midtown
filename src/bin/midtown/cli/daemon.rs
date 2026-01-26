//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
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

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();

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

## Delegation First
**IMPORTANT**: Your primary job is coordination, not implementation.

When the user requests work:
1. **Simple tasks** (typos, one-line fixes): Do them yourself
2. **Everything else**: Create a task and spawn a coworker

Benefits of delegation:
- Coworkers work in isolated worktrees (no conflicts)
- Multiple coworkers can work in parallel
- You stay available to answer questions and review

Example workflow:
```bash
# User asks for a feature
# 1. Create a task
TaskCreate with subject and description

# 2. Spawn a coworker
midtown coworker spawn

# 3. Nudge them with context
midtown coworker nudge <name> -m "Work on task #X: <brief description>"
```

## Commands
```bash
midtown status               # Check daemon and coworker status
midtown coworker spawn       # Spawn a new coworker
midtown coworker shutdown <name>  # Shutdown a coworker
midtown coworker nudge <name>     # Send message to coworker
midtown channel post "msg"   # Post to team channel
midtown channel read         # Read recent channel messages
```

## Spawning Coworkers
After spawning a coworker, immediately nudge them with context:
```bash
midtown coworker spawn
midtown coworker nudge <name> -m "Work on task #X: <brief description>"
```
Coworkers start with no context - they need a nudge to know what to do.

## Coordination
- Review work from coworkers
- Answer human questions about the project
- Create tasks and delegate to coworkers
- Monitor overall progress via `midtown status`
- Check channel for updates: `midtown channel read`

## Plans
- Always save plans to `~/.claude/plans/`
- Use descriptive filenames: `YYYY-MM-DD-<topic>.md`
- Plans persist across sessions and are shared with coworkers
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

/// Generate Lead settings JSON with stop hook for channel sync.
fn lead_settings_json() -> serde_json::Value {
    let bin_command = midtown::config::get_bin_command();
    serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("{} channel read", bin_command)
                }]
            }]
        }
    })
}

/// Write Lead settings to a file and return the path.
fn write_lead_settings_file() -> Result<PathBuf, String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory: {}", e))?;

    let path = dir.join("lead-settings.json");
    let settings = lead_settings_json();
    std::fs::write(&path, settings.to_string())
        .map_err(|e| format!("Failed to write lead settings file: {}", e))?;

    Ok(path)
}

/// Get the path to the Lead session ID file for a project.
fn lead_session_file(repo: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let repo_name = repo
        .file_name()
        .map(|s: &std::ffi::OsStr| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    PathBuf::from(home)
        .join(".midtown")
        .join(&repo_name)
        .join("lead-session-id")
}

/// Build the claude command for the Lead session.
///
/// Returns the full command string to launch Claude Code with appropriate flags.
/// Always includes the system prompt via --append-system-prompt, whether new or resuming.
/// Also includes settings file with stop hook for channel sync.
fn build_lead_claude_command(session_id: &str, is_existing: bool) -> Result<String, String> {
    let prompt_file = write_lead_prompt_file()?;
    let settings_file = write_lead_settings_file()?;

    if is_existing {
        // Resume existing session, but still inject system prompt and settings
        Ok(format!(
            "claude --dangerously-skip-permissions --resume {} --settings {} --append-system-prompt \"$(cat {})\"",
            session_id,
            settings_file.display(),
            prompt_file.display()
        ))
    } else {
        // New session: use specific session ID, settings, and inject system prompt
        Ok(format!(
            "claude --dangerously-skip-permissions --session-id {} --settings {} --append-system-prompt \"$(cat {})\"",
            session_id,
            settings_file.display(),
            prompt_file.display()
        ))
    }
}

/// Get or create the Lead session ID for a project.
///
/// If a session ID exists, returns it. Otherwise generates a new UUID and stores it.
fn get_or_create_lead_session_id(repo: &Path) -> Result<(String, bool), String> {
    let session_file = lead_session_file(repo);

    // Try to read existing session ID
    if session_file.exists() {
        let session_id = std::fs::read_to_string(&session_file)
            .map_err(|e| format!("Failed to read session ID: {}", e))?
            .trim()
            .to_string();
        if !session_id.is_empty() {
            return Ok((session_id, true)); // true = existing session
        }
    }

    // Generate new session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Ensure directory exists
    if let Some(parent) = session_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create session directory: {}", e))?;
    }

    // Store the session ID
    std::fs::write(&session_file, &session_id)
        .map_err(|e| format!("Failed to write session ID: {}", e))?;

    Ok((session_id, false)) // false = new session
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
        // Get or create the Lead's persistent session ID
        let (lead_session_id, is_existing) = get_or_create_lead_session_id(&repo)?;

        // Build the claude command (always includes system prompt)
        let claude_cmd = build_lead_claude_command(&lead_session_id, is_existing)?;

        // Get project name for status bar (uppercase)
        let project_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "PROJECT".to_string());

        // Create tmux session with claude command directly
        // -n sets the window name to "Lead"
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-n",
                "Lead",
                "-c",
                &repo.to_string_lossy(),
                "sh",
                "-c",
                &claude_cmd,
            ])
            .status()
            .map_err(|e| format!("Failed to create session: {}", e))?;

        if !status.success() {
            return Err(format!("Failed to create session '{}'", session));
        }

        // Configure status bar to show project name
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "status-left",
                &format!(" {} ", project_name),
            ])
            .status();

        // Split Lead window with chat TUI on the right (30% width)
        let lead_target = format!("{}:Lead", session);
        let _ = Command::new("tmux")
            .args(["split-window", "-h", "-t", &lead_target, "-p", "30"])
            .status();

        // Start chat TUI in the new pane (pane .1)
        let chat_pane = format!("{}:Lead.1", session);
        let bin_command = midtown::config::get_bin_command();
        let chat_cmd = format!("{} chat", bin_command);
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &chat_pane, &chat_cmd, "Enter"])
            .status();

        // Keep focus on the main pane (Claude Code, pane .0)
        let main_pane = format!("{}:Lead.0", session);
        let _ = Command::new("tmux")
            .args(["select-pane", "-t", &main_pane])
            .status();

        if is_existing {
            messages.push(format!("Resumed Lead session in '{}'", session));
        } else {
            messages.push(format!("Started new Lead session in '{}'", session));
        }
    }

    // Build response message
    let attach_hint = "Attach with: midtown attach".to_string();
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
/// If the session doesn't exist, it is automatically created first.
pub fn handle_attach() -> Result<Response, String> {
    let session = session_name()?;

    // Auto-create session if it doesn't exist
    if !session_exists(&session) {
        // Start midtown (daemon + tmux session)
        handle_start(false)?;

        // Wait briefly for the session to be ready
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Execute tmux attach - this replaces the current process
    let err = Command::new("tmux").args(["attach", "-t", &session]).exec();

    // If we get here, exec failed
    Err(format!("Failed to attach to session: {}", err))
}

/// Get session status for status command enhancement.
#[allow(dead_code)]
pub fn get_session_status() -> (bool, bool) {
    let exists = session_name().map(|s| session_exists(&s)).unwrap_or(false);
    (daemon_is_running(), exists)
}

/// Handle `midtown lead register-session` command.
///
/// Detects the Lead's Claude Code session UUID and saves it so coworkers
/// can link their task directories to share tasks.
pub fn handle_register_session() -> Result<Response, String> {
    use std::fs;

    let repo = repo_root()?;
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    // Find the Lead's session UUID (most recently modified task directory)
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let tasks_dir = home.join(".claude").join("tasks");

    if !tasks_dir.exists() {
        return Err(
            "No Claude Code task directories found. Create a task first with TaskCreate."
                .to_string(),
        );
    }

    let lead_uuid = find_newest_dir(&tasks_dir)?;

    // Save to ~/.midtown/<repo>/lead-session
    let midtown_dir = home.join(".midtown").join(&repo_name);
    fs::create_dir_all(&midtown_dir)
        .map_err(|e| format!("Failed to create midtown directory: {}", e))?;

    let session_file = midtown_dir.join("lead-session");
    fs::write(&session_file, &lead_uuid)
        .map_err(|e| format!("Failed to write session file: {}", e))?;

    Ok(Response::Message {
        message: format!("Registered Lead session: {}", lead_uuid),
    })
}

/// Find the most recently modified directory in the given path.
fn find_newest_dir(dir: &std::path::Path) -> Result<String, String> {
    use std::fs;

    let entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("Cannot read directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && !e.path().is_symlink())
        .collect();

    if entries.is_empty() {
        return Err("No directories found".to_string());
    }

    entries
        .iter()
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        .ok_or_else(|| "Failed to determine newest directory".to_string())
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
    fn test_lead_system_prompt_contains_plans_section() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("## Plans"));
        assert!(prompt.contains("~/.claude/plans/"));
    }

    #[test]
    fn test_lead_system_prompt_instructs_nudge_after_spawn() {
        let prompt = lead_system_prompt();
        // Lead should have a section explaining the spawn + nudge workflow
        assert!(
            prompt.contains("## Spawning Coworkers"),
            "Lead prompt should have a 'Spawning Coworkers' section with nudge instructions"
        );
    }

    #[test]
    fn test_session_exists_nonexistent() {
        // Random session name that definitely doesn't exist
        assert!(!session_exists("midtown-nonexistent-test-session-12345"));
    }

    #[test]
    fn test_get_or_create_lead_session_id_creates_new() {
        let temp = TempDir::new().unwrap();
        // Use a unique name with UUID to avoid conflicts between parallel tests
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // First call should create a new session ID
        let (session_id, is_existing) = get_or_create_lead_session_id(&repo_path).unwrap();

        // Clean up the session file we created in ~/.midtown/
        let session_file = lead_session_file(&repo_path);
        let _ = std::fs::remove_file(&session_file);
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(!is_existing);
        assert!(!session_id.is_empty());

        // Verify it's a valid UUID format (36 chars with hyphens)
        assert_eq!(session_id.len(), 36);
        assert!(session_id.contains('-'));
    }

    #[test]
    fn test_get_or_create_lead_session_id_returns_existing() {
        let temp = TempDir::new().unwrap();
        // Use a unique name with UUID to avoid conflicts between parallel tests
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // First call creates
        let (session_id_1, is_existing_1) = get_or_create_lead_session_id(&repo_path).unwrap();
        assert!(!is_existing_1);

        // Second call should return the same ID
        let (session_id_2, is_existing_2) = get_or_create_lead_session_id(&repo_path).unwrap();

        // Clean up the session file we created in ~/.midtown/
        let session_file = lead_session_file(&repo_path);
        let _ = std::fs::remove_file(&session_file);
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(is_existing_2);
        assert_eq!(session_id_1, session_id_2);
    }

    #[test]
    fn test_lead_session_file_path() {
        let repo_path = PathBuf::from("/tmp/my-project");
        let session_file = lead_session_file(&repo_path);

        assert!(session_file.to_string_lossy().contains(".midtown"));
        assert!(session_file.to_string_lossy().contains("my-project"));
        assert!(session_file.to_string_lossy().ends_with("lead-session-id"));
    }

    #[test]
    fn test_build_lead_claude_command_resume_includes_system_prompt() {
        // Bug: When resuming, the system prompt was not being included
        // The command should ALWAYS include --append-system-prompt
        let session_id = "test-session-123";
        let is_existing = true; // Resuming an existing session

        let cmd = build_lead_claude_command(session_id, is_existing).unwrap();

        // Must include system prompt even when resuming
        assert!(
            cmd.contains("--append-system-prompt"),
            "Resume command must include --append-system-prompt, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_new_includes_system_prompt() {
        let session_id = "test-session-456";
        let is_existing = false; // New session

        let cmd = build_lead_claude_command(session_id, is_existing).unwrap();

        assert!(
            cmd.contains("--append-system-prompt"),
            "New session command must include --append-system-prompt, got: {}",
            cmd
        );
        assert!(
            cmd.contains("--session-id"),
            "New session command must include --session-id, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_lead_claude_command_resume_uses_resume_flag() {
        let session_id = "test-session-789";
        let is_existing = true;

        let cmd = build_lead_claude_command(session_id, is_existing).unwrap();

        assert!(
            cmd.contains("--resume"),
            "Resume command must include --resume flag, got: {}",
            cmd
        );
    }
}
