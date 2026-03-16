use super::{TaskCommand, derive_thread_id};
use clap::Parser;

/// Minimal CLI wrapper for parsing TaskCommand subcommands in tests.
#[derive(Parser)]
struct TestCli {
    #[command(subcommand)]
    command: TaskCommand,
}

fn parse_task_cmd(args: &[&str]) -> TaskCommand {
    let mut full = vec!["test"];
    full.extend_from_slice(args);
    TestCli::parse_from(full).command
}

#[test]
fn handoff_parses_required_args() {
    let cmd = parse_task_cmd(&["handoff", "--id", "42", "--agent", "midtown-code-reviewer"]);
    match cmd {
        TaskCommand::Handoff { id, agent, message } => {
            assert_eq!(id, "42");
            assert_eq!(agent, "midtown-code-reviewer");
            assert!(message.is_none());
        }
        other => panic!("Expected Handoff, got {:?}", other),
    }
}

#[test]
fn handoff_parses_with_message() {
    let cmd = parse_task_cmd(&[
        "handoff",
        "--id",
        "42",
        "--agent",
        "midtown-code-reviewer",
        "--message",
        "Please review the PR",
    ]);
    match cmd {
        TaskCommand::Handoff { id, agent, message } => {
            assert_eq!(id, "42");
            assert_eq!(agent, "midtown-code-reviewer");
            assert_eq!(message.as_deref(), Some("Please review the PR"));
        }
        other => panic!("Expected Handoff, got {:?}", other),
    }
}

#[test]
fn derive_thread_id_prefers_cli_value() {
    let result = derive_thread_id(Some("cli-thread"), Some("env-thread"));
    assert_eq!(result.as_deref(), Some("cli-thread"));
}

#[test]
fn derive_thread_id_uses_env_when_cli_missing() {
    let result = derive_thread_id(None, Some("env-thread"));
    assert_eq!(result.as_deref(), Some("env-thread"));
}

#[test]
fn derive_thread_id_falls_back_when_cli_empty() {
    let result = derive_thread_id(Some("  "), Some("env-thread"));
    assert_eq!(result.as_deref(), Some("env-thread"));
}

#[test]
fn derive_thread_id_returns_none_when_no_values() {
    let result = derive_thread_id(None, None);
    assert!(result.is_none());
}

#[test]
fn derive_thread_id_ignores_empty_env_value() {
    let result = derive_thread_id(None, Some("   "));
    assert!(result.is_none());
}

#[test]
fn derive_thread_id_preserves_original_cli_value() {
    let raw = " thread-123 ";
    let result = derive_thread_id(Some(raw), None);
    assert_eq!(result.as_deref(), Some(raw));
}
