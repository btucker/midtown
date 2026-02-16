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

#[path = "clustering_tests.rs"]
#[cfg(test)]
mod tests;
