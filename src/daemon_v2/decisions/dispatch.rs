#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;

const DEFAULT_AGENT_TYPE: &str = "midtown-code-author";

/// Dispatch pending unblocked tasks up to `max_in_progress` total running workers.
/// Returns `SpawnAgent` commands for each slot available.
pub fn dispatch_pending_tasks(proj: &Projections, max_in_progress: usize) -> Vec<Command> {
    let current_in_progress = proj.work.in_progress_tasks.len();
    let slots = max_in_progress.saturating_sub(current_in_progress);
    if slots == 0 {
        return vec![];
    }

    proj.work
        .pending_unblocked()
        .into_iter()
        .take(slots)
        .filter_map(|task_id| proj.work.tasks.get(task_id))
        .filter_map(|task| {
            if proj.channels.is_lead_driven(&task.channel) {
                return None;
            }
            let agent_type = task
                .agent_type
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_TYPE.to_string());
            Some(Command::SpawnAgent(SpawnConfig {
                name: task.id.clone(),
                kind: AgentKind::Worker,
                agent_type,
                provider: Provider::ClaudeCode,
                channel: Some(task.channel.clone()),
                task_id: Some(task.id.clone()),
                initial_prompt: Some(task.subject.clone()),
                working_dir: None,
                model: None,
                bound_thread_id: None,
            }))
        })
        .collect()
}

/// Find running Worker agents whose task has completed.
/// Returns `StopAgent` commands for each such agent.
pub fn stop_completed_agents(proj: &Projections) -> Vec<Command> {
    proj.agents
        .running
        .iter()
        .filter_map(|agent_id| proj.agents.by_id.get(agent_id))
        .filter(|agent| agent.kind == AgentKind::Worker)
        .filter(|agent| {
            agent.task_id.as_ref().is_some_and(|task_id| {
                proj.work
                    .tasks
                    .get(task_id)
                    .is_some_and(|t| t.status == TaskStatus::Completed)
            })
        })
        .map(|agent| Command::StopAgent {
            id: agent.id.clone(),
            reason: "task completed".to_string(),
        })
        .collect()
}
