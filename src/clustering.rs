//! AI-driven channel clustering for task organization.
//!
//! The clusterer analyzes tasks and the current channel structure to produce
//! structured diffs that create, archive, merge, or reassign tasks to topic channels.
//! This keeps related work together while archiving channels when themes are exhausted.

use serde::{Deserialize, Serialize};

/// Structured diff from the AI clusterer describing channel operations.
///
/// The clusterer responds with this JSON structure to describe how to organize
/// tasks into topic channels. All fields are required but may be empty arrays.
///
/// # Example
///
/// ```json
/// {
///   "create_channels": [
///     {
///       "name": "webhook-security",
///       "tasks": ["1234", "1235"]
///     }
///   ],
///   "archive_channels": ["old-feature"],
///   "merge_channels": [
///     {
///       "from": "login-flow",
///       "into": "auth-module"
///     }
///   ],
///   "assign_tasks": [
///     {
///       "task": "1234",
///       "channel": "webhook-security"
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusteringDiff {
    /// New channels to create with initial task assignments.
    pub create_channels: Vec<CreateChannel>,

    /// Channels to archive (rename to .archived.jsonl).
    /// Only allowed when all tasks in the channel are completed.
    pub archive_channels: Vec<String>,

    /// Channel merge operations (merge `from` into `into`).
    /// The `from` channel is archived after merging.
    pub merge_channels: Vec<MergeChannel>,

    /// Task-to-channel assignments.
    /// Must include all pending tasks (both new and existing that need reassignment).
    pub assign_tasks: Vec<TaskAssignment>,
}

/// Request to create a new topic channel with initial tasks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateChannel {
    /// Name of the new channel (e.g., "webhook-security").
    /// Should use kebab-case and be descriptive.
    pub name: String,

    /// Task IDs to initially assign to this channel.
    pub tasks: Vec<String>,
}

/// Request to merge one channel into another.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeChannel {
    /// Source channel to merge (will be archived after merge).
    pub from: String,

    /// Target channel to merge into.
    pub into: String,
}

/// Assignment of a task to a specific channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskAssignment {
    /// Task ID (e.g., "1075").
    pub task: String,

    /// Target channel name.
    pub channel: String,
}

impl ClusteringDiff {
    /// Create an empty clustering diff with no operations.
    pub fn empty() -> Self {
        Self {
            create_channels: Vec::new(),
            archive_channels: Vec::new(),
            merge_channels: Vec::new(),
            assign_tasks: Vec::new(),
        }
    }

    /// Check if the diff is empty (no operations).
    pub fn is_empty(&self) -> bool {
        self.create_channels.is_empty()
            && self.archive_channels.is_empty()
            && self.merge_channels.is_empty()
            && self.assign_tasks.is_empty()
    }

    /// Validate the diff for logical consistency.
    ///
    /// Returns an error string if validation fails, or Ok(()) if valid.
    pub fn validate(&self) -> Result<(), String> {
        // 1. Check that all created channels have at least one task
        for create in &self.create_channels {
            if create.tasks.is_empty() {
                return Err(format!("Channel '{}' created with no tasks", create.name));
            }
            if create.name.is_empty() {
                return Err("Channel created with empty name".to_string());
            }
            // Check for invalid characters (spaces, etc.)
            if create.name.contains(' ') {
                return Err(format!(
                    "Channel name '{}' contains spaces (use kebab-case)",
                    create.name
                ));
            }
        }

        // 2. Cannot archive the main "midtown" channel
        for archive in &self.archive_channels {
            if archive == "midtown" {
                return Err("Cannot archive the 'midtown' channel".to_string());
            }
        }

        // 3. Merge operations must have distinct from/into
        for merge in &self.merge_channels {
            if merge.from == merge.into {
                return Err(format!("Cannot merge channel '{}' into itself", merge.from));
            }
            if merge.from == "midtown" || merge.into == "midtown" {
                return Err("Cannot merge the 'midtown' channel".to_string());
            }
        }

        // 4. All task assignments must have non-empty task and channel
        for assign in &self.assign_tasks {
            if assign.task.is_empty() {
                return Err("Task assignment with empty task ID".to_string());
            }
            if assign.channel.is_empty() {
                return Err(format!("Task '{}' assigned to empty channel", assign.task));
            }
        }

        // 5. Check that tasks assigned to new channels are listed in create_channels
        for assign in &self.assign_tasks {
            // If the channel is in the created list, verify the task is included in its tasks
            if let Some(create) = self
                .create_channels
                .iter()
                .find(|c| c.name == assign.channel)
                && !create.tasks.contains(&assign.task)
            {
                return Err(format!(
                    "Task '{}' assigned to new channel '{}' but not in create_channels.tasks",
                    assign.task, assign.channel
                ));
            }
        }

        // 6. Check that all tasks in create_channels.tasks have corresponding assign_tasks
        for create in &self.create_channels {
            for task in &create.tasks {
                if !self
                    .assign_tasks
                    .iter()
                    .any(|a| a.task == *task && a.channel == create.name)
                {
                    return Err(format!(
                        "Task '{}' in create_channels[{}].tasks has no matching assign_tasks entry",
                        task, create.name
                    ));
                }
            }
        }

        Ok(())
    }

