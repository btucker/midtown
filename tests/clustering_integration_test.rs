//! Integration tests for the clustering system.
//!
//! These tests verify that the clustering system correctly assigns tasks to channels
//! automatically when tasks are created without an explicit channel assignment.

use midtown::clustering::{ClusteringDiff, CreateChannel, TaskAssignment};
use midtown::daemon::Effect;

#[test]
fn test_clustering_diff_to_effects_creates_channel() {
    // Test that a clustering diff with a new channel creates the correct effects
    let diff = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "auth-module".to_string(),
            tasks: vec!["100".to_string(), "101".to_string()],
        }],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![
            TaskAssignment {
                task: "100".to_string(),
                channel: "auth-module".to_string(),
            },
            TaskAssignment {
                task: "101".to_string(),
                channel: "auth-module".to_string(),
            },
        ],
    };

    // Validate the diff
    assert!(diff.validate().is_ok(), "Diff should be valid");

    // Convert to effects
    let effects = midtown::daemon::apply_clustering_diff(diff);

    // Should have 1 CreateChannel effect + 2 AssignTaskChannel effects
    assert_eq!(effects.len(), 3);

    // First effect should be CreateChannel
    match &effects[0] {
        Effect::CreateChannel {
            name,
            initial_tasks,
        } => {
            assert_eq!(name, "auth-module");
            assert_eq!(initial_tasks.len(), 2);
            assert!(initial_tasks.contains(&"100".to_string()));
            assert!(initial_tasks.contains(&"101".to_string()));
        }
        _ => panic!("Expected CreateChannel effect"),
    }

    // Next effects should be AssignTaskChannel
    for effect in &effects[1..] {
        match effect {
            Effect::AssignTaskChannel { task_id, channel } => {
                assert_eq!(channel, "auth-module");
                assert!(task_id == "100" || task_id == "101");
            }
            _ => panic!("Expected AssignTaskChannel effect"),
        }
    }
}

#[test]
fn test_clustering_diff_validation_prevents_invalid_operations() {
    // Test that invalid diffs are caught by validation

    // Cannot assign task to "midtown" channel
    let invalid_diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "100".to_string(),
            channel: "midtown".to_string(),
        }],
    };
    assert!(
        invalid_diff.validate().is_err(),
        "Should reject assignment to midtown channel"
    );

    // Cannot archive "midtown" channel
    let invalid_diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec!["midtown".to_string()],
        merge_channels: vec![],
        assign_tasks: vec![],
    };
    assert!(
        invalid_diff.validate().is_err(),
        "Should reject archiving midtown channel"
    );

    // Created channel must have tasks
    let invalid_diff = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "empty-channel".to_string(),
            tasks: vec![],
        }],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![],
    };
    assert!(
        invalid_diff.validate().is_err(),
        "Should reject channel created with no tasks"
    );
}

#[test]
fn test_clustering_diff_roundtrip_json() {
    // Test that ClusteringDiff can be serialized and deserialized correctly
    let original = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "webhook-security".to_string(),
            tasks: vec!["1234".to_string()],
        }],
        archive_channels: vec!["old-feature".to_string()],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "1234".to_string(),
            channel: "webhook-security".to_string(),
        }],
    };

    // Serialize to JSON
    let json = serde_json::to_string(&original).expect("Should serialize");

    // Deserialize back
    let deserialized: ClusteringDiff = serde_json::from_str(&json).expect("Should deserialize");

    // Should match original
    assert_eq!(original, deserialized);

    // Should validate
    assert!(deserialized.validate().is_ok());
}

#[test]
fn test_effects_execution_order() {
    // Test that effects are generated in the correct order:
    // 1. CreateChannel
    // 2. MergeChannels
    // 3. ArchiveChannel
    // 4. AssignTaskChannel
    use midtown::clustering::MergeChannel;

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

    assert!(diff.validate().is_ok());

    let effects = midtown::daemon::apply_clustering_diff(diff);

    // Should have 1 CreateChannel + 1 MergeChannels + 1 ArchiveChannel + 2 AssignTaskChannel
    assert_eq!(effects.len(), 5);

    // Verify ordering
    match &effects[0] {
        Effect::CreateChannel { .. } => {}
        other => panic!("Expected CreateChannel as first effect, got {:?}", other),
    }

    match &effects[1] {
        Effect::MergeChannels { .. } => {}
        other => panic!("Expected MergeChannels as second effect, got {:?}", other),
    }

    match &effects[2] {
        Effect::ArchiveChannel { .. } => {}
        other => panic!("Expected ArchiveChannel as third effect, got {:?}", other),
    }

    match &effects[3] {
        Effect::AssignTaskChannel { .. } => {}
        other => panic!(
            "Expected AssignTaskChannel as fourth effect, got {:?}",
            other
        ),
    }

    match &effects[4] {
        Effect::AssignTaskChannel { .. } => {}
        other => panic!(
            "Expected AssignTaskChannel as fifth effect, got {:?}",
            other
        ),
    }
}

#[test]
fn test_empty_diff_produces_no_effects() {
    let diff = ClusteringDiff::empty();
    assert!(diff.is_empty());
    assert!(diff.validate().is_ok());

    let effects = midtown::daemon::apply_clustering_diff(diff);
    assert_eq!(effects.len(), 0, "Empty diff should produce no effects");
}

#[test]
fn test_clustering_diff_consistency_checks() {
    // Test that validation catches inconsistencies between create_channels and assign_tasks

    // Task in create_channels.tasks but missing from assign_tasks
    let inconsistent = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "test-channel".to_string(),
            tasks: vec!["100".to_string(), "101".to_string()],
        }],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "100".to_string(),
            channel: "test-channel".to_string(),
        }],
    };
    assert!(
        inconsistent.validate().is_err(),
        "Should reject task in create_channels but not in assign_tasks"
    );

    // Task in assign_tasks but not in create_channels.tasks (for new channel)
    let inconsistent = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "test-channel".to_string(),
            tasks: vec!["100".to_string()],
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
    assert!(
        inconsistent.validate().is_err(),
        "Should reject task in assign_tasks but not in create_channels.tasks"
    );
}
