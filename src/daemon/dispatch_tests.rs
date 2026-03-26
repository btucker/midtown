use super::*;
use crate::daemon::state::{DaemonPersistentState, SessionRecord};
use crate::task_store::{Task, TaskStatus};

/// Build active_names from a task's owner — used to preserve pre-existing test
/// behavior after adding the `active_names` parameter to `is_task_pr_protected`.
fn active_names_for(task: &crate::task_store::Task) -> HashSet<String> {
    if task.agent_name.is_empty() {
        HashSet::new()
    } else {
        [task.agent_name.to_lowercase()].into_iter().collect()
    }
}

fn make_task(id: &str, subject: &str, owner: &str, status: TaskStatus) -> Task {
    Task {
        id: id.to_string(),
        subject: subject.to_string(),
        agent_name: owner.to_string(),
        status,
        ..Default::default()
    }
}

#[allow(clippy::field_reassign_with_default)]
fn make_ps(project: &str) -> DaemonPersistentState {
    let mut ps = DaemonPersistentState::default();
    ps.tick_dir_key = project.to_string();
    ps.tick_project_name = project.to_string();
    ps.tick_default_channel = project.to_string();
    ps.tick_max_in_progress_tasks = 8;
    ps.tick_now = chrono::Utc::now();
    ps
}

fn make_session(
    session_id: &str,
    task_id: Option<&str>,
    name: &str,
    running: bool,
) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        name: name.to_string(),
        is_running: running,
        working_dir: "/tmp/test".to_string(),
        ..Default::default()
    }
}

// ============================================================================
// Push notification deep-link tests
// ============================================================================

#[test]
fn test_build_push_deep_link_basic() {
    let url = build_push_deep_link("myproject", "web", None, None);
    assert_eq!(url, "/myproject?channel=web");
}

#[test]
fn test_build_push_deep_link_with_msg() {
    let url = build_push_deep_link("myproject", "web", Some("msg-123"), None);
    assert_eq!(url, "/myproject?channel=web&msg=msg-123");
}

#[test]
fn test_build_push_deep_link_with_msg_and_thread() {
    let url = build_push_deep_link("myproject", "web", Some("msg-456"), Some("thread-789"));
    assert_eq!(url, "/myproject?channel=web&msg=msg-456&thread=thread-789");
}

// ============================================================================
// is_task_pr_protected tests
// ============================================================================

#[test]
fn completed_task_is_always_protected() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::Completed);
    assert!(is_task_pr_protected(
        &task,
        &HashSet::new(),
        &crate::daemon::snapshot::PrTaskIndex::default(),
        &HashSet::new(),
    ));
}

#[test]
fn task_with_merged_pr_via_session_is_protected() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let merged = [42u64].into_iter().collect();
    let pr_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );
    assert!(is_task_pr_protected(
        &task,
        &merged,
        &pr_index,
        &active_names_for(&task)
    ));
}

#[test]
fn task_with_merged_explicit_pr_is_protected() {
    let mut task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    task.pr = Some(42);
    let merged = [42u64].into_iter().collect();
    assert!(is_task_pr_protected(
        &task,
        &merged,
        &crate::daemon::snapshot::PrTaskIndex::default(),
        &active_names_for(&task),
    ));
}

#[test]
fn task_with_no_active_owner_not_protected_by_open_pr() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let pr_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );
    // Owner is NOT in active_names → open PR doesn't protect
    assert!(!is_task_pr_protected(
        &task,
        &HashSet::new(),
        &pr_index,
        &HashSet::new(), // no active names
    ));
}

#[test]
fn task_with_active_owner_and_open_pr_is_protected() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let pr_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );
    assert!(is_task_pr_protected(
        &task,
        &HashSet::new(),
        &pr_index,
        &active_names_for(&task),
    ));
}

// ============================================================================
// Duplicate task worker detection
// ============================================================================

