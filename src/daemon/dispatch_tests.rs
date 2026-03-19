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
        &snapshot::PrTaskIndex::default(),
        &HashSet::new(),
    ));
}

#[test]
fn task_with_merged_pr_via_session_is_protected() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let merged = [42u64].into_iter().collect();
    let pr_index = snapshot::PrTaskIndex::from_task_maps(
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
        &snapshot::PrTaskIndex::default(),
        &active_names_for(&task),
    ));
}

#[test]
fn task_with_no_active_owner_not_protected_by_open_pr() {
    let task = make_task("1", "Fix bug", "park", TaskStatus::InProgress);
    let pr_index = snapshot::PrTaskIndex::from_task_maps(
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
    let pr_index = snapshot::PrTaskIndex::from_task_maps(
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
    let effects = check_and_recover_orphans_impl(&ps, &tasks);
    assert!(effects.is_empty());
}

#[test]
fn orphan_recovery_skips_pr_protected_tasks() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.tick_pr_protected_tasks = ["1".to_string()].into_iter().collect();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks);
    assert!(effects.is_empty());
}

#[test]
fn orphan_recovery_spawns_for_dead_owner() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    // park is NOT active — orphaned
    ps.tick_active_session_names = HashSet::new();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = check_and_recover_orphans_impl(&ps, &tasks);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnForTask { .. })),
        "Should spawn to recover orphaned task"
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
    let effects = check_and_recover_orphans_impl(&ps, &tasks);
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
    ps.tick_pr_task_index = snapshot::PrTaskIndex::from_task_maps(
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
// Session recovery decision
// ============================================================================

#[test]
fn session_recovery_skips_lead() {
    let ps = make_ps("test-repo");

    let action = decide_session_recovery("1", "Fix", "test-repo", &ps);
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

    let action = decide_session_recovery("1", "Fix", "lead", &ps);
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

    let action = decide_session_recovery("1", "Fix", "park", &ps);
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

    let action = decide_session_recovery("1", "Fix", "park", &ps);
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

    let action = decide_session_recovery("1", "Fix", "park", &ps);
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
