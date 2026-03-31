#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "dispatch_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

use std::collections::{HashMap, HashSet};

use crate::daemon_v2::decisions::lifecycle::create_dm_channel_name;
use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::naming;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::agents::Agent;

const DEFAULT_AGENT_TYPE: &str = "midtown-code-author";

/// Dispatch pending unblocked tasks up to `max_in_progress` total running workers.
/// Returns `SpawnAgent` commands for each slot available.
pub fn dispatch_pending_tasks(proj: &Projections, max_in_progress: usize) -> Vec<Command> {
    let current_in_progress = proj.work.in_progress_tasks.len();
    let slots = max_in_progress.saturating_sub(current_in_progress);
    if slots == 0 {
        return vec![];
    }

    let mut existing_names: HashSet<String> = proj.agents.by_name.keys().cloned().collect();
    let mut commands = Vec::new();

    for task_id in proj.work.pending_unblocked().into_iter().take(slots) {
        let Some(task) = proj.work.tasks.get(task_id) else {
            continue;
        };
        if proj.channels.is_lead_driven(&task.channel) {
            continue;
        }
        let agent_type = task
            .agent_type
            .clone()
            .unwrap_or_else(|| DEFAULT_AGENT_TYPE.to_string());
        // Spec 2.1: use name/icon/color from the task if set, otherwise generate
        let name = task
            .agent_name
            .clone()
            .unwrap_or_else(|| naming::generate_name(&existing_names));
        existing_names.insert(name.clone());
        let icon = Some(task.icon.clone().unwrap_or_else(naming::random_icon));
        let color = Some(task.color.clone().unwrap_or_else(naming::random_color));
        // Workers auto-output to a DM channel, not the task channel.
        // Insights are cross-posted to the task channel by flush_auto_output.
        let dm_channel = create_dm_channel_name(&name);
        commands.push(Command::SpawnAgent(SpawnConfig {
            name,
            kind: AgentKind::Worker,
            agent_type,
            provider: Provider::ClaudeCode,
            channel: Some(dm_channel),
            task_id: Some(task.id.clone()),
            initial_prompt: Some(task.subject.clone()),
            working_dir: None,
            model: None,
            bound_thread_id: task.thread_id.clone(),
            fork_from_session: None,
            icon,
            color,
        }));
    }

    commands
}

/// Stop duplicate running agents that share the same grouping key.
/// Keeps the oldest (by `started_at`), stops the rest.
fn stop_duplicates(
    proj: &Projections,
    kind: AgentKind,
    key_fn: impl Fn(&Agent) -> Option<&str>,
    reason: &str,
) -> Vec<Command> {
    let mut groups: HashMap<&str, Vec<&Agent>> = HashMap::new();
    for id in &proj.agents.running {
        if let Some(agent) = proj.agents.by_id.get(id)
            && agent.kind == kind
            && let Some(key) = key_fn(agent)
        {
            groups.entry(key).or_default().push(agent);
        }
    }

    let mut commands = Vec::new();
    for agents in groups.values() {
        if agents.len() > 1 {
            let mut sorted = agents.clone();
            sorted.sort_by_key(|a| a.started_at);
            for agent in &sorted[1..] {
                commands.push(Command::StopAgent {
                    id: agent.id.clone(),
                    reason: reason.into(),
                });
            }
        }
    }
    commands
}

/// Detect when two agents are assigned to the same task and stop the newer one.
/// Per spec 2.2: keeps the oldest agent (by `started_at`), stops the rest.
pub fn check_duplicate_workers(proj: &Projections) -> Vec<Command> {
    stop_duplicates(
        proj,
        AgentKind::Worker,
        |a| a.task_id.as_deref(),
        "duplicate worker for task",
    )
}

/// Detect when multiple leads are running for the same channel and stop the newer ones.
/// Prevents race conditions between ensure_channel_leads_alive and demand-spawned leads.
pub fn check_duplicate_leads(proj: &Projections) -> Vec<Command> {
    stop_duplicates(
        proj,
        AgentKind::Lead,
        |a| a.channel.as_deref(),
        "duplicate lead for channel",
    )
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
