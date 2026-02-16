use super::*;
use crate::launch::{CoworkerRole, LaunchConfig, SessionMode};

fn make_spawn(name: &str) -> Effect {
    Effect::SpawnCoworker(LaunchConfig {
        name: name.to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    })
}

#[test]
fn dedup_removes_duplicate_spawn_for_same_coworker() {
    let effects = vec![
        make_spawn("lexington"),
        Effect::NudgeLead {
            message: "hello".to_string(),
        },
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
    // NudgeLead preserved
    assert_eq!(deduped.len(), 3);
}

#[test]
fn dedup_preserves_all_when_no_duplicates() {
    let effects = vec![
        make_spawn("lexington"),
        make_spawn("park"),
        Effect::NudgeLead {
            message: "hello".to_string(),
        },
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
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    };
    Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success: vec![],
        on_failure: vec![],
    }
}

fn make_assign_and_spawn(name: &str) -> Effect {
    let config = LaunchConfig {
        name: name.to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    };
    Effect::AssignAndSpawn {
        task_id: "1".to_string(),
        owner: name.to_string(),
        repo_name: "test".to_string(),
        config,
        on_success: vec![],
        on_failure: vec![],
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
    // AssignAndSpawn and SpawnCoworkerWithCallbacks for the same coworker
    // should deduplicate (first one wins).
    let effects = vec![
        make_assign_and_spawn("lexington"),
        make_spawn_with_callbacks("lexington"), // same coworker, different variant
        make_spawn("park"),
    ];

    let deduped = dedup_spawn_effects(effects);
    assert_eq!(deduped.len(), 2, "Should keep first lexington + park");
    // First effect should be the AssignAndSpawn (it came first)
    assert!(
        matches!(&deduped[0], Effect::AssignAndSpawn { config, .. } if config.name == "lexington"),
        "First effect should be AssignAndSpawn for lexington"
    );
}

#[test]
fn dedup_preserves_registry_effects_from_dropped_spawns() {
    // Issue #8 from PR #752 review: When two tasks are assigned to the same
    // coworker in one tick, the second AssignAndSpawn is dropped entirely,
    // losing its RegisterWorktreeAssignment effect.
    use crate::worktree_registry::WorktreeAssignment;

    let config1 = LaunchConfig {
        name: "lexington".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    };

    let config2 = config1.clone();

    // First spawn with task-123 worktree assignment
    let spawn1 = Effect::AssignAndSpawn {
        task_id: "123".to_string(),
        owner: "lexington".to_string(),
        repo_name: "test".to_string(),
        config: config1,
        on_success: vec![Effect::RegisterWorktreeAssignment {
            assignment: WorktreeAssignment {
                worktree_id: "task-123-foo".to_string(),
                branch_name: "task-123-foo".to_string(),
                task_id: Some("123".to_string()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            },
        }],
        on_failure: vec![],
    };

    // Second spawn with task-456 worktree assignment (different task, same coworker)
    let spawn2 = Effect::AssignAndSpawn {
        task_id: "456".to_string(),
        owner: "lexington".to_string(),
        repo_name: "test".to_string(),
        config: config2,
        on_success: vec![Effect::RegisterWorktreeAssignment {
            assignment: WorktreeAssignment {
                worktree_id: "task-456-bar".to_string(),
                branch_name: "task-456-bar".to_string(),
                task_id: Some("456".to_string()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            },
        }],
        on_failure: vec![],
    };

    let effects = vec![spawn1, spawn2];
    let deduped = dedup_spawn_effects(effects);

    // The spawn should be deduplicated (only one spawn for lexington)
    let spawn_count = deduped
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::AssignAndSpawn { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::SpawnCoworkerWithCallbacks { .. }
            )
        })
        .count();
    assert_eq!(spawn_count, 1, "Should have only one spawn for lexington");

    // BUT: Both RegisterWorktreeAssignment effects should be preserved
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
        "Both registry assignments should be preserved"
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
    // name, so both spawns go through.
    use crate::worktree_registry::WorktreeAssignment;

    let config_amsterdam = LaunchConfig {
        name: "amsterdam".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    };

    let config_york = LaunchConfig {
        name: "york".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
    };

    // Orphan recovery spawns amsterdam for task 123
    let orphan_spawn = Effect::SpawnCoworkerWithCallbacks {
        config: config_amsterdam,
        on_success: vec![
            Effect::RecordTaskAssignment {
                coworker: "amsterdam".to_string(),
                task_id: "123".to_string(),
            },
            Effect::RegisterWorktreeAssignment {
                assignment: WorktreeAssignment {
                    worktree_id: "task-123-foo".to_string(),
                    branch_name: "task-123-foo".to_string(),
                    task_id: Some("123".to_string()),
                    current_coworker: None,
                    pr_number: None,
                    created_at: chrono::Utc::now(),
                    completed_at: None,
                },
            },
        ],
        on_failure: vec![],
    };

    // Task dispatch spawns york for task 123 (same task, different coworker)
    let dispatch_spawn = Effect::AssignAndSpawn {
        task_id: "123".to_string(),
        owner: "york".to_string(),
        repo_name: "test".to_string(),
        config: config_york,
        on_success: vec![Effect::RegisterWorktreeAssignment {
            assignment: WorktreeAssignment {
                worktree_id: "task-123-bar".to_string(),
                branch_name: "task-123-bar".to_string(),
                task_id: Some("123".to_string()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            },
        }],
        on_failure: vec![],
    };

    let effects = vec![orphan_spawn, dispatch_spawn];
    let deduped = dedup_spawn_effects(effects);

    // EXPECTED: Only ONE spawn effect should remain (the first one)
    let spawn_count = deduped
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::AssignAndSpawn { .. }
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

    // Both worktree assignments should be preserved
    let registry_count = deduped
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();
    assert_eq!(
        registry_count, 2,
        "Both worktree registrations should be preserved"
    );

    // The task assignment from the kept spawn should be preserved (in its on_success callbacks)
    let task_assignments: Vec<&str> = deduped
        .iter()
        .flat_map(|e| match e {
            Effect::RecordTaskAssignment { task_id, .. } => vec![task_id.as_str()],
            Effect::SpawnCoworkerWithCallbacks { on_success, .. }
            | Effect::AssignAndSpawn { on_success, .. } => on_success
                .iter()
                .filter_map(|sub| {
                    if let Effect::RecordTaskAssignment { task_id, .. } = sub {
                        Some(task_id.as_str())
                    } else {
                        None
                    }
                })
                .collect(),
            _ => vec![],
        })
        .collect();
    assert_eq!(
        task_assignments.len(),
        1,
        "Should have exactly one task assignment for task 123 (from the kept spawn's callbacks)"
    );
    assert_eq!(task_assignments[0], "123");
}
