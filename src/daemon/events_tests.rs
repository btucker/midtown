use super::*;
use crate::launch::{LaunchConfig, SessionMode};

fn make_spawn(name: &str) -> Effect {
    Effect::SpawnCoworker(LaunchConfig {
        name: name.to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
    })
}

#[test]
fn dedup_removes_duplicate_spawn_for_same_coworker() {
    let effects = vec![
        make_spawn("lexington"),
        Effect::nudge_channel_lead("test-repo", "hello"),
        make_spawn("lexington"), // duplicate — should be removed
        make_spawn("park"),      // different coworker — should be kept
    ];

    let deduped = dedup_spawn_effects(effects);

    let spawn_names: Vec<&str> = deduped
        .iter()
        .filter_map(|e| {
            if let Effect::SpawnCoworker(config) = e {
                Some(config.name.as_str())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(spawn_names, vec!["lexington", "park"]);
    // NudgeChannelLead preserved
    assert_eq!(deduped.len(), 3);
}

#[test]
fn dedup_preserves_all_when_no_duplicates() {
    let effects = vec![
        make_spawn("lexington"),
        make_spawn("park"),
        Effect::nudge_channel_lead("test-repo", "hello"),
    ];

    let deduped = dedup_spawn_effects(effects);
    assert_eq!(deduped.len(), 3);
}

#[test]
fn dedup_is_case_insensitive() {
    let effects = vec![make_spawn("Lexington"), make_spawn("lexington")];

    let deduped = dedup_spawn_effects(effects);
    assert_eq!(deduped.len(), 1);
}

#[test]
fn dedup_empty_effects_returns_empty() {
    let deduped = dedup_spawn_effects(vec![]);
    assert!(deduped.is_empty());
}

fn make_spawn_with_callbacks(name: &str) -> Effect {
    let config = LaunchConfig {
        name: name.to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
    };
    Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success: vec![],
        on_failure: vec![],
    }
}

fn make_spawn_for_task(name: &str, task_id: &str) -> Effect {
    let config = LaunchConfig {
        name: String::new(), // name allocated at execution time
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
    };
    Effect::SpawnForTask {
        task_id: task_id.to_string(),
        dir_key: "test".to_string(),
        preferred_name: Some(name.to_string()),
        config: Box::new(config),
        worktree_id: format!("task-{}-slug", task_id),
        success_message: format!("spawned for task !{}", task_id),
        failure_message: format!("spawn failed for task !{}", task_id),
        cooldown_category: "task_dispatch".to_string(),
        extra_success_cooldowns: vec![],
        reviewer: None,
    }
}

#[test]
fn dedup_removes_duplicate_spawn_with_callbacks() {
    let effects = vec![
        make_spawn_with_callbacks("lexington"),
        make_spawn_with_callbacks("lexington"), // duplicate
        make_spawn_with_callbacks("park"),
    ];

    let deduped = dedup_spawn_effects(effects);
    assert_eq!(deduped.len(), 2, "Should keep one lexington + one park");
}

#[test]
fn dedup_across_spawn_variants() {
    // SpawnForTask and SpawnCoworkerWithCallbacks for the same coworker
    // should deduplicate (first one wins).
    let effects = vec![
        make_spawn_for_task("lexington", "1"),
        make_spawn_with_callbacks("lexington"), // same coworker, different variant
        make_spawn("park"),
    ];

    let deduped = dedup_spawn_effects(effects);
    assert_eq!(deduped.len(), 2, "Should keep first lexington + park");
    // First effect should be the SpawnForTask (it came first)
    assert!(
        matches!(&deduped[0], Effect::SpawnForTask { preferred_name, .. } if preferred_name.as_deref() == Some("lexington")),
        "First effect should be SpawnForTask for lexington"
    );
}

#[test]
fn dedup_preserves_registry_effects_from_dropped_spawns() {
    // Issue #8 from PR #752 review: When two tasks are assigned to the same
    // coworker in one tick, the second SpawnForTask is dropped entirely.
    // RegisterWorktreeAssignment is now a top-level effect emitted BEFORE
    // SpawnForTask (by build_spawn_effects / prepare_task_worktree), so it is
    // never lost — it lives outside the spawn and is not subject to dedup.
    use crate::worktree_registry::WorktreeAssignment;

    let make_register = |worktree_id: &str, task_id_str: &str| Effect::RegisterWorktreeAssignment {
        assignment: WorktreeAssignment {
            worktree_id: worktree_id.to_string(),
            branch_name: worktree_id.to_string(),
            task_id: Some(task_id_str.to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: chrono::Utc::now(),
            completed_at: None,
        },
    };

    // In the new design: RegisterWorktreeAssignment comes before SpawnForTask.
    let effects = vec![
        make_register("task-123-foo", "123"),
        make_spawn_for_task("lexington", "123"),
        make_register("task-456-bar", "456"),
        make_spawn_for_task("lexington", "456"), // duplicate coworker — spawn dropped
    ];

    let deduped = dedup_spawn_effects(effects);

    // The second spawn is deduplicated
    let spawn_count = deduped
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::SpawnCoworkerWithCallbacks { .. }
            )
        })
        .count();
    assert_eq!(spawn_count, 1, "Should have only one spawn for lexington");

    // Both RegisterWorktreeAssignment effects are preserved (they're top-level, not in spawn)
    let registry_assignments: Vec<&str> = deduped
        .iter()
        .filter_map(|e| {
            if let Effect::RegisterWorktreeAssignment { assignment } = e {
                Some(assignment.worktree_id.as_str())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        registry_assignments.len(),
        2,
        "Both registry assignments should be preserved (they are top-level effects)"
    );
    assert!(
        registry_assignments.contains(&"task-123-foo"),
        "First task's worktree should be registered"
    );
    assert!(
        registry_assignments.contains(&"task-456-bar"),
        "Second task's worktree should be registered"
    );
}

#[test]
fn dedup_prevents_double_spawn_for_same_task() {
    // Bug: Orphan recovery spawns "amsterdam" for task 123, then task dispatch
    // spawns "york" for the same task in the same tick. Dedup only checks coworker
    // name, so both spawns go through. Task ID dedup is the backstop.
    //
    // In the new design, RegisterWorktreeAssignment is a top-level effect before
    // SpawnForTask — it is always preserved and never needs extracting from spawns.

    // Orphan recovery: RegisterWorktreeAssignment + SpawnForTask
    let orphan_spawn = make_spawn_for_task("amsterdam", "123");

    // Task dispatch: same task ID, different coworker
    let dispatch_spawn = make_spawn_for_task("york", "123");

    let effects = vec![orphan_spawn, dispatch_spawn];
    let deduped = dedup_spawn_effects(effects);

    // EXPECTED: Only ONE spawn effect should remain (the first one)
    let spawn_count = deduped
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::SpawnCoworkerWithCallbacks { .. }
            )
        })
        .count();
    assert_eq!(
        spawn_count, 1,
        "Should have only one spawn for task 123 (got {} spawns)",
        spawn_count
    );

    // Verify the kept spawn is for amsterdam (first one wins)
    assert!(
        matches!(&deduped[0], Effect::SpawnForTask { preferred_name, .. } if preferred_name.as_deref() == Some("amsterdam")),
        "First spawn should be kept (amsterdam)"
    );
}

#[test]
fn dedup_prevents_double_spawn_for_same_task_across_variants() {
    // SpawnForTask and SpawnCoworkerWithCallbacks (with task assignment callback)
    // for the same task should deduplicate by task ID.
    let spawn_for_task = make_spawn_for_task("amsterdam", "123");

    let config_york = LaunchConfig {
        name: "york".to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
    };
    let spawn_with_callbacks = Effect::SpawnCoworkerWithCallbacks {
        config: config_york,
        on_success: vec![Effect::RecordTaskAssignment {
            coworker: "york".to_string(),
            task_id: "123".to_string(),
        }],
        on_failure: vec![],
    };

    let effects = vec![spawn_for_task, spawn_with_callbacks];
    let deduped = dedup_spawn_effects(effects);

    let spawn_count = deduped
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::SpawnCoworkerWithCallbacks { .. }
            )
        })
        .count();
    assert_eq!(
        spawn_count, 1,
        "Should have only one spawn effect for task 123 across variants"
    );
    assert!(
        matches!(&deduped[0], Effect::SpawnForTask { .. }),
        "First spawn effect should be kept"
    );
}
