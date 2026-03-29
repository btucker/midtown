use super::*;
use crate::daemon_v2::events::{AgentKind, DomainEvent, Provider};
use crate::daemon_v2::projections::Projections;

fn make_projections_with_merged_pr(
    pr_number: u64,
    task_id: Option<&str>,
    task_status: Option<TaskStatus>,
) -> Projections {
    let mut proj = Projections::default();

    // Add the merged PR
    proj.apply(&DomainEvent::PrOpened {
        number: pr_number,
        branch: format!("feat/pr-{pr_number}"),
        author: "alice".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: pr_number,
        branch: format!("feat/pr-{pr_number}"),
    });

    // Optionally create and link a task
    if let Some(tid) = task_id {
        proj.apply(&DomainEvent::TaskCreated {
            id: tid.to_string(),
            subject: "test task".into(),
            channel: "general".into(),
            blocked_by: vec![],
            agent_type: None,
            icon: None,
        });

        if task_status == Some(TaskStatus::InProgress) {
            proj.apply(&DomainEvent::TaskAssigned {
                task_id: tid.to_string(),
                agent_id: "agent-1".into(),
            });
        }
        if task_status == Some(TaskStatus::Completed) {
            proj.apply(&DomainEvent::TaskAssigned {
                task_id: tid.to_string(),
                agent_id: "agent-1".into(),
            });
            proj.apply(&DomainEvent::TaskCompleted {
                task_id: tid.to_string(),
            });
        }

        proj.apply(&DomainEvent::PrLinkedToTask {
            number: pr_number,
            task_id: tid.to_string(),
        });
    }

    proj
}

#[test]
fn merged_pr_with_linked_in_progress_task_returns_complete() {
    let proj = make_projections_with_merged_pr(42, Some("task-1"), Some(TaskStatus::InProgress));

    let commands = handle_merged_prs(&proj);
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0],
        Command::CompleteTask { task_id } if task_id == "task-1"
    ));
}

#[test]
fn merged_pr_without_task_returns_empty() {
    let proj = make_projections_with_merged_pr(42, None, None);

    let commands = handle_merged_prs(&proj);
    assert!(commands.is_empty());
}

#[test]
fn merged_pr_with_already_completed_task_returns_empty() {
    let proj = make_projections_with_merged_pr(42, Some("task-1"), Some(TaskStatus::Completed));

    let commands = handle_merged_prs(&proj);
    assert!(commands.is_empty());
}

// --- spawn_reviewers tests ---

#[test]
fn spawns_reviewer_for_pr_needing_review() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });

    let commands = spawn_reviewers(&proj);
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::SpawnAgent(c) if c.agent_type == "midtown-code-reviewer" && c.name == "reviewer-42")
    );
}

#[test]
fn no_duplicate_reviewer_when_already_running() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });
    // Reviewer already exists and is running
    proj.apply(&DomainEvent::AgentCreated {
        id: "r1".into(),
        name: "reviewer-42".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-reviewer".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "r1".into(),
        pid: 1234,
        session_id: None,
    });

    let commands = spawn_reviewers(&proj);
    assert!(commands.is_empty());
}

#[test]
fn respawns_reviewer_when_stopped() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });
    // Reviewer existed but stopped
    proj.apply(&DomainEvent::AgentCreated {
        id: "r1".into(),
        name: "reviewer-42".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-reviewer".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "r1".into(),
        pid: 1234,
        session_id: None,
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "r1".into(),
        reason: "done".into(),
    });

    let commands = spawn_reviewers(&proj);
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::SpawnAgent(c) if c.name == "reviewer-42"));
}

#[test]
fn no_reviewer_for_pr_not_needing_review() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    // No PrReviewRequested event

    let commands = spawn_reviewers(&proj);
    assert!(commands.is_empty());
}

// --- reviewer escalation tests ---

const MAX_REVIEWER_RESTARTS: usize = 3;

fn add_reviewer_attempt(proj: &mut Projections, pr_num: u64, attempt: usize) {
    let id = format!("reviewer-{pr_num}-attempt-{attempt}");
    proj.apply(&DomainEvent::AgentCreated {
        id: id.clone(),
        name: format!("reviewer-{pr_num}"),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-reviewer".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: id.clone(),
        pid: 1000 + attempt as u32,
        session_id: None,
    });
    proj.apply(&DomainEvent::AgentStopped {
        id,
        reason: "exited without posting review".into(),
    });
}

#[test]
fn spawn_reviewers_escalates_after_max_restarts() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });

    // Simulate MAX_REVIEWER_RESTARTS failed attempts
    for i in 0..MAX_REVIEWER_RESTARTS {
        add_reviewer_attempt(&mut proj, 42, i);
    }

    let commands = spawn_reviewers(&proj);

    // Should NOT spawn another reviewer — should post escalation to ops instead
    assert!(
        !commands.iter().any(|c| matches!(c, Command::SpawnAgent(_))),
        "should not spawn after max restarts, got {:?}",
        commands
    );
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::PostSystem { channel, content }
            if channel == "ops" && content.contains("42")
        )),
        "expected ops escalation post, got {:?}",
        commands
    );
}