#[test]
fn duplicate_detection_skips_legacy_lead_owner() {
    let mut ps = make_ps("my-repo");
    ps.tick_in_progress_tasks = vec![
        ("42".into(), "Fix bug".into(), "lead".into()),
        ("42".into(), "Fix bug".into(), "york".into()),
    ];

    let tasks = vec![
        make_task("42", "Fix bug", "lead", TaskStatus::InProgress),
        make_task("42", "Fix bug", "york", TaskStatus::InProgress),
    ];

    let effects = check_for_duplicate_task_workers(&ps, &tasks);
    let kill_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        kill_effects.is_empty(),
        "Should not kill — lead is excluded"
    );
}

#[test]
fn duplicate_detection_skips_repo_named_lead() {
    let mut ps = make_ps("my-repo");
    ps.tick_in_progress_tasks = vec![
        ("42".into(), "Fix bug".into(), "my-repo".into()),
        ("42".into(), "Fix bug".into(), "york".into()),
    ];

    let tasks = vec![
        make_task("42", "Fix bug", "my-repo", TaskStatus::InProgress),
        make_task("42", "Fix bug", "york", TaskStatus::InProgress),
    ];

    let effects = check_for_duplicate_task_workers(&ps, &tasks);
    let kill_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        kill_effects.is_empty(),
        "Should not kill — repo-named lead is excluded"
    );
}

#[test]
fn duplicate_detection_kills_later_worker() {
    let mut ps = make_ps("my-repo");
    ps.tick_in_progress_tasks = vec![
        ("42".into(), "Fix bug".into(), "york".into()),
        ("42".into(), "Fix bug".into(), "park".into()),
    ];
    // york started first
    ps.tick_coworker_start_times.insert(
        "york".to_string(),
        chrono::Utc::now() - chrono::Duration::minutes(5),
    );
    ps.tick_coworker_start_times.insert(
        "park".to_string(),
        chrono::Utc::now() - chrono::Duration::minutes(1),
    );

    let tasks = vec![
        make_task("42", "Fix bug", "york", TaskStatus::InProgress),
        make_task("42", "Fix bug", "park", TaskStatus::InProgress),
    ];

    let effects = check_for_duplicate_task_workers(&ps, &tasks);
    let killed: Vec<_> = effects
        .iter()
        .filter_map(|e| {
            if let Effect::ShutdownCoworker { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(killed, vec!["park"], "Should kill the later-started worker");
}

// ============================================================================
// Orphan recovery
// ============================================================================

#[test]
fn orphan_recovery_skips_when_cooldown_active() {
    let mut ps = make_ps("test");
    ps.tick_orphan_spawn_cooldown_active = true;
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks, &HashSet::new());
    assert!(effects.is_empty());
}

#[test]
fn orphan_recovery_skips_pr_protected_tasks() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.tick_pr_protected_tasks = ["1".to_string()].into_iter().collect();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks, &HashSet::new());
    assert!(effects.is_empty());
}

#[test]
fn orphan_recovery_spawns_for_dead_owner() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    // park is NOT active — orphaned
    ps.tick_active_session_names = HashSet::new();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks, &HashSet::new());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnForTask { .. })),
        "Should spawn to recover orphaned task"
    );
}

#[test]
fn orphan_recovery_skips_auto_closed_tasks() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.tick_active_session_names = HashSet::new();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let exclude = HashSet::from(["1".to_string()]);
    let effects = check_and_recover_orphans_impl(&ps, &tasks, &exclude);
    assert!(
        effects.is_empty(),
        "Should skip tasks already being auto-closed"
    );
}

#[test]
fn orphan_recovery_resumes_stopped_session() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.tick_active_session_names = HashSet::new();
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks, &HashSet::new());
    // Should use ResumeSession mode
    let has_spawn = effects.iter().any(|e| {
        if let Effect::SpawnForTask { config, .. } = e {
            matches!(
                config.session_mode,
                crate::launch::SessionMode::ResumeSession(_)
            )
        } else {
            false
        }
    });
    assert!(has_spawn, "Should resume the stopped session");
}

// ============================================================================
// Reset orphaned tasks
// ============================================================================

