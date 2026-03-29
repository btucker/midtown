#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;

use chrono::Utc;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;

/// Find workers that are stopped but have in-progress tasks.
/// Returns ResetTask commands so they can be re-dispatched.
pub fn check_dead_workers(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for (task_id, task) in &proj.work.tasks {
        if task.status != TaskStatus::InProgress {
            continue;
        }

        // Find the agent assigned to this task
        let agent_id = match proj.agents.by_task.get(task_id) {
            Some(id) => id,
            None => continue,
        };

        // Check if the agent is stopped (not in the running set)
        if !proj.agents.running.contains(agent_id) {
            commands.push(Command::ResetTask {
                task_id: task_id.clone(),
            });
        }
    }

    commands
}

/// Stop workers that are running but have no task assigned and have been
/// running for more than 5 minutes.
pub fn check_idle_workers(proj: &Projections) -> Vec<Command> {
    let cutoff = Utc::now() - chrono::Duration::minutes(5);
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .filter(|a| a.kind == AgentKind::Worker && a.task_id.is_none())
        .filter(|a| a.started_at.is_some_and(|t| t < cutoff))
        .map(|a| Command::StopAgent {
            id: a.id.clone(),
            reason: "idle worker".into(),
        })
        .collect()
}

/// Detect auth errors from session stderr (stub — requires stderr plumbing).
/// Auth errors will cause session death, which `check_dead_workers` handles.
pub fn check_auth_errors(_proj: &Projections) -> Vec<Command> {
    // TODO: detect auth errors from session stderr
    // For now, auth errors will cause session death, which check_dead_workers handles
    vec![]
}

/// Detect usage limits from session output (stub — requires stderr plumbing).
/// Usage-limited sessions will be stopped and tasks reset by `check_dead_workers`.
pub fn check_usage_limits(_proj: &Projections) -> Vec<Command> {
    // TODO: detect usage limits from session output
    // For now, usage-limited sessions will be stopped and tasks reset by check_dead_workers
    vec![]
}

/// Ensure a lead is running for every active (non-archived) channel.
/// Uses "midtown-project-lead" for the default channel and "midtown-channel-lead" for others.
pub fn ensure_channel_leads_alive(proj: &Projections, default_channel: &str) -> Vec<Command> {
    let mut commands = Vec::new();
    for (name, meta) in &proj.channels.channels {
        if meta.archived {
            continue;
        }
        if has_running_lead(proj, name) {
            continue;
        }
        let agent_type = if name == default_channel {
            "midtown-project-lead"
        } else {
            "midtown-channel-lead"
        };
        let working_dir = proj.channels.channel_directory(name).map(|d| d.to_string());
        commands.push(Command::SpawnAgent(SpawnConfig {
            name: name.clone(),
            kind: AgentKind::Lead,
            agent_type: agent_type.to_string(),
            provider: Provider::ClaudeCode,
            channel: Some(name.clone()),
            task_id: None,
            initial_prompt: None,
            working_dir,
            model: None,
            bound_thread_id: None,
            fork_from_session: None,
            icon: None,
            color: None,
        }));
    }
    commands
}

/// Ensure a lead is running for the given channel.
/// If no running lead exists, return a SpawnAgent command.
pub fn ensure_leads_alive(proj: &Projections, default_channel: &str) -> Vec<Command> {
    if has_running_lead(proj, default_channel) {
        vec![]
    } else {
        // Use the channel's configured directory as the lead's working dir
        // so AGENTS.md/CLAUDE.md from that subdirectory gets loaded
        let working_dir = proj
            .channels
            .channel_directory(default_channel)
            .map(|d| d.to_string());

        // Use project-lead for the main channel, channel-lead for topic channels
        let agent_type = "midtown-project-lead".to_string();

        vec![Command::SpawnAgent(SpawnConfig {
            name: default_channel.to_string(),
            kind: AgentKind::Lead,
            agent_type,
            provider: Provider::ClaudeCode,
            channel: Some(default_channel.to_string()),
            task_id: None,
            initial_prompt: None,
            working_dir,
            model: None,
            bound_thread_id: None,
            fork_from_session: None,
            icon: None,
            color: None,
        })]
    }
}

fn has_running_lead(proj: &Projections, channel: &str) -> bool {
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .any(|agent| agent.kind == AgentKind::Lead && agent.channel.as_deref() == Some(channel))
}
