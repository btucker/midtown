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
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AgentIndex {
    pub by_id: HashMap<AgentId, Agent>,
    pub by_name: HashMap<String, AgentId>,
    pub by_task: HashMap<TaskId, AgentId>,
    pub by_channel: HashMap<String, Vec<AgentId>>,
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
            } => {
                let agent = Agent {
                    id: id.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                    agent_type: agent_type.clone(),
                    provider: provider.clone(),
                    channel: channel.clone(),
                    task_id: task_id.clone(),
                    pid: None,
                    started_at: None,
                    stopped_at: None,
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
                self.by_id.insert(id.clone(), agent);
            }
            DomainEvent::AgentStarted { id, pid } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = Some(*pid);
                    agent.started_at = Some(Utc::now());
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            DomainEvent::AgentStopped { id, .. } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = None;
                    agent.stopped_at = Some(Utc::now());
                    self.running.remove(id);
                }
            }
            DomainEvent::AgentResumed { id } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            _ => {}
        }
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