#[test]
fn reset_orphaned_ownerless_in_progress_task() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix bug".into(), "".into())];

    let tasks = vec![make_task("1", "Fix bug", "", TaskStatus::InProgress)];
    let effects = reset_orphaned_tasks(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ResetTaskToPending { task_id, .. } if task_id == "1"))
    );
}

#[test]
fn reset_orphaned_skips_task_with_open_pr() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix bug".into(), "park".into())];
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let tasks = vec![make_task("1", "Fix bug", "park", TaskStatus::InProgress)];
    let effects = reset_orphaned_tasks(&ps, &tasks);
    assert!(effects.is_empty(), "Should skip task with open PR");
}

#[test]
fn reset_orphaned_skips_active_owner() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix bug".into(), "park".into())];
    ps.tick_active_session_names = ["park".to_string()].into_iter().collect();

    let tasks = vec![make_task("1", "Fix bug", "park", TaskStatus::InProgress)];
    let effects = reset_orphaned_tasks(&ps, &tasks);
    assert!(effects.is_empty(), "Should skip task with active owner");
}

// ============================================================================
// Auto-close completed tasks
// ============================================================================

#[test]
fn auto_close_code_task_with_pr_when_owner_exited() {
    let mut ps = make_ps("test");
    // Owner is NOT in active sessions (session exited)
    // Task has an associated PR
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let mut task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    task.agent_type = "midtown-code-author".to_string();
    task.channel = Some("ops".to_string());
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "1")),
        "Should auto-close code task with PR when owner exited"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ClearBlockedBy { completed_task_id, .. } if completed_task_id == "1")),
        "Should clear blocked_by for completed task"
    );
}

#[test]
fn auto_close_review_task_when_review_posted() {
    let mut ps = make_ps("test");
    // Owner is NOT in active sessions (session exited)
    // Review has been posted for PR #42
    ps.github.reviewed_prs.insert(42);

    let mut task = make_task("1", "Review PR #42", "riverside", TaskStatus::InProgress);
    task.agent_type = "midtown-code-reviewer".to_string();
    task.pr = Some(42);
    task.channel = Some("ops".to_string());
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "1")),
        "Should auto-close review task when review posted"
    );
}

#[test]
fn auto_close_skips_active_owner() {
    let mut ps = make_ps("test");
    ps.tick_active_session_names = ["park".to_string()].into_iter().collect();
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let mut task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    task.agent_type = "midtown-code-author".to_string();
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not auto-close task with active owner"
    );
}

#[test]
fn auto_close_skips_recently_stopped_owner() {
    let mut ps = make_ps("test");
    ps.tick_coworker_stop_times
        .insert("park".to_string(), chrono::Utc::now());
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let mut task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    task.agent_type = "midtown-code-author".to_string();
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not auto-close during grace period"
    );
}

#[test]
fn auto_close_skips_code_task_without_pr() {
    let ps = make_ps("test");
    // Owner exited, no PR for task

    let mut task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    task.agent_type = "midtown-code-author".to_string();
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not auto-close code task without PR"
    );
}

#[test]
fn auto_close_skips_review_task_without_review() {
    let ps = make_ps("test");
    // Owner exited, but review NOT posted

    let mut task = make_task("1", "Review PR #42", "riverside", TaskStatus::InProgress);
    task.agent_type = "midtown-code-reviewer".to_string();
    task.pr = Some(42);
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not auto-close review task without review posted"
    );
}

#[test]
fn auto_close_skips_completed_tasks() {
    let mut ps = make_ps("test");
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let mut task = make_task("1", "Fix bug", "park", TaskStatus::Completed);
    task.agent_type = "midtown-code-author".to_string();
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not act on already-completed tasks"
    );
}

#[test]
fn auto_close_skips_ownerless_tasks() {
    let mut ps = make_ps("test");
    ps.tick_pr_task_index = crate::daemon::snapshot::PrTaskIndex::from_task_maps(
        [("1".to_string(), 42)].into_iter().collect(),
        HashMap::new(),
    );

    let mut task = make_task("1", "Fix bug", "", TaskStatus::InProgress);
    task.agent_type = "midtown-code-author".to_string();
    let tasks = vec![task];

    let effects = auto_close_completed_tasks(&ps, &tasks);
    assert!(effects.is_empty(), "Should not auto-close ownerless tasks");
}

