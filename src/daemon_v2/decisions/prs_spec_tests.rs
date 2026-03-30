//! Behavioral tests for v2-spec.md Section 3 (partial): PR Integration
//!
//! Each test maps to a specific SHALL requirement from the spec.

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::decisions::prs::{
    handle_merged_prs, nudge_rebase_after_merge, resume_dead_reviewers, route_pr_comment,
    spawn_reviewers, suspend_authors_with_prs,
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
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
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
/// system SHALL spawn a reviewer named {author_name}-reviewer
#[test]
fn spawns_reviewer_named_after_author() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 101,
        branch: "feature/login".into(),
        author: "park".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 101 });

    let commands = spawn_reviewers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.name == "park-reviewer"
        ),
        "expected SpawnAgent named park-reviewer, got {:?}",
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
        name: "dev-reviewer".into(),
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
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
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
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
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

/// Spec 3.3: WHEN a PR merges THEN the system SHALL nudge workers with other
/// open PRs to rebase (1hr cooldown per agent)
#[test]
fn merged_pr_nudges_other_workers_to_rebase() {
    let mut proj = Projections::default();

    // Worker A has merged PR
    proj.apply(&DomainEvent::PrOpened {
        number: 10,
        branch: "feat-a".into(),
        author: "dev-a".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: 10,
        branch: "feat-a".into(),
    });

    // Worker B has an open PR — should get nudged to rebase
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "worker-b".into(),
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
        session_id: Some("sess-b".into()),
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Feat B".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t2".into(),
        agent_id: "a2".into(),
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 20,
        branch: "feat-b".into(),
        author: "dev-b".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 20,
        task_id: "t2".into(),
    });

    let commands = nudge_rebase_after_merge(&proj);

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "a2")),
        "worker B should be nudged to rebase, got {:?}",
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

// ── Section 3.2: Dead Reviewer Resume ──────────────────────────────────────

/// Spec 3.2: WHEN a reviewer dies AND has a session ID THEN the system SHALL
/// resume it
#[test]
fn dead_reviewer_with_session_id_is_resumed() {
    let mut proj = Projections::default();

    // Create a reviewer agent that has been stopped but has a session_id
    proj.apply(&DomainEvent::AgentCreated {
        id: "rev-1".into(),
        name: "alice-reviewer".into(),
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
        session_id: Some("sess-rev".into()),
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "rev-1".into(),
        reason: "process exited".into(),
    });

    let commands = resume_dead_reviewers(&proj);
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::ResumeAgent { id } if id == "rev-1"),
        "dead reviewer with session_id should be resumed, got {:?}",
        commands
    );
}

/// Spec 3.2: WHEN a reviewer dies AND has no session ID THEN spawn_reviewers
/// handles replacement (not resume_dead_reviewers)
#[test]
fn dead_reviewer_without_session_not_resumed() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::AgentCreated {
        id: "rev-2".into(),
        name: "bob-reviewer".into(),
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
        id: "rev-2".into(),
        pid: 5001,
        session_id: None, // No session_id
    });
    proj.apply(&DomainEvent::AgentStopped {
        id: "rev-2".into(),
        reason: "process exited".into(),
    });

    let commands = resume_dead_reviewers(&proj);
    assert!(
        commands.is_empty(),
        "reviewer without session_id should NOT be resumed, got {:?}",
        commands
    );
}

/// Spec 3.2 + 4.4: GC'd reviewers should NOT be counted toward restart limit
/// or attempted for resume
#[test]
fn gced_reviewer_excluded_from_restart_count_and_resume() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::PrOpened {
        number: 50,
        branch: "feat/gc-test".into(),
        author: "dev".into(),
    });
    proj.apply(&DomainEvent::PrReviewRequested { number: 50 });

    // Create and stop 3 reviewers (would normally hit the limit)
    for i in 0..3 {
        let id = format!("rev-gc-{i}");
        proj.apply(&DomainEvent::AgentCreated {
            id: id.clone(),
            name: "dev-reviewer".into(),
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
            pid: 5000 + i as u32,
            session_id: Some(format!("sess-gc-{i}")),
        });
        proj.apply(&DomainEvent::AgentStopped {
            id: id.clone(),
            reason: "exited".into(),
        });
        // GC all of them
        proj.apply(&DomainEvent::AgentGarbageCollected { id });
    }

    // GC'd reviewers should not count — spawn_reviewers should spawn a new one
    let commands = spawn_reviewers(&proj);
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::SpawnAgent(cfg) if cfg.name == "dev-reviewer")),
        "GC'd reviewers should not count toward restart limit — should spawn new reviewer, got {:?}",
        commands
    );

    // GC'd reviewers should not be resumed
    let resume_commands = resume_dead_reviewers(&proj);
    assert!(
        resume_commands.is_empty(),
        "GC'd reviewers should NOT be resumed, got {:?}",
        resume_commands
    );
}

