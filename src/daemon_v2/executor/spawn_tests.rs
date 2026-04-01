use super::*;
use crate::daemon_v2::decisions::SpawnConfig;
use crate::daemon_v2::events::{AgentKind, Provider};

fn worker_config() -> SpawnConfig {
    SpawnConfig {
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-42".into()),
        initial_prompt: Some("Fix the bug".into()),
        working_dir: None,
        model: Some("sonnet".into()),
        bound_thread_id: None,
        fork_from_session: None,
        icon: None,
        color: None,
    }
}

#[test]
fn build_launch_config_sets_name_and_agent_type() {
    let config = worker_config();
    let launch = build_launch_config(&config, "test-dir-key");

    assert_eq!(launch.name, "ghost-town");
    assert_eq!(launch.agent_type, "midtown-code-author");
}

#[test]
fn build_launch_config_sets_task_id_and_channel() {
    let config = worker_config();
    let launch = build_launch_config(&config, "test-dir-key");

    assert_eq!(launch.task_id.as_deref(), Some("task-42"));
    assert_eq!(launch.channel.as_deref(), Some("main"));
}

#[test]
fn build_launch_config_codex_provider_sets_auth() {
    let config = SpawnConfig {
        provider: Provider::Codex,
        ..worker_config()
    };
    let launch = build_launch_config(&config, "test-dir-key");

    assert_eq!(launch.auth_provider, crate::auth::AuthProvider::Codex);
}

#[test]
fn build_launch_config_claude_provider_sets_auth() {
    let config = worker_config(); // default is ClaudeCode
    let launch = build_launch_config(&config, "test-dir-key");

    assert_eq!(launch.auth_provider, crate::auth::AuthProvider::Claude);
}

#[test]
fn agent_spawned_events_include_codex_provider() {
    let config = SpawnConfig {
        provider: Provider::Codex,
        ..worker_config()
    };
    let id = "agent-1".to_string();
    let events = agent_spawned_events(&id, &config, 1234, Some("sess-1".into()));

    assert_eq!(events.len(), 2);
    match &events[0] {
        DomainEvent::AgentCreated { provider, .. } => {
            assert_eq!(*provider, Provider::Codex);
        }
        other => panic!("expected AgentCreated, got {:?}", other),
    }
}