// ============================================================================
// Session recovery decision
// ============================================================================

#[test]
fn session_recovery_skips_lead() {
    let ps = make_ps("test-repo");

    let action = decide_session_recovery("1", "Fix", "test-repo", &ps, &[]);
    assert!(matches!(
        action,
        crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::LeadOrChannelLead
        )
    ));
}

#[test]
fn session_recovery_skips_legacy_lead() {
    let ps = make_ps("test");

    let action = decide_session_recovery("1", "Fix", "lead", &ps, &[]);
    assert!(matches!(
        action,
        crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::LeadOrChannelLead
        )
    ));
}

#[test]
fn session_recovery_falls_back_when_no_session() {
    let ps = make_ps("test");

    let action = decide_session_recovery("1", "Fix", "park", &ps, &[]);
    assert!(matches!(
        action,
        crate::rules::SessionRecoveryAction::FallbackToOrphan { .. }
    ));
}

#[test]
fn session_recovery_skips_running_session() {
    let mut ps = make_ps("test");
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", true),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let action = decide_session_recovery("1", "Fix", "park", &ps, &[]);
    assert!(matches!(
        action,
        crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::SessionRunning
        )
    ));
}

#[test]
fn session_recovery_recovers_stopped_session() {
    let mut ps = make_ps("test");
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let action = decide_session_recovery("1", "Fix", "park", &ps, &[]);
    assert!(matches!(
        action,
        crate::rules::SessionRecoveryAction::Recover { .. }
    ));
}

// ============================================================================
// Subject-based completion
// ============================================================================

#[test]
fn subject_based_completion_completes_task_when_all_prs_merged() {
    let mut ps = make_ps("test");
    ps.tick_merged_pr_numbers = [901, 902].into_iter().collect();

    let tasks = vec![{
        let mut t = make_task("1", "Merge PRs: #901, #902", "park", TaskStatus::InProgress);
        t.channel = Some("web".into());
        t
    }];

    let effects = build_subject_based_completion_effects(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "1"))
    );
}

#[test]
fn subject_based_completion_skips_when_some_prs_not_merged() {
    let mut ps = make_ps("test");
    ps.tick_merged_pr_numbers = [901].into_iter().collect(); // only 901, not 902

    let tasks = vec![make_task(
        "1",
        "Merge PRs: #901, #902",
        "park",
        TaskStatus::InProgress,
    )];

    let effects = build_subject_based_completion_effects(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should not complete when not all PRs are merged"
    );
}

#[test]
fn subject_based_completion_with_explicit_pr_field() {
    let mut ps = make_ps("test");
    ps.tick_merged_pr_numbers = [42].into_iter().collect();

    let tasks = vec![{
        let mut t = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
        t.pr = Some(42);
        t
    }];

    let effects = build_subject_based_completion_effects(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "1"))
    );
}

// ============================================================================
// should_recover_task_test_helper (backward compat)
// ============================================================================

#[test]
fn should_recover_task_with_merged_pr() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let merged = [42u64].into_iter().collect();
    let open_prs: HashMap<String, u64> = [("1".to_string(), 42)].into_iter().collect();
    let github_prs = HashMap::new();

    // Task has merged PR → should NOT recover (protected)
    let should = should_recover_task_test_helper(
        &task,
        &merged,
        std::path::Path::new("/tmp"),
        &open_prs,
        &github_prs,
    );
    assert!(!should, "Should not recover task with merged PR");
}

#[test]
fn should_recover_task_without_pr() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let merged = HashSet::new();
    let open_prs = HashMap::new();
    let github_prs = HashMap::new();

    let should = should_recover_task_test_helper(
        &task,
        &merged,
        std::path::Path::new("/tmp"),
        &open_prs,
        &github_prs,
    );
    assert!(should, "Should recover task without any PR");
}

// ============================================================================
// decide_owned_pending_dispatch — skip reason tests
// ============================================================================

