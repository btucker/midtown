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
            channel_tasks
                .entry(channel.clone())
                .or_default()
                .push(task);
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
            effects.push(Effect::ArchiveChannel {
                name: channel_name,
            });
        }
    }

    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal task for testing.
    fn mock_task(id: &str, status: TaskStatus, channel: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            subject: format!("Task {}", id),
            description: None,
            status,
            owner: None,
            blocked_by: Vec::new(),
            pr: None,
            channel: channel.map(|s| s.to_string()),
            created_at: None,
        }
    }

    #[test]
    fn test_channel_with_all_tasks_completed_gets_archived() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, Some("auth-feature")),
            mock_task("2", TaskStatus::Completed, Some("auth-feature")),
            mock_task("3", TaskStatus::Completed, Some("auth-feature")),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::ArchiveChannel { name } => {
                assert_eq!(name, "auth-feature");
            }
            _ => panic!("Expected ArchiveChannel effect, got {:?}", effects[0]),
        }
    }

    #[test]
    fn test_channel_with_pending_tasks_not_archived() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, Some("feature-x")),
            mock_task("2", TaskStatus::Pending, Some("feature-x")),
            mock_task("3", TaskStatus::Completed, Some("feature-x")),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_channel_with_in_progress_tasks_not_archived() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, Some("refactor")),
            mock_task("2", TaskStatus::InProgress, Some("refactor")),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_midtown_channel_never_archived() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, Some("midtown")),
            mock_task("2", TaskStatus::Completed, Some("midtown")),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_tasks_without_channel_ignored() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, None),
            mock_task("2", TaskStatus::Completed, Some("topic-a")),
            mock_task("3", TaskStatus::Pending, None),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::ArchiveChannel { name } => {
                assert_eq!(name, "topic-a");
            }
            _ => panic!("Expected ArchiveChannel effect"),
        }
    }

    #[test]
    fn test_multiple_channels_archived_independently() {
        let tasks = vec![
            mock_task("1", TaskStatus::Completed, Some("channel-a")),
            mock_task("2", TaskStatus::Completed, Some("channel-a")),
            mock_task("3", TaskStatus::Pending, Some("channel-b")),
            mock_task("4", TaskStatus::Completed, Some("channel-b")),
            mock_task("5", TaskStatus::Completed, Some("channel-c")),
        ];

        let effects = collect_auto_archive_effects(&tasks, "test-repo");

        assert_eq!(effects.len(), 2);

        let archived: Vec<String> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::ArchiveChannel { name } => Some(name.clone()),
                _ => None,
            })
            .collect();

        assert!(archived.contains(&"channel-a".to_string()));
        assert!(archived.contains(&"channel-c".to_string()));
        assert!(!archived.contains(&"channel-b".to_string()));
    }

    #[test]
    fn test_empty_task_list_no_archive() {
        let tasks = vec![];
        let effects = collect_auto_archive_effects(&tasks, "test-repo");
        assert_eq!(effects.len(), 0);
    }
}
