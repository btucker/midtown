//! Behavioral tests for v2-spec.md Section 6: Projections
//!
//! Each test maps to a specific SHALL requirement from the spec.

use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::agents::AgentIndex;
use crate::daemon_v2::projections::work::WorkIndex;

// ── Section 6.1: AgentIndex ───────────────────────────────────────────────────

/// Spec 6.1: WHEN AgentCreated is applied THEN the agent SHALL be indexed by id,
/// name, task, channel, and thread
#[test]
fn agent_created_indexed_by_id_name_task_channel_thread() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "swift-hawk".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("backend".into()),
        task_id: Some("task-99".into()),
        bound_thread_id: Some("thread-xyz".into()),
        icon: None,
        color: None,
    });

    // indexed by id
    assert!(
        idx.by_id.contains_key("a1"),
        "agent should be indexed by id"
    );

    // indexed by name
    assert_eq!(
        idx.by_name.get("swift-hawk"),
        Some(&"a1".to_string()),
        "agent should be indexed by name"
    );

    // indexed by task
    assert_eq!(
        idx.by_task.get("task-99"),
        Some(&"a1".to_string()),
        "agent should be indexed by task_id"
    );

    // indexed by channel
    assert!(
        idx.by_channel
            .get("backend")
            .is_some_and(|ids| ids.contains(&"a1".to_string())),
        "agent should be indexed by channel"
    );

    // indexed by thread
    assert_eq!(
        idx.by_thread.get("thread-xyz"),
        Some(&"a1".to_string()),
        "agent should be indexed by bound_thread_id"
    );
}

/// Spec 6.1: WHEN AgentCreated has no task_id, channel, or thread THEN optional
/// indexes are not populated
#[test]
fn agent_created_minimal_no_optional_indexes() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "calm-river".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });

    assert!(idx.by_id.contains_key("a2"), "should be indexed by id");
    assert!(
        idx.by_task.is_empty(),
        "by_task should be empty with no task_id"
    );
    assert!(
        idx.by_channel.is_empty(),
        "by_channel should be empty with no channel"
    );
    assert!(
        idx.by_thread.is_empty(),
        "by_thread should be empty with no thread"
    );
}

/// Spec 6.1: WHEN AgentStarted is applied THEN pid and session_id SHALL be set,
/// agent added to running set, started_at set to now
#[test]
fn agent_started_sets_pid_session_running_and_started_at() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "bold-cliff".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });

    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 9876,
        session_id: Some("sess-abc".into()),
    });

    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.pid, Some(9876), "pid should be set");
    assert_eq!(
        agent.session_id,
        Some("sess-abc".into()),
        "session_id should be set"
    );
    assert!(
        idx.running.contains("a1"),
        "agent should be added to running set"
    );
    assert!(agent.started_at.is_some(), "started_at should be set");
}

/// Spec 6.1: WHEN AgentStopped is applied THEN agent removed from running set,
/// stopped_at set, thread binding preserved
#[test]
fn agent_stopped_removes_from_running_sets_stopped_at_preserves_thread() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "f1".into(),
        name: "fork-one".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: Some("thread-abc".into()),
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "f1".into(),
        pid: 111,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "f1".into(),
        reason: "idle".into(),
    });

    assert!(
        !idx.running.contains("f1"),
        "stopped agent should be removed from running"
    );

    let agent = idx.by_id.get("f1").unwrap();
    assert!(agent.stopped_at.is_some(), "stopped_at should be set");

    // Thread binding preserved — essential for resume-on-thread-activity
    assert_eq!(
        agent.bound_thread_id,
        Some("thread-abc".into()),
        "thread binding should be preserved after stop"
    );
    assert!(
        idx.by_thread.contains_key("thread-abc"),
        "by_thread index should still contain the stopped agent's thread"
    );
}

/// Spec 6.1: WHEN AgentResumed is applied THEN pid updated, started_at reset,
/// stopped_at cleared, added back to running set
#[test]
fn agent_resumed_updates_pid_clears_stopped_at_restores_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "warm-ember".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1000,
        session_id: Some("original-sess".into()),
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "a1".into(),
        reason: "crashed".into(),
    });

    // Verify it is stopped
    assert!(!idx.running.contains("a1"));

    idx.apply(&DomainEvent::AgentResumed {
        id: "a1".into(),
        pid: 2000,
    });

    assert!(
        idx.running.contains("a1"),
        "resumed agent should be added back to running"
    );
    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.pid, Some(2000), "pid should be updated to new pid");
    assert!(
        agent.stopped_at.is_none(),
        "stopped_at should be cleared on resume"
    );
    assert!(
        agent.started_at.is_some(),
        "started_at should be reset on resume"
    );
}

