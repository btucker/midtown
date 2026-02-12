//! Auto-archiving logic for topic channels.
//!
//! When all tasks in a topic channel are completed, the channel should be automatically
//! archived to reduce clutter in the UI. The main "midtown" channel is never archived.

use super::effects::Effect;
use crate::tasks::{Task, TaskStatus};
use std::collections::HashMap;

/// Collect auto-archive effects for channels where all tasks are completed.
///
/// This is a pure decision function that examines the task list and returns
/// ArchiveChannel effects for topic channels that have no pending/in-progress tasks.
///
/// Rules:
/// - Only archives topic channels (never "midtown")
/// - Archives when ALL tasks in a channel are Completed
/// - Ignores tasks without a channel assignment
/// - Returns empty vec if no channels should be archived
pub fn collect_auto_archive_effects(tasks: &[Task], _repo_name: &str) -> Vec<Effect> {
    // Group tasks by channel
    let mut channel_tasks: HashMap<String, Vec<&Task>> = HashMap::new();

    for task in tasks {
        if let Some(ref channel) = task.channel {
            channel_tasks.entry(channel.clone()).or_default().push(task);
        }
    }

    let mut effects = Vec::new();

    // Check each channel to see if all tasks are completed
    for (channel_name, channel_task_list) in channel_tasks {
        // Never archive the main "midtown" channel
        if channel_name == "midtown" {
            continue;
        }

        // Check if all tasks in this channel are completed
        let all_completed = channel_task_list
            .iter()
            .all(|task| task.status == TaskStatus::Completed);

        if all_completed && !channel_task_list.is_empty() {
            effects.push(Effect::ArchiveChannel { name: channel_name });
        }
    }

    effects
}

#[path = "auto_archive_tests.rs"]
#[cfg(test)]
mod tests;
