#[path = "prs_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::TaskStatus;
use crate::daemon_v2::projections::Projections;

/// For each merged PR that has a linked in-progress task, return a CompleteTask command.
pub fn handle_merged_prs(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for (&number, pr) in &proj.work.prs {
        if !pr.is_merged {
            continue;
        }

        if let Some((task_id, task)) = proj.work.task_for_pr(number)
            && task.status == TaskStatus::InProgress
        {
            commands.push(Command::CompleteTask {
                task_id: task_id.clone(),
            });
        }
    }

    commands
}