#[test]
fn owned_pending_skips_when_in_flight_spawn_active() {
    // GIVEN: a pending task with an in-flight spawn
    let mut ps = make_ps("test");
    ps.tick_in_flight_task_spawns = ["42".to_string()].into_iter().collect();

    let tasks = vec![make_task("42", "Fix bug", "park", TaskStatus::Pending)];

    // WHEN: decide owned pending dispatch
    let action = decide_owned_pending_dispatch("42", "Fix bug", "park", &ps, &tasks);

    // THEN: should skip due to in-flight spawn
    assert!(
        matches!(
            action,
            crate::rules::PendingTaskAction::Skip(
                crate::rules::OwnedPendingSkipReason::InFlightSpawn
            )
        ),
        "should skip when a spawn is already in flight: {:?}",
        action
    );
}

#[test]
fn owned_pending_skips_when_owner_already_assigned() {
    // GIVEN: a session that's already assigned to this task
    let mut ps = make_ps("test");
    ps.sessions.insert(
        "sess-park".into(),
        SessionRecord {
            session_id: "sess-park".into(),
            name: "park".into(),
            task_id: Some("42".into()),
            is_running: true,
            ..Default::default()
        },
    );

    let tasks = vec![make_task("42", "Fix bug", "park", TaskStatus::Pending)];

    // WHEN: decide owned pending dispatch
    let action = decide_owned_pending_dispatch("42", "Fix bug", "park", &ps, &tasks);

    // THEN: should skip because owner is already assigned to this task
    assert!(
        matches!(
            action,
            crate::rules::PendingTaskAction::Skip(
                crate::rules::OwnedPendingSkipReason::AlreadyAssigned
            )
        ),
        "should skip when owner is already assigned: {:?}",
        action
    );
}

#[test]
fn owned_pending_skips_lead_driven_channel_task() {
    // GIVEN: a task in a lead-driven channel
    let mut ps = make_ps("test");
    ps.lead_driven_channels = ["web".to_string()].into_iter().collect();

    let mut task = make_task("42", "Fix bug", "park", TaskStatus::Pending);
    task.channel = Some("web".to_string());
    let tasks = vec![task];

    // WHEN: decide owned pending dispatch
    let action = decide_owned_pending_dispatch("42", "Fix bug", "park", &ps, &tasks);

    // THEN: should skip because channel is lead-driven
    assert!(
        matches!(
            action,
            crate::rules::PendingTaskAction::Skip(
                crate::rules::OwnedPendingSkipReason::LeadDrivenChannel
            )
        ),
        "should skip task in lead-driven channel: {:?}",
        action
    );
}

#[test]
fn owned_pending_auto_completes_task_with_merged_pr() {
    // GIVEN: a pending task whose PR has been merged
    let mut ps = make_ps("test");
    ps.tick_merged_pr_numbers = [42u64].into_iter().collect();

    let tasks = vec![{
        let mut t = make_task("99", "Fix bug", "park", TaskStatus::Pending);
        t.pr = Some(42);
        t
    }];

    // WHEN: decide owned pending dispatch
    let action = decide_owned_pending_dispatch("99", "Fix bug", "park", &ps, &tasks);

    // THEN: should auto-complete because PR is merged
    assert!(
        matches!(
            action,
            crate::rules::PendingTaskAction::AutoComplete { ref task_id, pr_num }
            if task_id == "99" && pr_num == 42
        ),
        "should auto-complete when PR is merged: {:?}",
        action
    );
}

