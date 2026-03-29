//! Behavioral tests for v2-spec.md Section 3 (partial): PR Integration
//!
//! Each test maps to a specific SHALL requirement from the spec.

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::decisions::prs::{
    handle_merged_prs, spawn_reviewers, suspend_authors_with_prs,
};
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_worker_with_task(proj: &mut Projections, agent_id: &str, task_id: &str) {
    proj.apply(&DomainEvent::TaskCreated {
        id: task_id.into(),
        subject: format!("Task {task_id}"),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: agent_id.into(),
        name: format!("worker-{agent_id}"),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some(task_id.into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: agent_id.into(),
        pid: 1000,
        session_id: Some("sess-w".into()),
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: task_id.into(),
        agent_id: agent_id.into(),
    });
}

// ── Section 3.2: Reviewer Spawning ───────────────────────────────────────────

/// Spec 3.2: WHEN a PR needs review AND no reviewer is running for it THEN the
/// system SHALL spawn a reviewer named reviewer-{pr_num}
#[test]
fn spawns_reviewer_when_pr_needs_review() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 101,
        branch: "feature/login".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 101 });

    let commands = spawn_reviewers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.name == "reviewer-101"
        ),
        "expected SpawnAgent named reviewer-101, got {:?}",
        commands[0]
    );
}

/// Spec 3.2: WHEN a reviewer is already running THEN the system SHALL NOT spawn
/// another one
#[test]
fn no_duplicate_reviewer_when_already_running() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 101,
        branch: "feature/login".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 101 });
    proj.apply(&DomainEvent::AgentCreated {
        id: "rev-1".into(),
        name: "reviewer-101".into(),
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
        id: "rev-1".into(),
        pid: 5000,
        session_id: None,
    });

    let commands = spawn_reviewers(&proj);

    assert!(
        commands.is_empty(),
        "should not spawn reviewer when one is already running, got {:?}",
        commands
    );
}

/// Spec 3.2: WHEN spawning a reviewer THEN the initial prompt SHALL be
/// "Review PR #{pr_num}: {branch}"
#[test]
fn reviewer_initial_prompt_format() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix/auth-bug".into(),
        author: "alice".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 42 });

    let commands = spawn_reviewers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg)
                if cfg.initial_prompt.as_deref() == Some("Review PR #42: fix/auth-bug")
        ),
        "expected initial_prompt 'Review PR #42: fix/auth-bug', got {:?}",
        commands[0]
    );
}

/// Spec 3.2: WHEN spawning a reviewer THEN the agent_type SHALL be
/// midtown-code-reviewer
#[test]
fn reviewer_uses_code_reviewer_agent_type() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 7,
        branch: "main".into(),
        author: "bob".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 7 });

    let commands = spawn_reviewers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.agent_type == "midtown-code-reviewer"
        ),
        "expected midtown-code-reviewer agent_type, got {:?}",
        commands[0]
    );
}

/// Spec 3.2: WHEN a PR does not need review THEN the system SHALL NOT spawn a
/// reviewer
#[test]
fn no_reviewer_for_pr_not_needing_review() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 99,
        branch: "docs/update".into(),
        author: "dev".into(),
    });
    // No PrReviewRequested event

    let commands = spawn_reviewers(&proj);

    assert!(
        commands.is_empty(),
        "expected no reviewer spawn when review not requested, got {:?}",
        commands
    );
}

// ── Section 3.3: PR Lifecycle ─────────────────────────────────────────────────

/// Spec 3.3: WHEN a PR merges AND has a linked InProgress task THEN the system
/// SHALL complete the task
#[test]
fn merged_pr_completes_linked_in_progress_task() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Feature work".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 55,
        branch: "feature/new-thing".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 55,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 55,
        branch: "feature/new-thing".into(),
    });

    let commands = handle_merged_prs(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::CompleteTask { task_id } if task_id == "t1"),
        "expected CompleteTask for t1, got {:?}",
        commands[0]
    );
}

/// Spec 3.3: WHEN a PR merges AND the linked task is already Completed THEN the
/// system SHALL NOT emit CompleteTask again
#[test]
fn merged_pr_does_not_re_complete_already_completed_task() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Done work".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    proj.apply(&DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 55,
        branch: "done/branch".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 55,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 55,
        branch: "done/branch".into(),
    });

    let commands = handle_merged_prs(&proj);

    assert!(
        commands.is_empty(),
        "expected no commands for already-completed task, got {:?}",
        commands
    );
}

/// Spec 3.3: WHEN a PR merges with no linked task THEN the system SHALL emit no
/// commands
#[test]
fn merged_pr_without_task_produces_no_commands() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 77,
        branch: "fix/typo".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 77,
        branch: "fix/typo".into(),
    });

    let commands = handle_merged_prs(&proj);

    assert!(
        commands.is_empty(),
        "expected no commands for merged PR without linked task, got {:?}",
        commands
    );
}

/// Spec 3.3: WHEN a worker's task has an open PR awaiting review THEN the system
/// SHALL stop the worker
#[test]
fn worker_stopped_when_task_has_open_pr_awaiting_review() {
    let mut proj = Projections::default();
    make_worker_with_task(&mut proj, "a1", "t1");
    proj.apply(&DomainEvent::PrOpened {
        number: 33,
        branch: "feat/thing".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 33,
        task_id: "t1".into(),
    });

    let commands = suspend_authors_with_prs(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::StopAgent { id, .. } if id == "a1"
        ),
        "expected StopAgent for a1 with open PR, got {:?}",
        commands[0]
    );
}

/// Spec 3.3: WHEN a worker's task has a merged PR THEN the system SHALL NOT stop
/// the worker (it should be completed instead, handled separately)
#[test]
fn worker_not_stopped_for_merged_pr() {
    let mut proj = Projections::default();
    make_worker_with_task(&mut proj, "a1", "t1");
    proj.apply(&DomainEvent::PrOpened {
        number: 33,
        branch: "feat/thing".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 33,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 33,
        branch: "feat/thing".into(),
    });

    let commands = suspend_authors_with_prs(&proj);

    assert!(
        commands.is_empty(),
        "expected no stop for merged PR, got {:?}",
        commands
    );
}

/// Spec 3.3: WHEN a worker's task has a closed (not merged) PR THEN the system
/// SHALL NOT stop the worker
#[test]
fn worker_not_stopped_for_closed_pr() {
    let mut proj = Projections::default();
    make_worker_with_task(&mut proj, "a1", "t1");
    proj.apply(&DomainEvent::PrOpened {
        number: 33,
        branch: "feat/thing".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 33,
        task_id: "t1".into(),
    });
    proj.apply(&DomainEvent::PrClosed { number: 33 });

    let commands = suspend_authors_with_prs(&proj);

    assert!(
        commands.is_empty(),
        "expected no stop for closed PR, got {:?}",
        commands
    );
}
