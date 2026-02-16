//! Tests for session attach command construction.
//!
//! These tests verify that the shell command for attaching to a session
//! includes the correct system prompt flags.

use super::*;

#[test]
fn test_lead_attach_includes_system_prompt() {
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
    let result = build_attach_shell_command(
        "/tmp/test-cwd",
        "park",
        midtown::auth::AuthProvider::Claude,
        "session-456",
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
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

    // Should have at least 5 args: claude, --continue, --dangerously-skip-permissions, --append-system-prompt, "$(cat ...)"
    assert!(
        claude_cmd.len() >= 5,
        "Should have at least 5 arguments (claude, --continue, --dangerously-skip-permissions, --append-system-prompt, prompt file), got: {:?}",
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
