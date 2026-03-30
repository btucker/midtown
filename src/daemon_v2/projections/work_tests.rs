use super::*;
use crate::daemon_v2::events::*;

/// Spec 6.2: WHEN TaskCreated is applied THEN task added to tasks map and pending_tasks list
#[test]
fn create_task_adds_to_pending() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    assert_eq!(idx.pending_tasks.len(), 1);
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::Pending);
}

/// Spec 6.2: WHEN TaskAssigned is applied THEN status changes to InProgress,
/// moved from pending to in_progress list
#[test]
fn task_assigned_moves_to_in_progress() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    assert!(idx.pending_tasks.is_empty());
    assert_eq!(idx.in_progress_tasks.len(), 1);
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::InProgress);
}

/// Spec 6.2: WHEN TaskCompleted is applied THEN status changes to Completed,
/// removed from in_progress, completed_at set
#[test]
fn task_completed_removes_from_in_progress() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    idx.apply(&DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    });
    assert!(idx.in_progress_tasks.is_empty());
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::Completed);
    assert!(idx.tasks.get("t1").unwrap().completed_at.is_some());
}

/// Spec 6.2: WHEN TaskReset is applied THEN status reverts to Pending, moved back to pending list
#[test]
fn task_reset_returns_to_pending() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    idx.apply(&DomainEvent::TaskReset {
        task_id: "t1".into(),
        reason: "agent died".into(),
    });
    assert_eq!(idx.pending_tasks.len(), 1);
    assert!(idx.in_progress_tasks.is_empty());
}

/// Spec 6.2: WHEN TaskCreated has blocked_by THEN task added to blocked map
#[test]
fn blocked_tasks_tracked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "First".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Second".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    assert!(idx.blocked.contains_key("t2"));
    let unblocked = idx.pending_unblocked();
    assert_eq!(unblocked.len(), 1);
    assert_eq!(*unblocked[0], "t1");
}

/// Spec 6.2: WHEN TaskUnblocked is applied THEN task removed from blocked map
#[test]
fn unblock_removes_from_blocked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "First".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Second".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::TaskUnblocked {
        task_id: "t2".into(),
    });
    assert!(!idx.blocked.contains_key("t2"));
    assert_eq!(idx.pending_unblocked().len(), 2);
}

/// Spec 6.2: WHEN PrLinkedToTask is applied THEN task's pr_number set
#[test]
fn pr_linked_to_task() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrLinkedToTask {
        number: 42,
        task_id: "t1".into(),
    });
    assert_eq!(idx.pr_for_task(&"t1".into()).unwrap().number, 42);
    let (task_id, _) = idx.task_for_pr(42).unwrap();
    assert_eq!(task_id, "t1");
}

/// Spec 6.2: WHEN PrMerged is applied THEN is_merged/is_closed set, removed from open_prs
#[test]
fn pr_merged_tracked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrMerged {
        number: 42,
        branch: "fix-bug".into(),
    });
    assert!(idx.prs.get(&42).unwrap().is_merged);
    assert!(idx.prs.get(&42).unwrap().is_closed);
    assert!(!idx.open_prs.contains(&42));
}

/// Spec 6.2: WHEN PrReviewRequested is applied THEN needs_review set, added to needing_review
#[test]
fn pr_needing_review() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrReviewRequested { number: 42 });
    assert!(idx.needing_review.contains(&42));
    assert!(idx.prs.get(&42).unwrap().needs_review);
}

/// Spec 6.2: WHEN PrClosed is applied THEN is_closed set, removed from open_prs and needing_review
#[test]
fn pr_closed_tracked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrReviewRequested { number: 42 });
    idx.apply(&DomainEvent::PrClosed { number: 42 });
    assert!(idx.prs.get(&42).unwrap().is_closed);
    assert!(!idx.prs.get(&42).unwrap().is_merged);
    assert!(!idx.open_prs.contains(&42));
    assert!(!idx.needing_review.contains(&42));
}

/// Spec 6.2: WHEN PrUpdated is applied THEN ci_status and review_state updated
#[test]
fn pr_updated_changes_ci_and_review() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrUpdated {
        number: 42,
        ci_status: CiStatus::Passed,
        review_state: ReviewState::Approved,
    });
    let pr = idx.prs.get(&42).unwrap();
    assert_eq!(pr.ci_status, CiStatus::Passed);
    assert_eq!(pr.review_state, ReviewState::Approved);
}
