//! Tests for auto-archiving channels when all tasks complete.

use super::super::effects::Effect;
use crate::tasks::TaskStatus;

/// Helper to create a minimal task list for testing.
fn mock_task(id: &str, status: TaskStatus, channel: Option<&str>) -> crate::tasks::Task {
    crate::tasks::Task {
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
    // Setup: A topic channel with 3 tasks, all completed
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("auth-feature")),
        mock_task("2", TaskStatus::Completed, Some("auth-feature")),
        mock_task("3", TaskStatus::Completed, Some("auth-feature")),
    ];

    // When we check for channels to archive
    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");

    // Should produce an ArchiveChannel effect
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
    // Setup: A topic channel with 2 completed tasks and 1 pending
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("feature-x")),
        mock_task("2", TaskStatus::Pending, Some("feature-x")),
        mock_task("3", TaskStatus::Completed, Some("feature-x")),
    ];

    // When we check for channels to archive
    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");

    // Should NOT produce any archive effects
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_channel_with_in_progress_tasks_not_archived() {
    // Setup: A topic channel with completed and in-progress tasks
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("refactor")),
        mock_task("2", TaskStatus::InProgress, Some("refactor")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_midtown_channel_never_archived() {
    // Setup: Tasks in the main "midtown" channel, all completed
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("midtown")),
        mock_task("2", TaskStatus::Completed, Some("midtown")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");

    // Midtown channel should never be archived, even if all tasks are complete
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_tasks_without_channel_ignored() {
    // Setup: Mix of tasks with and without channels
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, None),
        mock_task("2", TaskStatus::Completed, Some("topic-a")),
        mock_task("3", TaskStatus::Pending, None),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");

    // Topic-a should be archived (only has 1 task, which is completed)
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
    // Setup: Two topic channels, one complete, one not
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("channel-a")),
        mock_task("2", TaskStatus::Completed, Some("channel-a")),
        mock_task("3", TaskStatus::Pending, Some("channel-b")),
        mock_task("4", TaskStatus::Completed, Some("channel-b")),
        mock_task("5", TaskStatus::Completed, Some("channel-c")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");

    // Should archive channel-a and channel-c, but not channel-b
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
    let effects = super::collect_auto_archive_effects(&tasks, "test-repo");
    assert_eq!(effects.len(), 0);
}
