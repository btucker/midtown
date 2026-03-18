use super::*;

fn make_task(id: &str, subject: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        id: id.to_string(),
        subject: subject.to_string(),
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }
}

fn make_reviewer_task(id: &str, pr_number: u64) -> crate::tasks::Task {
    crate::tasks::Task {
        id: id.to_string(),
        subject: format!("Review PR #{}", pr_number),
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        description: None,
        blocked_by: vec![],
        channel: Some("ops".to_string()),
        pr: Some(pr_number),
        created_at: None,
    }
}

// ============================================================================
// should_skip_coworker_for_task tests
// ============================================================================

#[test]
fn skip_coworker_on_spawn_failure_cooldown() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.spawn_failure_cooldown_names.insert("york".to_string());
    let task = make_task("1", "Fix bug");

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn skip_coworker_already_dispatched_by_owned_phase() {
    let snap = snapshot::minimal_snapshot_for_test();
    let task = make_task("1", "Fix bug");
    let mut owned_dispatched = HashSet::new();
    owned_dispatched.insert("york".to_string());

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &owned_dispatched,
        &HashSet::new(),
    ));
}

#[test]
fn skip_coworker_already_assigned_to_same_task() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.name_task_assignments
        .insert("york".to_string(), "1".to_string());
    let task = make_task("1", "Fix bug");

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn dont_skip_coworker_assigned_to_different_task() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.name_task_assignments
        .insert("york".to_string(), "2".to_string());
    let task = make_task("1", "Fix bug");

    assert!(!should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn skip_running_reviewer_coworker() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_names.insert("york".to_string());
    snap.reviewer.active_reviewers.insert("york".to_string());
    let task = make_task("1", "Fix bug");

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn skip_running_busy_coworker_when_not_grouped() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_names.insert("york".to_string());
    snap.busy_coworkers.insert("york".to_string());
    let task = make_task("1", "Fix bug");

    // Not grouped → skip
    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn allow_running_busy_coworker_when_grouped() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_names.insert("york".to_string());
    snap.busy_coworkers.insert("york".to_string());
    let task = make_task("1", "Fix bug");

    // Grouped → allow (cross-tick grouping)
    assert!(!should_skip_coworker_for_task(
        &task,
        "york",
        true,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

#[test]
fn skip_running_coworker_assigned_this_tick_even_if_grouped() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_names.insert("york".to_string());
    let task = make_task("1", "Fix bug");
    let mut names_this_tick = HashSet::new();
    names_this_tick.insert("york".to_string());

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        true,
        &snap,
        &HashSet::new(),
        &names_this_tick,
    ));
}

#[test]
fn skip_not_running_coworker_assigned_this_tick() {
    let snap = snapshot::minimal_snapshot_for_test();
    let task = make_task("1", "Fix bug");
    let mut names_this_tick = HashSet::new();
    names_this_tick.insert("york".to_string());

    assert!(should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &names_this_tick,
    ));
}

#[test]
fn allow_fresh_available_coworker() {
    let snap = snapshot::minimal_snapshot_for_test();
    let task = make_task("1", "Fix bug");

    assert!(!should_skip_coworker_for_task(
        &task,
        "york",
        false,
        &snap,
        &HashSet::new(),
        &HashSet::new(),
    ));
}

// ============================================================================
// build_grouped_nudge_effects tests
// ============================================================================

#[test]
fn grouped_nudge_produces_nudge_with_callbacks() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.name_session_map
        .insert("york".to_string(), "session-123".to_string());
    let task = make_task("1", "Fix bug");

    let effects = build_grouped_nudge_effects(&task, "york", &snap, "");

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::NudgeSessionWithCallbacks {
            session_id,
            on_success,
            ..
        } => {
            assert_eq!(session_id, "session-123");
            // Should have RecordTaskAssignment + post_to_ops
            assert!(on_success.len() >= 2);
        }
        other => panic!("Expected NudgeSessionWithCallbacks, got {:?}", other),
    }
}

#[test]
fn grouped_nudge_includes_workflow_event_when_channel_set() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.name_session_map
        .insert("york".to_string(), "session-123".to_string());
    let mut task = make_task("1", "Fix bug");
    task.channel = Some("ops".to_string());

    let effects = build_grouped_nudge_effects(&task, "york", &snap, "");

    match &effects[0] {
        Effect::NudgeSessionWithCallbacks { on_success, .. } => {
            // Should have RecordTaskAssignment + post_to_ops + EmitWorkflowEvent
            assert_eq!(on_success.len(), 3);
            assert!(matches!(
                &on_success[2],
                Effect::EmitWorkflowEvent(crate::workflow::WorkflowEvent::TaskAssigned { .. })
            ));
        }
        other => panic!("Expected NudgeSessionWithCallbacks, got {:?}", other),
    }
}

// ============================================================================
// build_reviewer_spawn_effects tests
// ============================================================================

#[test]
fn reviewer_spawn_returns_none_without_pr_number() {
    let snap = snapshot::minimal_snapshot_for_test();
    let task = make_task("1", "Review something");
    // task.pr is None

    assert!(build_reviewer_spawn_effects(&task, "york", &snap).is_none());
}

#[test]
fn reviewer_spawn_returns_none_on_worktree_collision() {
    let mut snap = snapshot::minimal_snapshot_for_test();
    let pr_number = 42u64;
    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
    // Register another coworker on this worktree
    snap.worktree_registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: worktree_id.clone(),
            branch_name: worktree_id.clone(),
            task_id: None,
            current_coworker: Some("other-coworker".to_string()),
            pr_number: Some(pr_number),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();
    snap.coworkers
        .active_names
        .insert("other-coworker".to_string());
    let task = make_reviewer_task("1", pr_number);

    assert!(build_reviewer_spawn_effects(&task, "york", &snap).is_none());
}

#[test]
fn reviewer_spawn_produces_worktree_and_spawn_effects() {
    let snap = snapshot::minimal_snapshot_for_test();
    let task = make_reviewer_task("1", 42);

    let effects = build_reviewer_spawn_effects(&task, "york", &snap).unwrap();

    assert_eq!(effects.len(), 2);
    assert!(matches!(&effects[0], Effect::EnsureWorktree { .. }));
    assert!(matches!(&effects[1], Effect::SpawnForTask { .. }));

    if let Effect::SpawnForTask {
        preferred_name,
        task_id,
        ..
    } = &effects[1]
    {
        assert_eq!(preferred_name.as_deref(), Some("york"));
        assert_eq!(task_id, "1");
    }
}
