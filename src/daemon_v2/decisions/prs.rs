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

const MAX_REVIEWER_RESTARTS: usize = 3;

/// For each open PR that needs review and doesn't already have a running reviewer agent,
/// spawn a reviewer agent. Escalates to ops after MAX_REVIEWER_RESTARTS failed attempts.
pub fn spawn_reviewers(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for pr_num in &proj.work.needing_review {
        let reviewer_name = format!("reviewer-{pr_num}");

        // Skip if reviewer is already running
        if proj
            .agents
            .by_name
            .get(&reviewer_name)
            .is_some_and(|id| proj.agents.running.contains(id))
        {
            continue;
        }

        let pr = match proj.work.prs.get(pr_num) {
            Some(pr) => pr,
            None => continue,
        };

        // Count stopped reviewer agents for this PR
        let restart_count = count_stopped_reviewers(proj, *pr_num);

        if restart_count >= MAX_REVIEWER_RESTARTS {
            // Escalate to ops — don't spawn another reviewer
            commands.push(Command::PostSystem {
                channel: "ops".into(),
                content: format!(
                    "Reviewer for PR #{pr_num} failed {restart_count} times. Manual review needed."
                ),
            });
            continue;
        }

        commands.push(Command::SpawnAgent(SpawnConfig {
            name: reviewer_name,
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
        }));
    }

    commands
}

/// Count how many times a reviewer for a given PR has been created and stopped.
fn count_stopped_reviewers(proj: &Projections, pr_num: u64) -> usize {
    let reviewer_name = format!("reviewer-{pr_num}");
    proj.agents
        .by_id
        .values()
        .filter(|a| {
            a.name == reviewer_name
                && a.agent_type == "midtown-code-reviewer"
                && a.stopped_at.is_some()
        })
        .count()
}

/// After a PR merges, nudge running workers with open PRs to rebase.
pub fn nudge_rebase_after_merge(proj: &Projections) -> Vec<Command> {
    // Only act if there are recently merged PRs
    let has_merged = proj.work.prs.values().any(|pr| pr.is_merged);
    if !has_merged {
        return vec![];
    }

    // Find running workers whose tasks have open (not merged, not closed) PRs
    proj.agents
        .running
        .iter()
        .filter_map(|id| proj.agents.by_id.get(id))
        .filter(|agent| agent.kind == AgentKind::Worker)
        .filter_map(|agent| {
            let task_id = agent.task_id.as_ref()?;
            let pr = proj.work.pr_for_task(task_id)?;
            if pr.is_merged || pr.is_closed {
                return None;
            }
            Some(Command::NudgeAgent {
                id: agent.id.clone(),
                message: format!(
                    "A PR was recently merged. Please rebase your branch for PR #{} to avoid merge conflicts.",
                    pr.number
                ),
            })
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
