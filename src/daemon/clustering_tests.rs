use super::*;
use crate::clustering::{CreateChannel, MergeChannel, TaskAssignment};

#[test]
fn test_empty_diff_produces_no_effects() {
    let diff = ClusteringDiff::empty();
    let effects = apply_clustering_diff(diff);
    assert_eq!(effects.len(), 0);
}

#[test]
fn test_create_channel_effect() {
    let diff = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "test-channel".to_string(),
            tasks: vec!["100".to_string(), "101".to_string()],
        }],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![
            TaskAssignment {
                task: "100".to_string(),
                channel: "test-channel".to_string(),
            },
            TaskAssignment {
                task: "101".to_string(),
                channel: "test-channel".to_string(),
            },
        ],
    };
    let effects = apply_clustering_diff(diff);
    // 1 CreateChannel + 2 AssignTaskChannel
    assert_eq!(effects.len(), 3);

    // First effect should be CreateChannel
    match &effects[0] {
        Effect::CreateChannel {
            name,
            initial_tasks,
        } => {
            assert_eq!(name, "test-channel");
            assert_eq!(initial_tasks.len(), 2);
        }
        _ => panic!("Expected CreateChannel effect"),
    }
}

#[test]
fn test_archive_channel_effect() {
    let diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec!["old-channel".to_string()],
        merge_channels: vec![],
        assign_tasks: vec![],
    };
    let effects = apply_clustering_diff(diff);
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::ArchiveChannel { name } => {
            assert_eq!(name, "old-channel");
        }
        _ => panic!("Expected ArchiveChannel effect"),
    }
}

#[test]
fn test_merge_channels_effect() {
    let diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec![],
        merge_channels: vec![MergeChannel {
            from: "feature-a".to_string(),
            into: "feature-b".to_string(),
        }],
        assign_tasks: vec![],
    };
    let effects = apply_clustering_diff(diff);
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::MergeChannels { from, into } => {
            assert_eq!(from, "feature-a");
            assert_eq!(into, "feature-b");
        }
        _ => panic!("Expected MergeChannels effect"),
    }
}

#[test]
fn test_assign_task_channel_effect() {
    let diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "1234".to_string(),
            channel: "existing-channel".to_string(),
        }],
    };
    let effects = apply_clustering_diff(diff);
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::AssignTaskChannel { task_id, channel } => {
            assert_eq!(task_id, "1234");
            assert_eq!(channel, "existing-channel");
        }
        _ => panic!("Expected AssignTaskChannel effect"),
    }
}

#[test]
fn test_complex_diff_ordering() {
    let diff = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "new-channel".to_string(),
            tasks: vec!["100".to_string()],
        }],
        archive_channels: vec!["completed".to_string()],
        merge_channels: vec![MergeChannel {
            from: "old-work".to_string(),
            into: "main-work".to_string(),
        }],
        assign_tasks: vec![
            TaskAssignment {
                task: "100".to_string(),
                channel: "new-channel".to_string(),
            },
            TaskAssignment {
                task: "101".to_string(),
                channel: "main-work".to_string(),
            },
        ],
    };
    let effects = apply_clustering_diff(diff);

    // 1 CreateChannel + 1 MergeChannels + 1 ArchiveChannel + 2 AssignTaskChannel
    assert_eq!(effects.len(), 5);

    // Verify ordering:
    // 1. CreateChannel first
    match &effects[0] {
        Effect::CreateChannel { .. } => {}
        _ => panic!("Expected CreateChannel as first effect"),
    }

    // 2. MergeChannels second
    match &effects[1] {
        Effect::MergeChannels { .. } => {}
        _ => panic!("Expected MergeChannels as second effect"),
    }

    // 3. ArchiveChannel third
    match &effects[2] {
        Effect::ArchiveChannel { .. } => {}
        _ => panic!("Expected ArchiveChannel as third effect"),
    }

    // 4-5. AssignTaskChannel last
    match &effects[3] {
        Effect::AssignTaskChannel { .. } => {}
        _ => panic!("Expected AssignTaskChannel as fourth effect"),
    }
    match &effects[4] {
        Effect::AssignTaskChannel { .. } => {}
        _ => panic!("Expected AssignTaskChannel as fifth effect"),
    }
}
