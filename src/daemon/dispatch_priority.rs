//! Dispatch priority ordering for pending tasks.
//!
//! Pure function — no I/O. Called once per tick to order pending tasks
//! before the dispatch loop iterates them.
//!
//! Priority tiers (stable sort — FIFO within each tier):
//! 1. Children of in-progress parents
//! 2. Tasks that block other tasks
//! 3. Everything else (FIFO by creation time)

use crate::tasks::Task;
use std::collections::{HashMap, HashSet};

fn tier(
    task: &Task,
    in_progress_task_ids: &HashSet<String>,
    task_parent_map: &HashMap<String, String>,
    blocks_map: &HashMap<String, Vec<String>>,
) -> u8 {
    if let Some(parent_id) = task_parent_map.get(&task.id)
        && in_progress_task_ids.contains(parent_id)
    {
        return 1;
    }
    if blocks_map.contains_key(&task.id) {
        return 2;
    }
    3
}

#[allow(dead_code)]
pub(crate) fn prioritize_pending_tasks(
    pending_tasks: &[Task],
    in_progress_task_ids: &HashSet<String>,
    task_parent_map: &HashMap<String, String>,
    blocks_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut tasks: Vec<&Task> = pending_tasks.iter().collect();
    tasks.sort_by_key(|t| t.created_at);
    tasks.sort_by_key(|t| tier(t, in_progress_task_ids, task_parent_map, blocks_map));
    tasks.into_iter().map(|t| t.id.clone()).collect()
}

#[path = "dispatch_priority_tests.rs"]
#[cfg(test)]
mod tests;
