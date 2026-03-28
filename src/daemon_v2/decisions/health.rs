#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;

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

/// Ensure a lead is running for the given channel.
/// If no running lead exists, return a SpawnAgent command.
pub fn ensure_leads_alive(proj: &Projections, default_channel: &str) -> Vec<Command> {
    let has_running_lead = proj
        .agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .any(|agent| {
            agent.kind == AgentKind::Lead && agent.channel.as_deref() == Some(default_channel)
        });

    if has_running_lead {
        vec![]
    } else {
        // Use the channel's configured directory as the lead's working dir
        // so AGENTS.md/CLAUDE.md from that subdirectory gets loaded
        let working_dir = proj
            .channels
            .channel_directory(default_channel)
            .map(|d| d.to_string());

        vec![Command::SpawnAgent(SpawnConfig {
            name: default_channel.to_string(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".to_string(),
            provider: Provider::ClaudeCode,
            channel: Some(default_channel.to_string()),
            task_id: None,
            initial_prompt: None,
            working_dir,
            model: None,
        })]
    }
}
