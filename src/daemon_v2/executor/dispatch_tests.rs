use super::*;
use crate::daemon_v2::decisions::Command;

#[test]
fn assign_task_is_inline() {
    let cmd = Command::AssignTask {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    assert!(matches!(classify_command(&cmd), CommandClass::Inline));
}

#[test]
fn poll_prs_is_background() {
    let cmd = Command::PollPrs;
    assert!(matches!(classify_command(&cmd), CommandClass::Background));
}

#[test]
fn spawn_agent_is_background() {
    let cmd = Command::SpawnAgent(crate::daemon_v2::decisions::SpawnConfig {
        name: "test".into(),
        kind: crate::daemon_v2::events::AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: crate::daemon_v2::events::Provider::ClaudeCode,
        channel: None,
        task_id: None,
        initial_prompt: None,
        working_dir: None,
        model: None,
        bound_thread_id: None,
        fork_from_session: None,
        icon: None,
        color: None,
    });
    assert!(matches!(classify_command(&cmd), CommandClass::Background));
}

#[test]
fn nudge_agent_needs_resolution() {
    let cmd = Command::NudgeAgent {
        id: "a1".into(),
        message: "hello".into(),
    };
    assert!(matches!(
        classify_command(&cmd),
        CommandClass::NeedsResolution
    ));
}

#[test]
fn stop_agent_is_background() {
    let cmd = Command::StopAgent {
        id: "a1".into(),
        reason: "test".into(),
    };
    assert!(matches!(classify_command(&cmd), CommandClass::Background));
}

#[test]
fn complete_task_is_inline() {
    let cmd = Command::CompleteTask {
        task_id: "t1".into(),
    };
    assert!(matches!(classify_command(&cmd), CommandClass::Inline));
}

#[test]
fn poll_process_health_is_inline() {
    let cmd = Command::PollProcessHealth;
    assert!(matches!(classify_command(&cmd), CommandClass::Inline));
}

#[test]
fn merge_pr_is_background() {
    let cmd = Command::MergePr { number: 42 };
    assert!(matches!(classify_command(&cmd), CommandClass::Background));
}
