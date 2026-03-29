use chrono::{Duration, Utc};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::AgentKind;
use crate::daemon_v2::projections::Projections;

#[path = "lifecycle_tests.rs"]
#[cfg(test)]
mod tests;

/// Returns the DM channel name for the given agent name.
/// Workers use this channel for private communication with the lead.
pub fn create_dm_channel_name(agent_name: &str) -> String {
    format!("dm-{agent_name}")
}

/// Clean up agent records that have been stopped for more than 24 hours.
/// Returns the list of agent IDs to remove.
/// Leads are never garbage-collected (they may be resumed).
pub fn garbage_collect_agents(proj: &Projections) -> Vec<String> {
    let cutoff = Utc::now() - Duration::hours(24);

    proj.agents
        .by_id
        .values()
        .filter(|agent| {
            // Only GC stopped agents
            !proj.agents.running.contains(&agent.id)
                // That stopped more than 24h ago
                && agent.stopped_at.is_some_and(|t| t < cutoff)
                // Don't GC leads (they may be resumed)
                && agent.kind != AgentKind::Lead
        })
        .map(|agent| agent.id.clone())
        .collect()
}

/// Decision function wrapper for scheduler registration.
/// Returns GarbageCollect commands for all eligible agents.
pub fn gc_decision(proj: &Projections, _channel: &str) -> Vec<Command> {
    garbage_collect_agents(proj)
        .into_iter()
        .map(|id| Command::GarbageCollect { agent_id: id })
        .collect()
}
