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
use crate::daemon_v2::projections::cooldowns::CooldownCategory;

pub(crate) const MAX_WORKER_RESTARTS: usize = 3;

/// Find workers that are stopped but have in-progress tasks.
/// Per spec 2.2: resume the worker (not reset the task).
/// If the worker has no session_id, spawn a replacement with the same config.
/// After MAX_WORKER_RESTARTS consecutive spawn failures, stop retrying and escalate to ops.
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
            // Spec 4.4: respect spawn failure cooldown for workers
            if proj
                .cooldowns
                .is_active(CooldownCategory::SpawnFailure, task_id)
            {
                continue;
            }

            // Spec 2.2: stop retrying after MAX_WORKER_RESTARTS consecutive failures
            let failures = proj
                .cooldowns
                .failure_count(CooldownCategory::SpawnFailure, task_id);
            if failures >= MAX_WORKER_RESTARTS {
                let escalation_key = format!("worker-escalation-{task_id}");
                if !proj
                    .cooldowns
                    .is_active(CooldownCategory::TaskNudge, &escalation_key)
                {
                    commands.push(Command::PostSystem {
                        channel: "ops".into(),
                        content: format!(
                            "Worker for task !{task_id} ({}) failed {failures} times. Manual intervention needed.",
                            task.subject
                        ),
                    });
                }
                continue;
            }

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
    let cutoff = Utc::now() - chrono::Duration::seconds(60);
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
            reason: "idle for 60s with no new message".into(),
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
/// If a stopped lead exists, resume it. Only spawn a new one if no lead exists at all.
pub fn ensure_channel_leads_alive(proj: &Projections, default_channel: &str) -> Vec<Command> {
    let mut commands = Vec::new();

    // Always ensure the default channel has a lead
    if let Some(cmd) = ensure_lead_for_channel(proj, default_channel, "midtown-project-lead") {
        commands.push(cmd);
    }

    // Also check any channels that exist in the projection
    for (name, meta) in &proj.channels.channels {
        if meta.archived || name == default_channel {
            continue;
        }
        if let Some(cmd) = ensure_lead_for_channel(proj, name, "midtown-channel-lead") {
            commands.push(cmd);
        }
    }

    commands
}

/// Ensure a single channel has a lead. Returns None if a lead is already running.
/// Resumes a stopped lead if one exists. Only spawns new if no lead exists at all.
/// Uses SpawnFailure cooldown to avoid tight respawn loops when the lead keeps dying.
fn ensure_lead_for_channel(proj: &Projections, channel: &str, agent_type: &str) -> Option<Command> {
    use crate::daemon_v2::projections::cooldowns::CooldownCategory;

    let lead = proj.agents.channel_lead(channel);

    match lead {
        // Running lead — nothing to do
        Some(agent) if proj.agents.running.contains(&agent.id) => None,
        // Stopped lead — resume it (with cooldown to prevent respawn storms)
        Some(agent) => {
            if proj
                .cooldowns
                .is_active(CooldownCategory::SpawnFailure, channel)
            {
                return None; // In cooldown from a recent failed spawn/resume
            }
            Some(Command::ResumeAgent {
                id: agent.id.clone(),
            })
        }
        // No lead at all — spawn one (with cooldown to prevent respawn storms)
        None => {
            if proj
                .cooldowns
                .is_active(CooldownCategory::SpawnFailure, channel)
            {
                return None;
            }
            let working_dir = proj
                .channels
                .channel_directory(channel)
                .map(|d| d.to_string());
            Some(Command::SpawnAgent(SpawnConfig {
                name: channel.to_string(),
                kind: AgentKind::Lead,
                agent_type: agent_type.to_string(),
                provider: Provider::ClaudeCode,
                channel: Some(channel.to_string()),
                task_id: None,
                initial_prompt: None,
                working_dir,
                model: None,
                bound_thread_id: None,
                fork_from_session: None,
                icon: None,
                color: None,
            }))
        }
    }
}
