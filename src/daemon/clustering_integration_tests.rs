//! Integration tests for clustering system.
//!
//! These tests verify that ClusteringDiff can be converted to Effects and that
//! the effects correctly update task-channel mappings.

use super::apply_clustering_diff;
use crate::clustering::{ClusteringDiff, CreateChannel, TaskAssignment};
use crate::daemon::effects::Effect;

#[test]
fn test_clustering_to_effects_creates_assign_task_channel() {
    // This test verifies the critical integration point: ClusteringDiff → Effects
    // that include AssignTaskChannel, which updates persistent state.

    let diff = ClusteringDiff {
        create_channels: vec![CreateChannel {
            name: "test-channel".to_string(),
            tasks: vec!["1234".to_string()],
        }],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "1234".to_string(),
            channel: "test-channel".to_string(),
        }],
    };

    let effects = apply_clustering_diff(diff);

    // Should produce 2 effects: CreateChannel + AssignTaskChannel
    assert_eq!(effects.len(), 2);

    // First should be CreateChannel
    match &effects[0] {
        Effect::CreateChannel {
            name,
            initial_tasks,
        } => {
            assert_eq!(name, "test-channel");
            assert_eq!(initial_tasks, &vec!["1234".to_string()]);
        }
        _ => panic!("Expected CreateChannel effect, got {:?}", effects[0]),
    }

    // Second should be AssignTaskChannel (the critical effect for persistence)
    match &effects[1] {
        Effect::AssignTaskChannel { task_id, channel } => {
            assert_eq!(task_id, "1234");
            assert_eq!(channel, "test-channel");
        }
        _ => panic!("Expected AssignTaskChannel effect, got {:?}", effects[1]),
    }
}

#[test]
fn test_clustering_reassignment_produces_correct_effect() {
    // Verify that reassigning an existing task to a different channel
    // produces the correct AssignTaskChannel effect.

    let diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![TaskAssignment {
            task: "5678".to_string(),
            channel: "new-channel".to_string(),
        }],
    };

    let effects = apply_clustering_diff(diff);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::AssignTaskChannel { task_id, channel } => {
            assert_eq!(task_id, "5678");
            assert_eq!(channel, "new-channel");
        }
        _ => panic!("Expected AssignTaskChannel effect"),
    }
}

#[test]
fn test_clustering_with_multiple_assignments() {
    // Verify that multiple task assignments all produce effects.

    let diff = ClusteringDiff {
        create_channels: vec![],
        archive_channels: vec![],
        merge_channels: vec![],
        assign_tasks: vec![
            TaskAssignment {
                task: "100".to_string(),
                channel: "channel-a".to_string(),
            },
            TaskAssignment {
                task: "101".to_string(),
                channel: "channel-a".to_string(),
            },
            TaskAssignment {
                task: "102".to_string(),
                channel: "channel-b".to_string(),
            },
        ],
    };

    let effects = apply_clustering_diff(diff);

    assert_eq!(effects.len(), 3);

    // All should be AssignTaskChannel effects
    for (i, effect) in effects.iter().enumerate() {
        match effect {
            Effect::AssignTaskChannel { task_id, channel } => {
                let expected_task = format!("{}", 100 + i);
                assert_eq!(task_id, &expected_task);
                assert!(channel == "channel-a" || channel == "channel-b");
            }
            _ => panic!("Expected AssignTaskChannel effect at index {}", i),
        }
    }
}