    /// Parse a JSON string from the clusterer into a ClusteringDiff.
    ///
    /// Returns an error if the JSON is invalid or validation fails.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let diff: ClusteringDiff =
            serde_json::from_str(json).map_err(|e| format!("JSON parse error: {}", e))?;
        diff.validate()?;
        Ok(diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_diff() {
        let diff = ClusteringDiff::empty();
        assert!(diff.is_empty());
        assert!(diff.validate().is_ok());
    }

    #[test]
    fn test_valid_create_channel() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "webhook-security".to_string(),
                tasks: vec!["1234".to_string()],
            }],
            archive_channels: vec![],
            merge_channels: vec![],
            assign_tasks: vec![TaskAssignment {
                task: "1234".to_string(),
                channel: "webhook-security".to_string(),
            }],
        };
        assert!(!diff.is_empty());
        assert!(diff.validate().is_ok());
    }

    #[test]
    fn test_create_channel_with_no_tasks_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "empty-channel".to_string(),
                tasks: vec![],
            }],
            archive_channels: vec![],
            merge_channels: vec![],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_err());
        assert!(
            diff.validate()
                .unwrap_err()
                .contains("created with no tasks")
        );
    }

    #[test]
    fn test_create_channel_with_spaces_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "bad name".to_string(),
                tasks: vec!["1234".to_string()],
            }],
            archive_channels: vec![],
            merge_channels: vec![],
            assign_tasks: vec![TaskAssignment {
                task: "1234".to_string(),
                channel: "bad name".to_string(),
            }],
        };
        assert!(diff.validate().is_err());
        assert!(diff.validate().unwrap_err().contains("contains spaces"));
    }

    #[test]
    fn test_archive_midtown_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![],
            archive_channels: vec!["midtown".to_string()],
            merge_channels: vec![],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_err());
        assert!(
            diff.validate()
                .unwrap_err()
                .contains("Cannot archive the 'midtown'")
        );
    }

    #[test]
    fn test_merge_into_self_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![],
            archive_channels: vec![],
            merge_channels: vec![MergeChannel {
                from: "channel-a".to_string(),
                into: "channel-a".to_string(),
            }],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_err());
        assert!(diff.validate().unwrap_err().contains("into itself"));
    }

    #[test]
    fn test_merge_midtown_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![],
            archive_channels: vec![],
            merge_channels: vec![MergeChannel {
                from: "midtown".to_string(),
                into: "other".to_string(),
            }],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_err());
        assert!(
            diff.validate()
                .unwrap_err()
                .contains("Cannot merge the 'midtown'")
        );
    }

    #[test]
    fn test_task_in_create_but_not_assign_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "new-channel".to_string(),
                tasks: vec!["1234".to_string(), "1235".to_string()],
            }],
            archive_channels: vec![],
            merge_channels: vec![],
            assign_tasks: vec![TaskAssignment {
                task: "1234".to_string(),
                channel: "new-channel".to_string(),
            }],
        };
        assert!(diff.validate().is_err());
        assert!(
            diff.validate()
                .unwrap_err()
                .contains("has no matching assign_tasks")
        );
    }

    #[test]
    fn test_task_in_assign_but_not_create_fails() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "new-channel".to_string(),
                tasks: vec!["1234".to_string()],
            }],
            archive_channels: vec![],
            merge_channels: vec![],
            assign_tasks: vec![
                TaskAssignment {
                    task: "1234".to_string(),
                    channel: "new-channel".to_string(),
                },
                TaskAssignment {
                    task: "1235".to_string(),
                    channel: "new-channel".to_string(),
                },
            ],
        };
        assert!(diff.validate().is_err());
        assert!(
            diff.validate()
                .unwrap_err()
                .contains("not in create_channels.tasks")
        );
    }

    #[test]
    fn test_from_json_valid() {
        let json = r#"{
            "create_channels": [
                {
                    "name": "test-channel",
                    "tasks": ["100"]
                }
            ],
            "archive_channels": [],
            "merge_channels": [],
            "assign_tasks": [
                {
                    "task": "100",
                    "channel": "test-channel"
                }
            ]
        }"#;
        let diff = ClusteringDiff::from_json(json).unwrap();
        assert_eq!(diff.create_channels.len(), 1);
        assert_eq!(diff.create_channels[0].name, "test-channel");
    }

    #[test]
    fn test_from_json_invalid() {
        let json = "not json";
        assert!(ClusteringDiff::from_json(json).is_err());
    }

    #[test]
    fn test_from_json_validation_fails() {
        let json = r#"{
            "create_channels": [],
            "archive_channels": ["midtown"],
            "merge_channels": [],
            "assign_tasks": []
        }"#;
        assert!(ClusteringDiff::from_json(json).is_err());
    }

    #[test]
    fn test_valid_archive() {
        let diff = ClusteringDiff {
            create_channels: vec![],
            archive_channels: vec!["old-feature".to_string()],
            merge_channels: vec![],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_ok());
    }

    #[test]
    fn test_valid_merge() {
        let diff = ClusteringDiff {
            create_channels: vec![],
            archive_channels: vec![],
            merge_channels: vec![MergeChannel {
                from: "feature-a".to_string(),
                into: "feature-b".to_string(),
            }],
            assign_tasks: vec![],
        };
        assert!(diff.validate().is_ok());
    }

    #[test]
    fn test_complex_valid_diff() {
        let diff = ClusteringDiff {
            create_channels: vec![CreateChannel {
                name: "new-feature".to_string(),
                tasks: vec!["1001".to_string(), "1002".to_string()],
            }],
            archive_channels: vec!["completed-feature".to_string()],
            merge_channels: vec![MergeChannel {
                from: "old-work".to_string(),
                into: "main-work".to_string(),
            }],
            assign_tasks: vec![
                TaskAssignment {
                    task: "1001".to_string(),
                    channel: "new-feature".to_string(),
                },
                TaskAssignment {
                    task: "1002".to_string(),
                    channel: "new-feature".to_string(),
                },
                TaskAssignment {
                    task: "1003".to_string(),
                    channel: "existing-channel".to_string(),
                },
            ],
        };
        assert!(diff.validate().is_ok());
    }
}
