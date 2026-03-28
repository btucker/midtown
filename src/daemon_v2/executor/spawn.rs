#[path = "spawn_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::SpawnConfig;
use crate::daemon_v2::events::{AgentId, DomainEvent, Provider};
use crate::launch::LaunchConfig;

/// Convert a daemon_v2 `Provider` to the auth `AuthProvider`.
fn to_auth_provider(provider: &Provider) -> crate::auth::AuthProvider {
    match provider {
        Provider::ClaudeCode => crate::auth::AuthProvider::Claude,
        Provider::Codex => crate::auth::AuthProvider::Codex,
    }
}

/// Build a `LaunchConfig` from a `SpawnConfig`.
///
/// Translates the daemon_v2 spawn request into the launch configuration
/// used by the existing spawn infrastructure.
pub fn build_launch_config(config: &SpawnConfig, dir_key: &str) -> LaunchConfig {
    let launch = LaunchConfig::new(
        config.name.clone(),
        config.agent_type.clone(),
        dir_key,
        config.initial_prompt.clone(),
        None,
    )
    .with_task_id(config.task_id.clone())
    .with_channel(config.channel.clone())
    .with_auth_provider(to_auth_provider(&config.provider));

    let launch = if let Some(model) = &config.model {
        launch.with_model(model.clone())
    } else {
        launch
    };

    if let Some(working_dir) = &config.working_dir {
        launch.with_working_dir(Some(std::path::PathBuf::from(working_dir)))
    } else {
        launch
    }
}

/// Build the events emitted when an agent is successfully spawned.
pub fn agent_spawned_events(id: &AgentId, config: &SpawnConfig, pid: u32) -> Vec<DomainEvent> {
    vec![
        DomainEvent::AgentCreated {
            id: id.clone(),
            name: config.name.clone(),
            kind: config.kind.clone(),
            agent_type: config.agent_type.clone(),
            provider: config.provider.clone(),
            channel: config.channel.clone(),
            task_id: config.task_id.clone(),
        },
        DomainEvent::AgentStarted {
            id: id.clone(),
            pid,
        },
    ]
}
