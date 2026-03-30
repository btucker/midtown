#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "channel_spec_tests.rs"]
#[cfg(test)]
mod channel_spec_tests;

use chrono::Utc;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;

/// Find workers that are stopped but have in-progress tasks.
/// Per spec 2.2: resume the worker (not reset the task).
/// If the worker has no session_id, spawn a replacement with the same config.
pub fn check_dead_workers(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for (task_id, task) in &proj.work.tasks {
        if task.status != TaskStatus::InProgress {
            continue;
        }

        let agent_id = match proj.agents.by_task.get(task_id) {
            Some(id) => id,
            None => continue,
        };

        if !proj.agents.running.contains(agent_id)
            && let Some(agent) = proj.agents.by_id.get(agent_id)
            && !agent.gc
        {
            if agent.session_id.is_some() {
                // Has session_id — resume it
                commands.push(Command::ResumeAgent {
                    id: agent_id.clone(),
                });
            } else {
                // No session_id — spawn replacement with same config
                commands.push(Command::SpawnAgent(SpawnConfig {
                    name: agent.name.clone(),
                    kind: agent.kind.clone(),
                    agent_type: agent.agent_type.clone(),
                    provider: agent.provider.clone(),
                    channel: agent.channel.clone(),
                    task_id: agent.task_id.clone(),
                    initial_prompt: None,
                    working_dir: None,
                    model: None,
                    bound_thread_id: agent.bound_thread_id.clone(),
                    fork_from_session: None,
                    icon: agent.icon.clone(),
                    color: agent.color.clone(),
                }));
            }
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

/// Spec 2.2: nudge workers that have been running with an in-progress task
/// for more than 5 minutes without a state change.
/// Uses TaskNudge cooldown to avoid repeated nudging (1hr).
pub fn nudge_stale_workers(proj: &Projections) -> Vec<Command> {
    use crate::daemon_v2::projections::cooldowns::CooldownCategory;

    let cutoff = Utc::now() - chrono::Duration::minutes(5);
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .filter(|a| a.kind == AgentKind::Worker)
        .filter(|a| a.task_id.is_some())
        .filter(|a| a.started_at.is_some_and(|t| t < cutoff))
        .filter(|a| !proj.cooldowns.is_active(CooldownCategory::TaskNudge, &a.id))
        .map(|a| Command::NudgeAgent {
            id: a.id.clone(),
            message:
                "You've been running for more than 5 minutes. Please report your current status."
                    .into(),
        })
        .collect()
}

/// Spec 2.2: stop workers that reported "idle" more than 2 minutes ago.
pub fn stop_idle_reported_workers(proj: &Projections) -> Vec<Command> {
    let cutoff = Utc::now() - chrono::Duration::minutes(2);
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .filter(|a| a.kind == AgentKind::Worker)
        .filter(|a| {
            a.reported_state.as_deref() == Some("idle")
                && a.state_reported_at.is_some_and(|t| t < cutoff)
        })
        .map(|a| Command::StopAgent {
            id: a.id.clone(),
            reason: "idle for more than 2 minutes".into(),
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

    // Always ensure the default channel has a lead, even if it's not in the projection yet.
    // On fresh start, the projection has no channels — this guarantees the project lead spawns.
    if !has_running_lead(proj, default_channel) {
        let working_dir = proj
            .channels
            .channel_directory(default_channel)
            .map(|d| d.to_string());
        commands.push(Command::SpawnAgent(SpawnConfig {
            name: default_channel.to_string(),
            kind: AgentKind::Lead,
            agent_type: "midtown-project-lead".to_string(),
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
        }));
    }

    // Also check any channels that exist in the projection
    for (name, meta) in &proj.channels.channels {
        if meta.archived || name == default_channel {
            continue;
        }
        if has_running_lead(proj, name) {
            continue;
        }
        let working_dir = proj.channels.channel_directory(name).map(|d| d.to_string());
        commands.push(Command::SpawnAgent(SpawnConfig {
            name: name.clone(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".to_string(),
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

fn has_running_lead(proj: &Projections, channel: &str) -> bool {
    proj.agents.by_channel.get(channel).is_some_and(|ids| {
        ids.iter().any(|id| {
            proj.agents.running.contains(id)
                && proj
                    .agents
                    .by_id
                    .get(id)
                    .is_some_and(|a| a.kind == AgentKind::Lead)
        })
    })
}
