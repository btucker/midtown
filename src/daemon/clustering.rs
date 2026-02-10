//! Channel clustering decision logic.
//!
//! Pure functions that convert ClusteringDiff into Effects for the daemon to execute.

use super::effects::Effect;
use crate::clustering::ClusteringDiff;

/// Convert a validated ClusteringDiff into executable Effects.
///
/// This is a pure decision function that takes the diff and returns effects
/// without performing any I/O. The effects are executed by `execute_effects()`.
///
/// # Order of operations
///
/// 1. Create new channels (CreateChannel)
/// 2. Merge channels (MergeChannels - includes archiving source)
/// 3. Archive standalone channels (ArchiveChannel)
/// 4. Assign tasks to channels (AssignTaskChannel)
///
/// # Validation
///
/// The diff must be validated before calling this function (use `ClusteringDiff::validate()`).
/// Invalid diffs may result in inconsistent effects.
#[allow(dead_code)] // Not yet wired to daemon event loop
pub fn apply_clustering_diff(diff: ClusteringDiff) -> Vec<Effect> {
    let mut effects = Vec::new();

    // 1. Create new channels first so they exist for task assignments
    for create in diff.create_channels {
        effects.push(Effect::CreateChannel {
            name: create.name,
            initial_tasks: create.tasks,
        });
    }

    // 2. Merge channels (this also archives the source channel)
    for merge in diff.merge_channels {
        effects.push(Effect::MergeChannels {
            from: merge.from,
            into: merge.into,
        });
    }

    // 3. Archive standalone channels (those not involved in merges)
    for archive in diff.archive_channels {
        effects.push(Effect::ArchiveChannel { name: archive });
    }

    // 4. Assign tasks to channels
    for assign in diff.assign_tasks {
        effects.push(Effect::AssignTaskChannel {
            task_id: assign.task,
            channel: assign.channel,
        });
    }

    effects
}

#[cfg(test)]
mod tests {
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
}