/// Spec 6.1: WHEN AgentGarbageCollected is applied THEN agent marked as GC'd,
/// excluded from routing indexes, but record preserved in by_id
#[test]
fn agent_gc_preserves_record_but_excludes_from_routing() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "gc1".into(),
        name: "old-worker".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("backend".into()),
        task_id: Some("t99".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "gc1".into(),
        pid: 999,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "gc1".into(),
        reason: "done".into(),
    });
    idx.apply(&DomainEvent::AgentGarbageCollected { id: "gc1".into() });

    // Record preserved
    assert!(
        idx.by_id.contains_key("gc1"),
        "GC'd agent should still be in by_id"
    );
    let agent = idx.by_id.get("gc1").unwrap();
    assert!(agent.gc, "agent should be marked as gc=true");

    // Excluded from routing
    assert!(
        !idx.by_name.contains_key("old-worker"),
        "GC'd agent should be removed from by_name"
    );
    assert!(
        !idx.by_task.contains_key("t99"),
        "GC'd agent should be removed from by_task"
    );
    assert!(
        !idx.running.contains("gc1"),
        "GC'd agent should be removed from running"
    );
}

// ── Section 6.2: WorkIndex ────────────────────────────────────────────────────

/// Spec 6.2: WHEN TaskCreated is applied THEN task added to tasks map and
/// pending_tasks list
#[test]
fn task_created_added_to_tasks_and_pending() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Build the thing".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });

    assert!(idx.tasks.contains_key("t1"), "task should be in tasks map");
    assert!(
        idx.pending_tasks.contains(&"t1".to_string()),
        "task should be in pending_tasks list"
    );
    assert_eq!(
        idx.tasks.get("t1").unwrap().status,
        TaskStatus::Pending,
        "new task status should be Pending"
    );
}

/// Spec 6.2: WHEN TaskCreated has blocked_by THEN task added to blocked map
#[test]
fn task_created_with_blocked_by_added_to_blocked_map() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "First".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Second".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        icon: None,
        parent: None,
    });

    assert!(
        idx.blocked.contains_key("t2"),
        "blocked task should be in blocked map"
    );
    assert!(
        !idx.blocked.contains_key("t1"),
        "unblocked task should not be in blocked map"
    );
}

/// Spec 6.2: WHEN TaskAssigned is applied THEN status changes to InProgress,
/// moved from pending to in_progress list
#[test]
fn task_assigned_moves_to_in_progress() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Work item".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });

    assert_eq!(
        idx.tasks.get("t1").unwrap().status,
        TaskStatus::InProgress,
        "status should be InProgress after assignment"
    );
    assert!(
        !idx.pending_tasks.contains(&"t1".to_string()),
        "task should be removed from pending_tasks"
    );
    assert!(
        idx.in_progress_tasks.contains(&"t1".to_string()),
        "task should be in in_progress_tasks"
    );
}

/// Spec 6.2: WHEN TaskCompleted is applied THEN status changes to Completed,
/// removed from in_progress, completed_at set
#[test]
fn task_completed_removes_from_in_progress_and_sets_completed_at() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Complete me".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    idx.apply(&DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    });

    let task = idx.tasks.get("t1").unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "status should be Completed"
    );
    assert!(
        !idx.in_progress_tasks.contains(&"t1".to_string()),
        "task should be removed from in_progress_tasks"
    );
    assert!(
        task.completed_at.is_some(),
        "completed_at should be set after completion"
    );
}

/// Spec 6.2: WHEN PrLinkedToTask is applied THEN task's pr_number set
#[test]
fn pr_linked_to_task_sets_pr_number_on_task() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "PR work".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::PrOpened {
        number: 88,
        branch: "feat/pr-linked".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrLinkedToTask {
        number: 88,
        task_id: "t1".into(),
    });

    let task = idx.tasks.get("t1").unwrap();
    assert_eq!(
        task.pr_number,
        Some(88),
        "task's pr_number should be set to 88 after PrLinkedToTask"
    );
}

/// Spec 6.2: WHEN PrLinkedToTask is applied THEN task_for_pr lookup works
#[test]
fn pr_linked_to_task_enables_reverse_lookup() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "PR work".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::PrOpened {
        number: 88,
        branch: "feat/pr-linked".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrLinkedToTask {
        number: 88,
        task_id: "t1".into(),
    });

    let (task_id, _) = idx.task_for_pr(88).expect("should find task for pr 88");
    assert_eq!(task_id, "t1", "task_for_pr should return t1");
}

/// Spec 6.2: Verify pending_unblocked correctly excludes blocked tasks
#[test]
fn pending_unblocked_excludes_blocked_tasks() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Unblocked".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Blocked by t1".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        icon: None,
        parent: None,
    });

    let unblocked = idx.pending_unblocked();
    assert_eq!(unblocked.len(), 1, "only one task should be unblocked");
    assert_eq!(*unblocked[0], "t1", "t1 should be the unblocked task");
}
