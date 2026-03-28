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
