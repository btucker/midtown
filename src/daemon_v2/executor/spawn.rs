#[path = "spawn_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::SpawnConfig;
use crate::daemon_v2::events::{AgentId, AgentKind, DomainEvent, Provider};
use crate::headless::HeadlessSession;
use crate::launch::{LaunchConfig, expand_session_id_in_prompt};
use crate::paths::ProjectPaths;

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

/// Spawn a headless agent session from a `SpawnConfig`.
///
/// Builds a `LaunchConfig`, converts it to a `HeadlessConfig`, spawns the
/// process, and returns the session handle along with the domain events
/// that record the creation.
///
/// For worker agents, an initial `AgentStateReported` event is emitted so the
/// sidebar shows activity immediately (e.g. "developing" or "reviewing")
/// without waiting for the agent to call `midtown state` itself.
pub async fn spawn_agent(
    spawn_config: &SpawnConfig,
    paths: &ProjectPaths,
) -> Result<(HeadlessSession, Vec<DomainEvent>), String> {
    let launch_config = build_launch_config(spawn_config, paths.dir_key());
    let mut headless_config = launch_config.to_headless_config(paths);

    // Pre-assign a session ID so the daemon knows it immediately at spawn time.
    let pre_assigned_session_id = uuid::Uuid::new_v4().to_string();
    headless_config.session_id = Some(pre_assigned_session_id.clone());

    // Expand $MIDTOWN_SESSION_ID in the system prompt so the AI sees the real UUID
    // and can include it verbatim in GitHub PR/comment frontmatter.
    // (Single-quoted heredocs prevent shell expansion, so we do it here in Rust.)
    headless_config.system_prompt =
        expand_session_id_in_prompt(&headless_config.system_prompt, &pre_assigned_session_id);

    if let Some(thread_id) = &spawn_config.bound_thread_id {
        headless_config
            .env
            .insert("MIDTOWN_BOUND_THREAD_ID".into(), thread_id.clone());
    }

    // Fork mode: inherit the parent session's context via --fork-session.
    if let Some(parent_session_id) = &spawn_config.fork_from_session {
        headless_config.resume_session_id = Some(parent_session_id.clone());
        headless_config.fork_session = true;
    }

    let mut session = HeadlessSession::spawn(&headless_config)
        .await
        .map_err(|e| format!("failed to spawn agent '{}': {}", spawn_config.name, e))?;

    // Deliver the initial prompt via stdin (send_message).
    // The -p flag tells Claude to wait for stdin input — we must actually send it.
    // Expand $MIDTOWN_SESSION_ID so task prompts also carry the real UUID.
    if let Some(ref prompt) = spawn_config.initial_prompt {
        let expanded = expand_session_id_in_prompt(prompt, &pre_assigned_session_id);
        if let Err(e) = session.send_message(&expanded).await {
            tracing::error!(name = %spawn_config.name, %e, "failed to send initial prompt");
        }
    }

    let pid = session.pid().unwrap_or(0);
    let agent_id: AgentId = uuid::Uuid::new_v4().to_string();
    let mut events =
        agent_spawned_events(&agent_id, spawn_config, pid, Some(pre_assigned_session_id));

    // Auto-report initial state for workers so the sidebar shows activity immediately.
    // Without this, there's a visible gap between spawn and the agent's first
    // `midtown state` call (which agents sometimes skip entirely). Reviewers
    // start as "reviewing"; all other workers start as "developing".
    if spawn_config.kind == AgentKind::Worker {
        let initial_state = if spawn_config.agent_type == "midtown-code-reviewer" {
            "reviewing"
        } else {
            "developing"
        };
        events.push(DomainEvent::AgentStateReported {
            id: agent_id,
            state: initial_state.to_string(),
        });
    }

    Ok((session, events))
}

/// Kill a running headless session.
pub async fn stop_agent(session: &mut HeadlessSession) -> Result<(), String> {
    session
        .kill()
        .await
        .map_err(|e| format!("failed to kill session: {}", e))
}

/// Build the events emitted when an agent is successfully spawned.
pub fn agent_spawned_events(
    id: &AgentId,
    config: &SpawnConfig,
    pid: u32,
    session_id: Option<String>,
) -> Vec<DomainEvent> {
    vec![
        DomainEvent::AgentCreated {
            id: id.clone(),
            name: config.name.clone(),
            kind: config.kind.clone(),
            agent_type: config.agent_type.clone(),
            provider: config.provider.clone(),
            channel: config.channel.clone(),
            task_id: config.task_id.clone(),
            bound_thread_id: config.bound_thread_id.clone(),
            icon: config.icon.clone(),
            color: config.color.clone(),
        },
        DomainEvent::AgentStarted {
            id: id.clone(),
            pid,
            session_id,
        },
    ]
}
