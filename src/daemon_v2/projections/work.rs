use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::daemon_v2::events::{CiStatus, DomainEvent, ReviewState, TaskId, TaskStatus};

#[path = "work_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub subject: String,
    pub channel: String,
    pub status: TaskStatus,
    pub pr_number: Option<u64>,
    pub blocked_by: Vec<TaskId>,
    pub agent_type: Option<String>,
    pub agent_name: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub parent: Option<TaskId>,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrState {
    pub number: u64,
    pub branch: String,
    /// GitHub username of the PR author.
    pub author: String,
    /// Midtown agent name that created this PR (set via PrLinkedToTask).
    pub midtown_author: Option<String>,
    pub ci_status: CiStatus,
    pub review_state: ReviewState,
    pub is_merged: bool,
    pub is_closed: bool,
    pub needs_review: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkIndex {
    pub tasks: HashMap<TaskId, Task>,
    pub prs: HashMap<u64, PrState>,
    pub pending_tasks: Vec<TaskId>,
    pub in_progress_tasks: Vec<TaskId>,
    pub open_prs: Vec<u64>,
    pub needing_review: Vec<u64>,
    pub blocked: HashMap<TaskId, Vec<TaskId>>,
    /// Parent → children mapping for task hierarchy (spec 1.3)
    pub children: HashMap<TaskId, Vec<TaskId>>,
}

impl WorkIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::TaskCreated {
                id,
                subject,
                channel,
                blocked_by,
                agent_type,
                agent_name,
                icon,
                color,
                parent,
                thread_id,
                message_id,
            } => {
                let task = Task {
                    id: id.clone(),
                    subject: subject.clone(),
                    channel: channel.clone(),
                    status: TaskStatus::Pending,
                    pr_number: None,
                    blocked_by: blocked_by.clone(),
                    agent_type: agent_type.clone(),
                    agent_name: agent_name.clone(),
                    icon: icon.clone(),
                    color: color.clone(),
                    parent: parent.clone(),
                    thread_id: thread_id.clone(),
                    message_id: message_id.clone(),
                    created_at: Utc::now(),
                    completed_at: None,
                };
                self.tasks.insert(id.clone(), task);
                self.pending_tasks.push(id.clone());
                if !blocked_by.is_empty() {
                    self.blocked.insert(id.clone(), blocked_by.clone());
                }
                if let Some(parent_id) = parent {
                    self.children
                        .entry(parent_id.clone())
                        .or_default()
                        .push(id.clone());
                }
            }
            DomainEvent::TaskAssigned { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::InProgress;
                    self.pending_tasks.retain(|id| id != task_id);
                    self.in_progress_tasks.push(task_id.clone());
                }
            }
            DomainEvent::TaskCompleted { task_id } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Completed;
                    task.completed_at = Some(Utc::now());
                    self.in_progress_tasks.retain(|id| id != task_id);
                }
            }
            DomainEvent::TaskReset { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    self.in_progress_tasks.retain(|id| id != task_id);
                    if !self.pending_tasks.contains(task_id) {
                        self.pending_tasks.push(task_id.clone());
                    }
                }
            }
            DomainEvent::TaskUnblocked { task_id } => {
                self.blocked.remove(task_id);
            }
            DomainEvent::PrOpened {
                number,
                branch,
                author,
            } => {
                let pr = PrState {
                    number: *number,
                    branch: branch.clone(),
                    author: author.clone(),
                    midtown_author: None,
                    ci_status: CiStatus::Pending,
                    review_state: ReviewState::None,
                    is_merged: false,
                    is_closed: false,
                    needs_review: false,
                };
                self.prs.insert(*number, pr);
                self.open_prs.push(*number);
            }
            DomainEvent::PrUpdated {
                number,
                ci_status,
                review_state,
            } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.ci_status = ci_status.clone();
                    pr.review_state = review_state.clone();
                }
            }
            DomainEvent::PrMerged { number, .. } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.is_merged = true;
                    pr.is_closed = true;
                    pr.needs_review = false;
                }
                self.open_prs.retain(|n| n != number);
                self.needing_review.retain(|n| n != number);
            }
            DomainEvent::PrClosed { number } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.is_closed = true;
                    pr.needs_review = false;
                }
                self.open_prs.retain(|n| n != number);
                self.needing_review.retain(|n| n != number);
            }
            DomainEvent::PrReviewRequested { number } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.needs_review = true;
                }
                if !self.needing_review.contains(number) {
                    self.needing_review.push(*number);
                }
            }
            DomainEvent::PrLinkedToTask { number, task_id } => {
                // Resolve the agent name from the linked task
                let agent_name = self.tasks.get(task_id).and_then(|t| t.agent_name.clone());
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.pr_number = Some(*number);
                }
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.midtown_author = agent_name;
                }
            }
            _ => {}
        }
    }

    pub fn pr_for_task(&self, id: &TaskId) -> Option<&PrState> {
        self.tasks.get(id)?.pr_number.and_then(|n| self.prs.get(&n))
    }

    pub fn task_for_pr(&self, pr: u64) -> Option<(&TaskId, &Task)> {
        self.tasks.iter().find(|(_, t)| t.pr_number == Some(pr))
    }

    /// Return all descendant task IDs (children, grandchildren, etc.) of a given task.
    pub fn descendants_of(&self, task_id: &str) -> Vec<TaskId> {
        let mut result = Vec::new();
        let mut stack: Vec<&str> = vec![task_id];
        while let Some(id) = stack.pop() {
            if let Some(kids) = self.children.get(id) {
                for kid in kids {
                    result.push(kid.clone());
                    stack.push(kid);
                }
            }
        }
        result
    }

    pub fn pending_unblocked(&self) -> Vec<&TaskId> {
        self.pending_tasks
            .iter()
            .filter(|id| !self.blocked.contains_key(*id))
            .collect()
    }
}
