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

// --- suspend_authors_with_prs tests ---

#[test]
fn suspends_author_with_open_pr() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix".into(),
        channel: "main".into(),
        blocked_by: vec![],
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
