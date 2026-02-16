//! Tests for session attach command construction.
//!
//! These tests verify that the shell command for attaching to a session
//! includes the correct system prompt flags.

use super::*;
use std::sync::Mutex;

// Mutex to serialize tests that depend on CWD being in a git repo.
// Other tests (like daemon tests) may change CWD, so we need to serialize
// to prevent interference. We use unwrap_or_else to recover from poisoned mutex.
static SESSION_CWD_MUTEX: Mutex<()> = Mutex::new(());

/// Ensure we're in a git repository before running a test.
/// If not, try to cd to the project root (which is a git repo).
fn ensure_in_git_repo() {
    if midtown::paths::detect_repo_name().is_none() {
        // Not in a repo - try to find the project root by looking for Cargo.toml
        let current = std::env::current_dir().ok();
        if let Some(mut path) = current {
            // Walk up until we find Cargo.toml or run out of parents
            while !path.join("Cargo.toml").exists() {
                if !path.pop() {
                    // Reached root without finding Cargo.toml - skip this approach
                    break;
                }
            }
            // If we found Cargo.toml, cd there
            if path.join("Cargo.toml").exists() {
                let _ = std::env::set_current_dir(&path);
            }
        }
    }
}

#[test]
fn test_lead_attach_includes_system_prompt() {
    let _lock = SESSION_CWD_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_in_git_repo();
    let result = build_attach_shell_command(
        "/tmp/test-cwd",
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
    let command = result.unwrap();

    // Should include --append-system-prompt flag
    assert!(
        command.contains("--append-system-prompt"),
        "Lead attach command should include --append-system-prompt flag, got: {}",
        command
    );

    // Should reference a temp file with $(cat ...)
    assert!(
        command.contains("$(cat") && command.contains("midtown-attach-"),
        "Should use temp file pattern $(cat .../midtown-attach-...), got: {}",
        command
    );
}

#[test]
fn test_coworker_attach_includes_system_prompt() {
    let _lock = SESSION_CWD_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_in_git_repo();
    let result = build_attach_shell_command(
        "/tmp/test-cwd",
        "park",
        midtown::auth::AuthProvider::Claude,
        "session-456",
    );

    assert!(
        result.is_ok(),
        "build_attach_shell_command should succeed, got error: {:?}",
        result.as_ref().err()
    );
    let command = result.unwrap();

    // Should include --append-system-prompt flag
    assert!(
        command.contains("--append-system-prompt"),
        "Coworker attach command should include --append-system-prompt flag, got: {}",
        command
    );

    // Should reference a temp file with $(cat ...)
    assert!(
        command.contains("$(cat") && command.contains("midtown-attach-"),
        "Should use temp file pattern $(cat .../midtown-attach-...), got: {}",
        command
    );
}

#[test]
fn test_provider_resume_command_structure() {
    let claude_cmd =
        provider_resume_command(midtown::auth::AuthProvider::Claude, "test-session", "lead")
            .expect("provider_resume_command should succeed");

    // Should have at least 7 args: claude, --continue, --dangerously-skip-permissions, --settings, <settings-file>, --append-system-prompt, "$(cat ...)"
    assert!(
        claude_cmd.len() >= 7,
        "Should have at least 7 arguments (claude, --continue, --dangerously-skip-permissions, --settings, settings-file, --append-system-prompt, prompt file), got: {:?}",
        claude_cmd
    );

    // First arg should be 'claude'
    assert_eq!(claude_cmd[0], "claude");

    // Should include --continue
    assert!(
        claude_cmd.contains(&"--continue".to_string()),
        "Should include --continue flag"
    );

    // Should include --dangerously-skip-permissions
    assert!(
        claude_cmd.contains(&"--dangerously-skip-permissions".to_string()),
        "Should include --dangerously-skip-permissions flag"
    );

    // Should include --settings
    assert!(
        claude_cmd.contains(&"--settings".to_string()),
        "Should include --settings flag"
    );

    // Should include --append-system-prompt
    assert!(
        claude_cmd.contains(&"--append-system-prompt".to_string()),
        "Should include --append-system-prompt flag"
    );

    // Last arg should be the temp file reference
    let last_arg = &claude_cmd[claude_cmd.len() - 1];
    assert!(
        last_arg.contains("$(cat") && last_arg.contains("midtown-attach-"),
        "Last argument should use $(cat .../midtown-attach-...) pattern, got: {}",
        last_arg
    );
}

#[test]
fn test_lead_attach_sets_task_list_id() {
    let _lock = SESSION_CWD_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_in_git_repo();
    let result = build_attach_shell_command(
        "/tmp/test-cwd",
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
    let command = result.unwrap();

    // Lead should have CLAUDE_CODE_TASK_LIST_ID set
    assert!(
        command.contains("CLAUDE_CODE_TASK_LIST_ID="),
        "Lead attach command should set CLAUDE_CODE_TASK_LIST_ID env var, got: {}",
        command
    );
}

#[test]
fn test_coworker_attach_no_task_list_id() {
    let _lock = SESSION_CWD_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    ensure_in_git_repo();
    let result = build_attach_shell_command(
        "/tmp/test-cwd",
        "park",
        midtown::auth::AuthProvider::Claude,
        "session-456",
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
    let command = result.unwrap();

    // Coworkers should NOT have CLAUDE_CODE_TASK_LIST_ID set
    assert!(
        !command.contains("CLAUDE_CODE_TASK_LIST_ID="),
        "Coworker attach command should not set CLAUDE_CODE_TASK_LIST_ID env var, got: {}",
        command
    );
}
