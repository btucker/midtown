#[path = "prs_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "prs_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::{AgentKind, Provider, TaskStatus};
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::cooldowns::CooldownCategory;

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

pub(crate) const MAX_REVIEWER_RESTARTS: usize = 3;

/// For each open PR that needs review and doesn't already have a running reviewer agent,
/// spawn a reviewer agent. Escalates to ops after MAX_REVIEWER_RESTARTS failed attempts.
pub fn spawn_reviewers(proj: &Projections) -> Vec<Command> {
    let mut commands = Vec::new();

    for pr_num in &proj.work.needing_review {
        let pr = match proj.work.prs.get(pr_num) {
            Some(pr) => pr,
            None => continue,
        };
        // Spec 3.2: reviewer named {author_name}-reviewer
        let reviewer_name = format!("{}-reviewer", pr.author);

        // Skip if reviewer is already running
        if proj
            .agents
            .by_name
            .get(&reviewer_name)
            .is_some_and(|id| proj.agents.running.contains(id))
        {
            continue;
        }

        // Count stopped reviewer agents for this PR
        let restart_count = count_stopped_reviewers(proj, *pr_num);

        if restart_count >= MAX_REVIEWER_RESTARTS {
            // Escalate to ops — but only once per PR (cooldown prevents repeat spam)
            let cooldown_key = format!("reviewer-escalation-{pr_num}");
            if !proj
                .cooldowns
                .is_active(CooldownCategory::TaskNudge, &cooldown_key)
            {
                commands.push(Command::PostSystem {
                    channel: "ops".into(),
                    content: format!(
                        "Reviewer for PR #{pr_num} failed {restart_count} times. Manual review needed."
                    ),
                });
            }
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

/// Count how many times a reviewer for a given PR author has been created and stopped.
fn count_stopped_reviewers(proj: &Projections, pr_num: u64) -> usize {
    let pr = match proj.work.prs.get(&pr_num) {
        Some(pr) => pr,
        None => return 0,
    };
    let reviewer_name = format!("{}-reviewer", pr.author);
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

/// Spec 3.2: Resume dead reviewer agents. If they can't be resumed, spawn_reviewers
/// handles the replacement (with retry limit).
pub fn resume_dead_reviewers(proj: &Projections) -> Vec<Command> {
    proj.agents
        .by_id
        .values()
        .filter(|a| {
            a.agent_type == "midtown-code-reviewer"
                && !proj.agents.running.contains(&a.id)
                && a.stopped_at.is_some()
                && a.session_id.is_some()
        })
        .map(|a| Command::ResumeAgent { id: a.id.clone() })
        .collect()
}

/// After a PR merges, nudge workers with open PRs to rebase.
/// Uses MergeRebaseNudge cooldown (1hr) per agent to avoid repeated nudging.
pub fn nudge_rebase_after_merge(proj: &Projections) -> Vec<Command> {
    use crate::daemon_v2::projections::cooldowns::CooldownCategory;

    let has_merged = proj.work.prs.values().any(|pr| pr.is_merged);
    if !has_merged {
        return vec![];
    }

    proj.agents
        .by_id
        .values()
        .filter(|agent| agent.kind == AgentKind::Worker)
        .filter(|agent| {
            !proj
                .cooldowns
                .is_active(CooldownCategory::MergeRebaseNudge, &agent.id)
        })
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

/// Spec 3.3/12: Route a PR comment to the task's channel thread and nudge the author agent.
/// Returns Post + NudgeAgent commands unless the commenter is the author agent itself.
pub fn route_pr_comment(
    proj: &Projections,
    pr_number: u64,
    commenter: &str,
    comment_body: &str,
) -> Vec<Command> {
    let mut commands = Vec::new();

    // Find the task linked to this PR
    let Some((task_id, task)) = proj.work.task_for_pr(pr_number) else {
        return commands;
    };

    // Find the agent assigned to this task
    let author_agent = proj.agents.by_task.get(task_id);

    // Check if the commenter IS the author agent (skip self-nudge)
    let is_self = author_agent.is_some_and(|aid| {
        proj.agents
            .by_id
            .get(aid)
            .is_some_and(|a| a.name == commenter)
    });

    // Post the comment to the task's channel thread
    commands.push(Command::Post {
        channel: task.channel.clone(),
        sender: commenter.to_string(),
        content: format!("[PR #{pr_number} comment] {comment_body}"),
        thread_id: None, // TODO: map to task thread when thread tracking is available
    });

    // Nudge the author agent unless commenter is the author
    if !is_self
        && let Some(agent_id) = author_agent
    {
        commands.push(Command::NudgeAgent {
            id: agent_id.clone(),
            message: format!("New comment on PR #{pr_number} from {commenter}: {comment_body}"),
        });
    }

    commands
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