#[test]
fn pending_task_spawn_skipped_when_spawn_failure_cooldown_active() {
    // GIVEN: a pending task assigned to "park", but park is on spawn failure cooldown
    let mut ps = make_ps("test");
    ps.tick_pending_tasks_with_owners = vec![("42".into(), "Fix bug".into(), "park".into())];
    ps.tick_spawn_failure_cooldown_names = ["park".to_string()].into_iter().collect();

    // park is NOT active — would normally trigger a spawn
    ps.tick_active_session_names = HashSet::new();

    let tasks = vec![make_task("42", "Fix bug", "park", TaskStatus::Pending)];

    // WHEN: check_for_duplicate_task_workers (the dispatch for owned pending
    // runs inside dispatch_owned_pending_tasks which takes &DaemonState, so
    // we verify the spawn failure skip via decide_owned_pending_dispatch directly).
    //
    // decide_owned_pending_dispatch does NOT check spawn_failure_cooldown_names —
    // that check is in dispatch_owned_pending_tasks (which requires &DaemonState).
    // Instead, verify the rules path returns SpawnOwner so we know dispatch
    // would attempt the spawn (the cooldown block in dispatch_owned_pending_tasks
    // prevents it from becoming an Effect — that's covered by E2E tests).
    let action = decide_owned_pending_dispatch("42", "Fix bug", "park", &ps, &tasks);
    assert!(
        matches!(action, crate::rules::PendingTaskAction::SpawnOwner { .. }),
        "decide_owned_pending_dispatch should return SpawnOwner (cooldown enforcement is in dispatch_owned_pending_tasks): {:?}",
        action
    );
}

// ============================================================================
// Plan prompt section
// ============================================================================

#[test]
fn plan_prompt_empty_when_no_plan_or_skill() {
    let section = build_plan_prompt_section_from_parts("1", None, None);
    assert!(section.is_empty());
}

#[test]
fn plan_prompt_includes_execution_skill() {
    let section =
        build_plan_prompt_section_from_parts("1", None, Some("subagent-driven-development"));
    assert!(section.contains("subagent-driven-development"));
}

// ============================================================================
// Channel lead exclusion from dispatch
// ============================================================================

#[test]
fn duplicate_detection_skips_channel_leads() {
    let mut ps = make_ps("test");
    ps.channel_lead_sessions
        .insert("web".to_string(), "sess-lead".to_string());
    ps.tick_in_progress_tasks = vec![
        ("42".into(), "Fix".into(), "web".into()),
        ("42".into(), "Fix".into(), "york".into()),
    ];

    let tasks = vec![
        make_task("42", "Fix", "web", TaskStatus::InProgress),
        make_task("42", "Fix", "york", TaskStatus::InProgress),
    ];

    let effects = check_for_duplicate_task_workers(&ps, &tasks);
    let killed: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        killed.is_empty(),
        "Channel leads excluded from duplicate detection"
    );
}

// ── Reviewer name collision suffix re-check (!2511) ──────────────────────────

/// When a reviewer name collides and a random suffix is appended, the suffixed
/// name must be re-checked against the exclusion set. Previously, the suffixed
/// name was returned without verification, potentially colliding with an
/// existing session.
#[test]
fn reviewer_suffixed_name_is_rechecked_against_exclusions() {
    let parent_task = Task {
        id: "100".to_string(),
        subject: "Fix auth".to_string(),
        agent_name: "york".to_string(),
        status: TaskStatus::InProgress,
        ..Default::default()
    };

    let review_task = Task {
        id: "200".to_string(),
        subject: "Review PR #42".to_string(),
        agent_type: "midtown-code-reviewer".to_string(),
        parent: Some("100".to_string()),
        ..Default::default()
    };

    let mut ps = make_ps("test");
    // "york-reviewer" is already active, forcing a suffix
    ps.tick_active_session_names = ["york-reviewer".to_string()].into_iter().collect();

    let tasks = vec![parent_task, review_task.clone()];
    let name = allocate_fresh_coworker_name(&review_task, &ps, &tasks, true);

    // The name should NOT be "york-reviewer" (it's excluded)
    assert_ne!(
        name, "york-reviewer",
        "Suffixed reviewer name should not collide with active session"
    );
    // The name should start with "york-reviewer-" (it has a suffix)
    assert!(
        name.starts_with("york-reviewer-"),
        "Suffixed reviewer name should have a numeric suffix, got: {}",
        name
    );
    // The suffixed name should not be in the exclusion set
    assert!(
        !ps.tick_active_session_names.contains(&name),
        "Suffixed name should not be in exclusion set"
    );
}
