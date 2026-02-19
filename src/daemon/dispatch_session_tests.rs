//! Tests for session-centric dispatch migration.
//!
//! These tests verify that dispatch functions use session-centric lookups
//! alongside existing name-based paths. Session data comes from WorldSnapshot
//! fields: `sessions`, `session_task_map`, `session_name_map`, `name_session_map`.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::daemon::state::SessionRecord;

fn make_session_record(
    session_id: &str,
    task_id: Option<&str>,
    current_name: Option<&str>,
    is_running: bool,
) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        current_name: current_name.map(|s| s.to_string()),
        preferred_name: current_name.map(|s| s.to_string()),
        working_dir: "/tmp/test-worktree".to_string(),
        branch: Some("main".to_string()),
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running,
        created_at: chrono::Utc::now(),
        resume_on_startup: true,
    }
}

fn in_progress_task_for_lookup(task_id: &str, subject: &str, owner: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        id: task_id.to_string(),
        subject: subject.to_string(),
        status: crate::tasks::TaskStatus::InProgress,
        owner: Some(owner.to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }
}

// ======================================================================
// find_session_for_task tests
// ======================================================================

#[test]
fn test_find_session_for_task_returns_session() {
    // Given: a task with an associated session in the snapshot
    let session = make_session_record("sess-abc-123", Some("42"), Some("lexington"), false);

    let snap = snapshot::WorldSnapshot {
        sessions: [("sess-abc-123".to_string(), session.clone())]
            .into_iter()
            .collect(),
        session_task_map: [("42".to_string(), "sess-abc-123".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    // When: find_session_for_task is called
    let result = find_session_for_task("42", &snap);

    // Then: returns the SessionRecord
    assert!(result.is_some(), "Should find the session for task 42");
    let found = result.unwrap();
    assert_eq!(found.session_id, "sess-abc-123");
    assert_eq!(found.task_id.as_deref(), Some("42"));
}

#[test]
fn test_find_session_for_task_returns_none_when_no_session() {
    // Given: a task with no session record
    let snap = snapshot::WorldSnapshot {
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    // When: find_session_for_task is called
    let result = find_session_for_task("42", &snap);

    // Then: returns None
    assert!(
        result.is_none(),
        "Should return None when no session exists for task"
    );
}

// ======================================================================
// Orphan recovery with session awareness tests
// ======================================================================

#[test]
fn test_session_dispatch_uses_resume_session_for_stopped_session() {
    // Given: an in-progress task with a stopped session record (is_running=false).
    // dispatch_via_sessions (not orphan recovery) handles tasks with session records.
    let session = make_session_record("sess-abc-123", Some("42"), Some("lexington"), false);

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(), // lexington is NOT active
        sessions: [("sess-abc-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("42".to_string(), "sess-abc-123".to_string())]
            .into_iter()
            .collect(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions(&snap);

    // Then: emits SpawnCoworkerWithCallbacks with SessionMode::ResumeSession
    // (SpawnCoworkerWithCallbacks enables on_failure cooldown and task reset)
    let has_resume_spawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(id) if id == "sess-abc-123"))
    });
    assert!(
        has_resume_spawn,
        "Should emit SpawnCoworkerWithCallbacks with ResumeSession('sess-abc-123'), got effects: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );

    // Should NOT emit a bare ResumeCoworker (which lacks failure callbacks)
    let has_bare_resume = effects
        .iter()
        .any(|e| matches!(e, Effect::ResumeCoworker { .. }));
    assert!(
        !has_bare_resume,
        "Should NOT emit bare ResumeCoworker (no failure callbacks)"
    );
}

#[test]
fn test_orphan_recovery_skips_tasks_with_session_records() {
    // Given: an orphaned task with a session record.
    // Orphan recovery should skip it — dispatch_via_sessions handles these.
    let session = make_session_record("sess-abc-123", Some("42"), Some("lexington"), false);

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(), // lexington is NOT active
        sessions: [("sess-abc-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("42".to_string(), "sess-abc-123".to_string())]
            .into_iter()
            .collect(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let state = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "42" {
            Some(in_progress_task_for_lookup(
                "42",
                "Add auth endpoint",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Orphan recovery must produce no spawn effects for session-tracked tasks.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworker(_)
                    | Effect::SpawnCoworkerWithCallbacks { .. }
                    | Effect::AssignAndSpawn { .. }
                    | Effect::ResumeCoworker { .. }
            )
        })
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Orphan recovery should skip tasks with session records, got: {:?}",
        spawn_effects
    );
}

#[test]
fn test_orphan_recovery_falls_back_to_fresh_spawn_without_session() {
    // Given: an orphaned task with NO session record
    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(), // lexington is NOT active (orphaned)
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let state = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "42" {
            Some(in_progress_task_for_lookup(
                "42",
                "Add auth endpoint",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Then: emits the standard SpawnCoworkerWithCallbacks effect (backward compat)
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        has_spawn,
        "Should emit SpawnCoworkerWithCallbacks when no session record exists, got effects: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );

    // Should NOT emit ResumeCoworker
    let has_resume = effects
        .iter()
        .any(|e| matches!(e, Effect::ResumeCoworker { .. }));
    assert!(
        !has_resume,
        "Should NOT emit ResumeCoworker when no session record exists"
    );
}

// ======================================================================
// Dual-spawn prevention tests
// ======================================================================

/// Regression test: `dispatch_via_sessions` and `check_and_recover_orphans` must not
/// both claim the same task in a single tick, even when the task qualifies for both paths.
///
/// A task qualifies for both when it has a stopped session record (triggering
/// `dispatch_via_sessions`) AND its owner is absent from `active_names` (triggering
/// `check_and_recover_orphans`). The exclusion set built in `events.rs` prevents this,
/// but here we verify the combined effects don't contain two spawns for the same task.
#[test]
fn test_no_dual_spawn_for_stopped_session_task() {
    // Task with a stopped session record AND owner not in active_names
    let session = make_session_record("sess-dual-123", Some("99"), Some("lexington"), false);

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "99".to_string(),
            "Fix auth".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(), // lexington is absent — qualifies for orphan recovery
        sessions: [("sess-dual-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("99".to_string(), "sess-dual-123".to_string())]
            .into_iter()
            .collect(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let state = make_test_state();

    // Run dispatch_via_sessions first (as events.rs does)
    let session_effects = dispatch_via_sessions(&snap);
    let session_claimed_ids = extract_claimed_task_ids_from_effects(&session_effects);

    // Run check_and_recover_orphans with the same task
    let orphan_effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "99" {
            Some(in_progress_task_for_lookup("99", "Fix auth", "lexington"))
        } else {
            None
        }
    });

    // Combine all spawn effects
    let all_effects: Vec<_> = session_effects
        .iter()
        .chain(orphan_effects.iter())
        .collect();

    // Count spawns targeting task 99 (via RecordTaskAssignment in on_success)
    let task_99_spawns = all_effects
        .iter()
        .filter(|e| {
            matches!(e, Effect::SpawnCoworkerWithCallbacks { on_success, .. }
                if on_success.iter().any(|s| matches!(s, Effect::RecordTaskAssignment { task_id, .. } if task_id == "99")))
        })
        .count();

    assert_eq!(
        task_99_spawns, 1,
        "Should only spawn once for task 99 (either session dispatch or orphan recovery, not both). Got {} spawns.",
        task_99_spawns
    );

    // The session_claimed_ids should contain "99" from session dispatch,
    // which is how events.rs excludes it from pending dispatch. Verify it's populated.
    assert!(
        session_claimed_ids.contains("99"),
        "session_claimed_ids should contain task 99 so orphan recovery can be excluded"
    );
}

/// Helper to create minimal DaemonState for testing (duplicated from dispatch_tests.rs
/// because test module boundaries prevent sharing private helpers).
fn make_test_state() -> DaemonState {
    use std::process::Command;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    // Leak temp_dir so it survives the test
    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir.clone()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state")
}