// ── Section 3.3: PR Comment Routing ────────────────────────────────────────

/// Spec 3.3: WHEN a new comment is posted on a PR THEN the system SHALL post it
/// to the task's channel AND nudge the author agent
#[test]
fn pr_comment_posts_to_channel_and_nudges_author() {
    let mut proj = Projections::default();
    make_worker_with_task(&mut proj, "w1", "t1");

    // Link task to PR
    proj.apply(&DomainEvent::PrOpened {
        number: 50,
        branch: "feat/x".into(),
        author: "worker-w1".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 50,
        task_id: "t1".into(),
    });

    let commands = route_pr_comment(&proj, 50, "reviewer-bob", "LGTM");

    // Should post to channel
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::Post { channel, .. } if channel == "main")),
        "should post comment to task channel, got {:?}",
        commands
    );

    // Should nudge the author agent
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "w1")),
        "should nudge author agent, got {:?}",
        commands
    );
}

/// Spec 3.3: UNLESS the comment's frontmatter identifies the commenter as the author agent
#[test]
fn pr_comment_skips_nudge_when_commenter_is_author() {
    let mut proj = Projections::default();
    make_worker_with_task(&mut proj, "w1", "t1");

    proj.apply(&DomainEvent::PrOpened {
        number: 50,
        branch: "feat/x".into(),
        author: "worker-w1".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 50,
        task_id: "t1".into(),
    });

    // Comment from the author agent itself (name matches)
    let commands = route_pr_comment(&proj, 50, "worker-w1", "Updated the code");

    // Should still post to channel
    assert!(
        commands.iter().any(|c| matches!(c, Command::Post { .. })),
        "should still post comment to channel"
    );

    // Should NOT nudge (self-comment)
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { .. })),
        "should NOT nudge author for their own comment, got {:?}",
        commands
    );
}

/// Spec 3.3: PR comment SHALL be posted to the task's channel thread
/// when the author agent has a bound_thread_id
#[test]
fn pr_comment_uses_agent_thread_id() {
    let mut proj = Projections::default();

    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Thread task".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    // Create agent with a bound_thread_id
    proj.apply(&DomainEvent::AgentCreated {
        id: "w1".into(),
        name: "thread-worker".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
        bound_thread_id: Some("thread-task-1".into()),
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "w1".into(),
        pid: 1000,
        session_id: Some("sess-w".into()),
    });
    proj.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "w1".into(),
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 60,
        branch: "feat/thread".into(),
        author: "thread-worker".into(),
    });
    proj.apply(&DomainEvent::PrLinkedToTask {
        number: 60,
        task_id: "t1".into(),
    });

    let commands = route_pr_comment(&proj, 60, "reviewer-alice", "Looks good");

    // The Post command should include thread_id from the agent's bound_thread_id
    let post = commands
        .iter()
        .find(|c| matches!(c, Command::Post { .. }))
        .expect("should have a Post command");
    match post {
        Command::Post { thread_id, .. } => {
            assert_eq!(
                thread_id.as_deref(),
                Some("thread-task-1"),
                "PR comment should be posted to the agent's bound thread"
            );
        }
        _ => unreachable!(),
    }
}

/// Spec 3.3: WHEN PR has no linked task THEN no routing
#[test]
fn pr_comment_no_linked_task_no_routing() {
    let proj = Projections::default();

    let commands = route_pr_comment(&proj, 999, "reviewer", "comment");
    assert!(
        commands.is_empty(),
        "no linked task should produce no commands, got {:?}",
        commands
    );
}
