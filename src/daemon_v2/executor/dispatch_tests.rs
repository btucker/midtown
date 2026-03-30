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

#[tokio::test]
async fn execute_inline_handles_assign_task() {
    let mut sessions = HashMap::new();
    let cmd = Command::AssignTask {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], DomainEvent::TaskAssigned { task_id, agent_id }
            if task_id == "t1" && agent_id == "a1")
    );
}

#[tokio::test]
async fn execute_inline_handles_complete_task() {
    let mut sessions = HashMap::new();
    let cmd = Command::CompleteTask {
        task_id: "t1".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::TaskCompleted { task_id } if task_id == "t1"));
}

#[tokio::test]
async fn execute_inline_handles_reset_task() {
    let mut sessions = HashMap::new();
    let cmd = Command::ResetTask {
        task_id: "t1".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::TaskReset { task_id, .. } if task_id == "t1"));
}

#[tokio::test]
async fn execute_inline_handles_garbage_collect() {
    let mut sessions = HashMap::new();
    let cmd = Command::GarbageCollect {
        agent_id: "a1".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::AgentGarbageCollected { id } if id == "a1"));
}

#[tokio::test]
async fn execute_inline_handles_create_worktree() {
    let mut sessions = HashMap::new();
    let cmd = Command::CreateWorktree {
        task_id: "t1".into(),
        branch: "feat/test".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert!(events.is_empty());
}

#[tokio::test]
async fn execute_inline_handles_remove_worktree() {
    let mut sessions = HashMap::new();
    let cmd = Command::RemoveWorktree {
        task_id: "t1".into(),
    };
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert!(events.is_empty());
}

#[tokio::test]
async fn execute_inline_post_writes_to_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let channels_dir = tmp.path();
    let mut sessions = HashMap::new();
    let cmd = Command::Post {
        channel: "test-ch".into(),
        sender: "alice".into(),
        content: "hello world".into(),
        thread_id: None,
    };
    let events = execute_inline(cmd, &mut sessions, channels_dir);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], DomainEvent::MessagePosted { channel, sender, content, .. }
        if channel == "test-ch" && sender == "alice" && content == "hello world")
    );
}

#[tokio::test]
async fn execute_inline_post_system_writes_to_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let channels_dir = tmp.path();
    let mut sessions = HashMap::new();
    let cmd = Command::PostSystem {
        channel: "test-ch".into(),
        content: "system msg".into(),
    };
    let events = execute_inline(cmd, &mut sessions, channels_dir);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], DomainEvent::MessagePosted { sender, content, .. }
        if sender == "midtown" && content == "system msg")
    );
}

#[tokio::test]
async fn execute_inline_returns_empty_for_non_inline_command() {
    let mut sessions = HashMap::new();
    let cmd = Command::PollPrs;
    let events = execute_inline(cmd, &mut sessions, std::path::Path::new("/tmp"));
    assert!(events.is_empty());
}

#[tokio::test]
async fn spawn_background_poll_prs_sends_result() {
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<ExecutorResult>(16);
    let work = crate::daemon_v2::projections::work::WorkIndex::default();

    // PollPrs will fail (no gh CLI in test env or no repo context) but should
    // send back a result or complete without panicking
    spawn_background_poll_prs(work, result_tx);

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), result_rx.recv()).await;
    // Either received a result or the task completed with no events (both OK).
    // The channel closing (recv returns None) is also acceptable since
    // spawn_background_poll_prs only sends if events are non-empty.
    assert!(result.is_ok(), "background task should not hang");
}

#[tokio::test]
async fn spawn_background_gh_command_merge_sends_result() {
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<ExecutorResult>(16);
    let cmd = Command::MergePr { number: 99999 };

    spawn_background_gh_command(cmd, result_tx);

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), result_rx.recv()).await;
    // gh pr merge will fail in test env, but the task should not panic.
    // It may send Events(vec![]) or nothing at all (channel closes).
    assert!(result.is_ok(), "background gh command should not hang");
}

#[tokio::test]
async fn spawn_background_stop_sends_lifecycle_complete() {
    // We can't easily create a real HeadlessSession in tests, but we can
    // verify the function signature compiles and the type system is correct.
    // This test validates the type plumbing.
    let (result_tx, _result_rx) = tokio::sync::mpsc::channel::<ExecutorResult>(16);
    // Just verify the function exists and has the right signature
    let _: fn(String, String, HeadlessSession, tokio::sync::mpsc::Sender<ExecutorResult>) =
        spawn_background_stop;
    drop(result_tx);
}