#[test]
fn spawn_reviewers_respawns_within_limit() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });

    // Only 1 failed attempt — under the limit
    add_reviewer_attempt(&mut proj, 42, 0);

    let commands = spawn_reviewers(&proj);

    // Should still spawn a new reviewer
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::SpawnAgent(cfg) if cfg.name == "reviewer-42")),
        "expected SpawnAgent for reviewer-42, got {:?}",
        commands
    );
}

// --- suspend_authors_with_prs tests ---

#[test]
fn suspends_author_with_open_pr() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "worker-1".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1,
        session_id: None,
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 42,
        task_id: "t1".into(),
    });

    let commands = suspend_authors_with_prs(&proj);
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::StopAgent { id, .. } if id == "a1"));
}

#[test]
fn no_suspend_for_merged_pr() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "worker-1".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1,
        session_id: None,
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 42,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 42,
        branch: "fix".into(),
    });

    let commands = suspend_authors_with_prs(&proj);
    assert!(commands.is_empty());
}

#[test]
fn no_suspend_for_closed_pr() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "worker-1".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1,
        session_id: None,
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 42,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrClosed { number: 42 });

    let commands = suspend_authors_with_prs(&proj);
    assert!(commands.is_empty());
}

#[test]
fn no_suspend_for_non_worker_agents() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "lead1".into(),
        name: "lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "lead1".into(),
        pid: 1,
        session_id: None,
    });

    let commands = suspend_authors_with_prs(&proj);
    assert!(commands.is_empty());
}

// --- nudge_rebase_after_merge tests ---

#[test]
fn nudge_rebase_after_merge_nudges_open_pr_workers() {
    let mut proj = Projections::default();

    // PR #1 is merged
    proj.apply(&DomainEvent::PrOpened {
        number: 1,
        branch: "feat-a".into(),
        author: "dev1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 1,
        branch: "feat-a".into(),
    });

    // PR #2 is still open with a running worker
    proj.apply(&DomainEvent::PrOpened {
        number: 2,
        branch: "feat-b".into(),
        author: "dev2".into(),
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Task B".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 2,
        task_id: "t2".into(),
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "worker-2".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t2".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a2".into(),
        pid: 200,
        session_id: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t2".into(),
        agent_id: "a2".into(),
    });

    let commands = nudge_rebase_after_merge(&proj);

    // Should nudge worker-2 to rebase their PR
    assert_eq!(commands.len(), 1, "expected 1 nudge, got {:?}", commands);
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, message }
            if id == "a2" && message.contains("rebase")),
        "expected rebase nudge for a2, got {:?}",
        commands[0]
    );
}

#[test]
fn no_rebase_nudge_when_no_merged_prs() {
    let mut proj = Projections::default();

    // Only open PRs, nothing merged
    proj.apply(&DomainEvent::PrOpened {
        number: 1,
        branch: "feat".into(),
        author: "dev".into(),
    });

    let commands = nudge_rebase_after_merge(&proj);
    assert!(
        commands.is_empty(),
        "no nudges when nothing merged, got {:?}",
        commands
    );
}

#[test]
fn rebase_nudge_includes_stopped_workers() {
    let mut proj = Projections::default();

    // PR #1 merged
    proj.apply(&DomainEvent::PrOpened {
        number: 1,
        branch: "feat-a".into(),
        author: "dev1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 1,
        branch: "feat-a".into(),
    });

    // PR #2 open, but worker is stopped
    proj.apply(&DomainEvent::PrOpened {
        number: 2,
        branch: "feat-b".into(),
        author: "dev2".into(),
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Task B".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 2,
        task_id: "t2".into(),
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "worker-2".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t2".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a2".into(),
        pid: 200,
        session_id: None,
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "a2".into(),
        reason: "stopped".into(),
    });

    let commands = nudge_rebase_after_merge(&proj);
    // Stopped workers still get nudged — executor resumes them
    assert_eq!(
        commands.len(),
        1,
        "stopped worker should still be nudged, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, .. } if id == "a2"),
        "expected nudge for a2, got {:?}",
        commands[0]
    );
}

#[test]
fn no_rebase_nudge_when_cooldown_active() {
    use crate::daemon_v2::projections::cooldowns::CooldownCategory;

    let mut proj = Projections::default();

    // PR #1 merged
    proj.apply(&DomainEvent::PrOpened {
        number: 1,
        branch: "feat-a".into(),
        author: "dev1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 1,
        branch: "feat-a".into(),
    });

    // PR #2 open with running worker
    proj.apply(&DomainEvent::PrOpened {
        number: 2,
        branch: "feat-b".into(),
        author: "dev2".into(),
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Task B".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 2,
        task_id: "t2".into(),
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "worker-2".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t2".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a2".into(),
        pid: 200,
        session_id: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t2".into(),
        agent_id: "a2".into(),
    });

    // Record cooldown for this agent — simulates a previous nudge
    proj.cooldowns
        .record(CooldownCategory::MergeRebaseNudge, "a2".into());

    let commands = nudge_rebase_after_merge(&proj);
    assert!(
        commands.is_empty(),
        "cooldown should prevent re-nudge, got {:?}",
        commands
    );
}
