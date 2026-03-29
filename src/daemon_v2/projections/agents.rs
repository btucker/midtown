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
            DomainEvent::AgentResumed { id, pid } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = Some(*pid);
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            DomainEvent::AgentGarbageCollected { id } => {
                if let Some(agent) = self.by_id.remove(id) {
                    self.by_name.remove(&agent.name);
                    self.running.remove(id);
                    if let Some(task_id) = &agent.task_id {
                        self.by_task.remove(task_id);
                    }
                    if let Some(channel) = &agent.channel
                        && let Some(list) = self.by_channel.get_mut(channel)
                    {
                        list.retain(|aid| aid != id);
                        if list.is_empty() {
                            self.by_channel.remove(channel);
                        }
                    }
                    if let Some(thread_id) = &agent.bound_thread_id {
                        self.by_thread.remove(thread_id);
                    }
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
