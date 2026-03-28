#[path = "prs_tests.rs"]
#[cfg(test)]
mod tests;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
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

/// For each open PR that needs review and doesn't already have a running reviewer agent,
/// spawn a reviewer agent.
pub fn spawn_reviewers(proj: &Projections) -> Vec<Command> {
    proj.work
        .needing_review
        .iter()
        .filter(|pr_num| {
            let reviewer_name = format!("reviewer-{pr_num}");
            !proj
                .agents
                .by_name
                .get(&reviewer_name)
                .is_some_and(|id| proj.agents.running.contains(id))
        })
        .filter_map(|pr_num| {
            let pr = proj.work.prs.get(pr_num)?;
            Some(Command::SpawnAgent(SpawnConfig {
                name: format!("reviewer-{pr_num}"),
                kind: AgentKind::Worker,
                agent_type: "midtown-code-reviewer".into(),
                provider: Provider::ClaudeCode,
                channel: None,
                task_id: None,
                initial_prompt: Some(format!("Review PR #{pr_num}: {}", pr.branch)),
                working_dir: None,
                model: None,
                bound_thread_id: None,
                fork_from_session: None,
                icon: None,
                color: None,
            }))
        })
        .collect()
}

/// Stop worker agents whose tasks have open PRs (they're waiting for review).
pub fn suspend_authors_with_prs(proj: &Projections) -> Vec<Command> {
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .filter(|agent| agent.kind == AgentKind::Worker)
        .filter(|agent| {
            agent.task_id.as_ref().is_some_and(|tid| {
                proj.work
                    .pr_for_task(tid)
                    .is_some_and(|pr| !pr.is_merged && !pr.is_closed)
            })
        })
        .map(|agent| Command::StopAgent {
            id: agent.id.clone(),
            reason: "PR opened, waiting for review".into(),
        })
        .collect()
}
