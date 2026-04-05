use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::daemon_v2::events::{AgentId, AgentKind, DomainEvent, Provider, TaskId};

#[path = "agents_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub agent_type: String,
    pub provider: Provider,
    pub channel: Option<String>,
    pub task_id: Option<TaskId>,
    pub bound_thread_id: Option<String>,
    pub session_id: Option<String>,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub icon: Option<String>,
    pub color: Option<String>,
    /// Last reported state (e.g. "idle", "working")
    #[serde(default)]
    pub reported_state: Option<String>,
    /// When the last state report was received
    #[serde(default)]
    pub state_reported_at: Option<DateTime<Utc>>,
    /// True when the agent has been garbage-collected (excluded from routing/dispatch)
    #[serde(default)]
    pub gc: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIndex {
    pub by_id: HashMap<AgentId, Agent>,
    pub by_name: HashMap<String, AgentId>,
    pub by_task: HashMap<TaskId, AgentId>,
    pub by_channel: HashMap<String, Vec<AgentId>>,
    pub by_thread: HashMap<String, AgentId>,
    pub running: HashSet<AgentId>,
}

impl AgentIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::AgentCreated {
                id,
                name,
                kind,
                agent_type,
                provider,
                channel,
                task_id,
                bound_thread_id,
                icon,
                color,
            } => {
                let agent = Agent {
                    id: id.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                    agent_type: agent_type.clone(),
                    provider: provider.clone(),
                    channel: channel.clone(),
                    task_id: task_id.clone(),
                    bound_thread_id: bound_thread_id.clone(),
                    session_id: None,
                    pid: None,
                    started_at: None,
                    stopped_at: None,
                    icon: icon.clone(),
                    color: color.clone(),
                    reported_state: None,
                    state_reported_at: None,
                    gc: false,
                };
                self.by_name.insert(name.clone(), id.clone());
                if let Some(task_id) = task_id {
                    self.by_task.insert(task_id.clone(), id.clone());
                }
                if let Some(channel) = channel {
                    self.by_channel
                        .entry(channel.clone())
                        .or_default()
                        .push(id.clone());
                }
                if let Some(thread_id) = bound_thread_id {
                    self.by_thread.insert(thread_id.clone(), id.clone());
                }
                self.by_id.insert(id.clone(), agent);
            }
            DomainEvent::AgentStarted {
                id,
                pid,
                session_id,
            } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = Some(*pid);
                    agent.session_id = session_id.clone();
                    agent.started_at = Some(Utc::now());
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            DomainEvent::AgentStopped { id, .. } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    // Thread binding persists through stop — the agent can be
                    // resumed to handle new thread messages. Only GC clears it.
                    agent.pid = None;
                    agent.stopped_at = Some(Utc::now());
                    self.running.remove(id);
                }
            }
            DomainEvent::AgentStateReported { id, state } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.reported_state = Some(state.clone());
                    agent.state_reported_at = Some(Utc::now());
                }
            }
            DomainEvent::AgentResumed { id, pid } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = Some(*pid);
                    agent.started_at = Some(Utc::now());
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            DomainEvent::AgentGarbageCollected { id } => {
                // Mark as GC'd but preserve the record (spec 6.1)
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.gc = true;
                    // Remove from routing indexes
                    self.by_name.remove(&agent.name);
                    self.running.remove(id);
                    if let Some(task_id) = &agent.task_id {
                        self.by_task.remove(task_id);
                    }
                    let channel = agent.channel.clone();
                    let thread_id = agent.bound_thread_id.clone();
                    if let Some(ch) = channel
                        && let Some(list) = self.by_channel.get_mut(ch.as_str())
                    {
                        list.retain(|aid| aid != id);
                        if list.is_empty() {
                            self.by_channel.remove(&ch);
                        }
                    }
                    if let Some(tid) = thread_id {
                        self.by_thread.remove(&tid);
                    }
                }
            }
            DomainEvent::AgentSessionNotFound { name } => {
                if let Some(agent_id) = self.by_name.get(name).cloned()
                    && let Some(agent) = self.by_id.get_mut(&agent_id)
                {
                    agent.session_id = None;
                    // Also mark as not running — the session is gone, so the
                    // agent can't be interacted with until respawned.
                    self.running.remove(&agent_id);
                }
            }
            DomainEvent::ChannelRenamed { old_name, new_name } => {
                // Update by_channel index
                if let Some(agent_ids) = self.by_channel.remove(old_name) {
                    // Update each agent's channel field
                    for aid in &agent_ids {
                        if let Some(agent) = self.by_id.get_mut(aid) {
                            agent.channel = Some(new_name.clone());
                        }
                    }
                    self.by_channel.insert(new_name.clone(), agent_ids);
                }
            }
            _ => {}
        }
    }

    pub fn fork_for_thread(&self, thread_id: &str) -> Option<&Agent> {
        self.by_thread
            .get(thread_id)
            .and_then(|id| self.by_id.get(id))
    }

    /// Find the lead agent for a channel (regardless of running state).
    /// Find the lead agent for a channel. Prefers a running lead; falls back
    /// to the most recently created stopped lead (for resume-on-nudge).
    pub fn channel_lead(&self, channel: &str) -> Option<&Agent> {
        let agents = self.by_channel.get(channel)?;
        // Prefer a running lead
        if let Some(agent) = agents
            .iter()
            .filter(|id| self.running.contains(*id))
            .filter_map(|id| self.by_id.get(id))
            .find(|a| a.kind == AgentKind::Lead && !a.gc)
        {
            return Some(agent);
        }
        // Fall back to the last (most recent) non-GC'd lead
        agents
            .iter()
            .rev()
            .filter_map(|id| self.by_id.get(id))
            .find(|a| a.kind == AgentKind::Lead && !a.gc)
    }

    pub fn idle_workers(&self) -> Vec<AgentId> {
        self.running
            .iter()
            .filter(|id| {
                self.by_id
                    .get(*id)
                    .is_some_and(|a| a.kind == AgentKind::Worker && a.task_id.is_none())
            })
            .cloned()
            .collect()
    }
}
