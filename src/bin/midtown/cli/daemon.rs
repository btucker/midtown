//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::cli::Response;

/// Session name for the Lead Claude Code instance.
pub const LEAD_SESSION: &str = "midtown-lead";

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

/// Check if the Lead tmux session exists.
fn lead_session_exists() -> bool {
    let output = Command::new("tmux")
        .args(["has-session", "-t", LEAD_SESSION])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Get the repository root directory (working directory for Lead).
fn repo_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err("Not in a git repository".to_string());
    }

    let path = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();

    Ok(PathBuf::from(path))
}

/// JSON settings for the Lead Claude Code session.
fn lead_settings_json() -> String {
    // Lead doesn't need special hooks for now
    r#"{}"#.to_string()
}

/// Generate the system prompt for the Lead.
fn lead_system_prompt() -> String {
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

## Designing Evaluation Systems

When planning work with the human, collaborate on how coworkers will verify their work:

1. **Ask the human**: "How should coworkers know their work is correct?"
2. **Design evaluation criteria together**:
   - Test suites (unit, integration, e2e)
   - Visual parity checks (screenshots, diffs)
   - Linting and formatting checks
   - Type checking
   - Benchmark comparisons
   - Manual checklists for subjective work
3. **Create eval commands** coworkers can run (e.g., `cargo test`, `npm run lint`, custom scripts)
4. **Document acceptance criteria** in task descriptions

### Before Spawning Coworkers

- Ensure an eval system exists or create one with the human
- Include verification instructions in every task description
- Example: "Verification: Run `make test-auth` - all tests should pass"

### What "Done" Looks Like

Help the human define success criteria:
- What does correct behavior look like?
- How do we measure success objectively?
- What edge cases should be handled?

## Coordination
- Review work from coworkers
- Answer human questions about the project
- Delegate tasks to coworkers when appropriate
- Monitor overall progress via `midtown status`
"#.to_string()
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates tmux session 'midtown-lead'
/// 3. Launches Claude Code with Lead plugin/config in that session
pub fn handle_start(daemon_only: bool) -> Result<Response, String> {
    let mut messages = Vec::new();

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
    } else {
        // Start the daemon in the background
        let mut cmd = Command::new("midtownd");

        // Set working directory to repo root if available
        if let Ok(root) = repo_root() {
            cmd.current_dir(&root);
            cmd.arg("--workdir").arg(&root);
        }

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

    // Step 2: Launch Lead session (unless --daemon-only)
    if daemon_only {
        messages.push("Skipping Lead session (--daemon-only)".to_string());
    } else if lead_session_exists() {
        messages.push(format!("Lead session '{}' already exists", LEAD_SESSION));
    } else {
        // Get repo root for working directory
        let working_dir = repo_root().unwrap_or_else(|_| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        // Create tmux session
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d", // Detached
                "-s",
                LEAD_SESSION,
                "-c",
                &working_dir.to_string_lossy(),
            ])
            .status()
            .map_err(|e| format!("Failed to create tmux session: {}", e))?;

        if !status.success() {
            return Err(format!("Failed to create tmux session '{}'", LEAD_SESSION));
        }

        // Build the claude command with Lead settings
        let settings = lead_settings_json();
        let system_prompt = lead_system_prompt();
        let escaped_prompt = system_prompt.replace('\'', "'\\''");

        let command = format!(
            "claude --settings '{}' --append-system-prompt '{}'",
            settings, escaped_prompt
        );

        // Send the command to start claude
        let status = Command::new("tmux")
            .args(["send-keys", "-t", LEAD_SESSION, &command, "Enter"])
            .status()
            .map_err(|e| format!("Failed to start Claude Code in Lead session: {}", e))?;

        if !status.success() {
            return Err("Failed to start Claude Code in Lead session".to_string());
        }

        messages.push(format!("Started Lead session '{}'", LEAD_SESSION));
    }

    // Build response message
    let attach_hint = format!("Attach to Lead with: tmux attach -t {}", LEAD_SESSION);
    messages.push(attach_hint);

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Handle `midtown stop` command.
///
/// Stops the daemon and optionally the Lead session.
pub fn handle_stop(keep_lead: bool) -> Result<Response, String> {
    let mut messages = Vec::new();

    // Step 1: Stop Lead session (unless --keep-lead)
    if !keep_lead && lead_session_exists() {
        let status = Command::new("tmux")
            .args(["kill-session", "-t", LEAD_SESSION])
            .status()
            .map_err(|e| format!("Failed to kill Lead session: {}", e))?;

        if status.success() {
            messages.push(format!("Stopped Lead session '{}'", LEAD_SESSION));
        } else {
            messages.push(format!("Warning: Failed to stop Lead session '{}'", LEAD_SESSION));
        }
    } else if lead_session_exists() {
        messages.push(format!("Keeping Lead session '{}' (use without --keep-lead to stop)", LEAD_SESSION));
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

/// Handle `midtown attach` command.
///
/// Attaches to the Lead tmux session.
pub fn handle_attach() -> Result<Response, String> {
    if !lead_session_exists() {
        return Err(format!(
            "Lead session '{}' not found. Run 'midtown start' first.",
            LEAD_SESSION
        ));
    }

    // Execute tmux attach - this replaces the current process
    let err = Command::new("tmux")
        .args(["attach", "-t", LEAD_SESSION])
        .exec();

    // If we get here, exec failed
    Err(format!("Failed to attach to Lead session: {}", err))
}

/// Get Lead session status for status command enhancement.
#[allow(dead_code)]
pub fn get_lead_status() -> (bool, bool) {
    (daemon_is_running(), lead_session_exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lead_session_constant() {
        assert_eq!(LEAD_SESSION, "midtown-lead");
    }

    #[test]
    fn test_lead_system_prompt_contains_required_sections() {
        let prompt = lead_system_prompt();

        // Verify all required sections are present
        assert!(prompt.contains("## Identity & Role"));
        assert!(prompt.contains("## Commands"));
        assert!(prompt.contains("## Designing Evaluation Systems"));
        assert!(prompt.contains("## Coordination"));
    }

    #[test]
    fn test_lead_system_prompt_contains_evaluation_guidance() {
        let prompt = lead_system_prompt();

        // Verify evaluation system design guidance
        assert!(prompt.contains("How should coworkers know their work is correct"));
        assert!(prompt.contains("Design evaluation criteria"));
        assert!(prompt.contains("Create eval commands"));
        assert!(prompt.contains("Document acceptance criteria"));
    }

    #[test]
    fn test_lead_system_prompt_contains_eval_examples() {
        let prompt = lead_system_prompt();

        // Verify concrete eval examples are provided
        assert!(prompt.contains("Test suites"));
        assert!(prompt.contains("Visual parity"));
        assert!(prompt.contains("Linting"));
        assert!(prompt.contains("Type checking"));
        assert!(prompt.contains("Benchmark"));
    }

    #[test]
    fn test_lead_system_prompt_contains_pre_spawn_checklist() {
        let prompt = lead_system_prompt();

        // Verify pre-spawn checklist
        assert!(prompt.contains("Before Spawning Coworkers"));
        assert!(prompt.contains("Ensure an eval system exists"));
        assert!(prompt.contains("Include verification instructions"));
    }

    #[test]
    fn test_lead_system_prompt_contains_commands() {
        let prompt = lead_system_prompt();

        // Verify key commands are documented
        assert!(prompt.contains("midtown status"));
        assert!(prompt.contains("midtown coworker spawn"));
        assert!(prompt.contains("midtown coworker shutdown"));
        assert!(prompt.contains("midtown coworker nudge"));
        assert!(prompt.contains("midtown channel post"));
    }
}
