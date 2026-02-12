//! Tests for auto-archiving channels when all tasks complete.

use crate::daemon::effects::Effect;
use crate::tasks::TaskStatus;
use std::collections::HashSet;

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
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("auth-feature")),
        mock_task("2", TaskStatus::Completed, Some("auth-feature")),
        mock_task("3", TaskStatus::Completed, Some("auth-feature")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());

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

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_channel_with_in_progress_tasks_not_archived() {
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("refactor")),
        mock_task("2", TaskStatus::InProgress, Some("refactor")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_midtown_channel_never_archived() {
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("midtown")),
        mock_task("2", TaskStatus::Completed, Some("midtown")),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_tasks_without_channel_ignored() {
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, None),
        mock_task("2", TaskStatus::Completed, Some("topic-a")),
        mock_task("3", TaskStatus::Pending, None),
    ];

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());

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

    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());

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
    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_already_archived_channel_skipped() {
    // All tasks completed for "auth-feature", but it's already archived
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("auth-feature")),
        mock_task("2", TaskStatus::Completed, Some("auth-feature")),
        mock_task("3", TaskStatus::Completed, Some("fresh-channel")),
    ];

    let archived = HashSet::from(["auth-feature".to_string()]);
    let effects = super::collect_auto_archive_effects(&tasks, &archived);

    // Only "fresh-channel" should be archived; "auth-feature" is already archived
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ArchiveChannel { name } => {
            assert_eq!(name, "fresh-channel");
        }
        _ => panic!("Expected ArchiveChannel effect for fresh-channel"),
    }
}

#[test]
fn test_second_call_with_archived_produces_no_effects() {
    // Simulate: first tick archives "auth-feature", second tick should skip it
    let tasks = vec![
        mock_task("1", TaskStatus::Completed, Some("auth-feature")),
        mock_task("2", TaskStatus::Completed, Some("auth-feature")),
    ];

    // First call: no archived channels yet
    let effects = super::collect_auto_archive_effects(&tasks, &HashSet::new());
    assert_eq!(effects.len(), 1);

    // Second call: "auth-feature" is now in the archived set
    let archived = HashSet::from(["auth-feature".to_string()]);
    let effects = super::collect_auto_archive_effects(&tasks, &archived);
    assert_eq!(
        effects.len(),
        0,
        "Should not re-archive an already-archived channel"
    );
}
