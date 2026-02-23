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
        is_running,
        ..Default::default()
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

    let effects = dispatch_via_sessions_for_test(&snap, None, |_| None);

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

    let (state, _tmp, _guard) = make_test_state();
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

    let (state, _tmp, _guard) = make_test_state();
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

    let (state, _tmp, _guard) = make_test_state();

    // Run dispatch_via_sessions first (as events.rs does)
    let session_effects = dispatch_via_sessions_for_test(&snap, None, |_| None);
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

/// Regression test: recovery loop when stopped reviewer session is not marked running after spawn.
///
/// Bug: `spawn_coworker` used `or_insert_with` to update `persistent_state.sessions`, which
/// does NOT update an existing entry. A stopped session (is_running=false) remains stopped
/// after the resume spawn, causing `dispatch_via_sessions` to trigger recovery on every tick.
///
/// Captured snapshot: snapshot-session-dispatch-recovered-loop-task-1690-riverside-20260221-020931.json
/// Task !1690 had stopped reviewer session "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834" (riverside).
/// Each tick: dispatch found stopped session → spawned new coworker → session stayed stopped →
/// dispatch triggered again (loop indefinitely).
///
/// Fix: `spawn_coworker` must use `and_modify(|r| r.is_running = true)` before `or_insert_with`
/// so existing stopped session records are marked running immediately on resume. This ensures
/// the next snapshot sees `is_running=true` and skips recovery.
#[test]
fn test_session_dispatch_recovery_loop_stopped_reviewer_session() {
    // Exact session state from the captured loop snapshot:
    // Task !1690 owned by "riverside", stopped reviewer session "e2bafbb6".
    let session = SessionRecord {
        is_reviewer: true,
        coworker_type: "reviewer".to_string(),
        ..make_session_record(
            "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834",
            Some("1690"),
            Some("riverside"),
            false, // stopped
        )
    };

    // Snapshot as seen on every looping tick: stopped session, no cooldown.
    let snap_with_stopped_session = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1690".to_string(),
            "Review PR #1378".to_string(),
            "riverside".to_string(),
        )],
        sessions: [(
            "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string(),
            session.clone(),
        )]
        .into_iter()
        .collect(),
        session_task_map: [(
            "1690".to_string(),
            "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string(),
        )]
        .into_iter()
        .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    // Tick 1: recovery fires (expected — session is stopped).
    let effects_tick1 = dispatch_via_sessions_for_test(&snap_with_stopped_session, None, |_| None);
    assert!(
        effects_tick1
            .iter()
            .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. })),
        "First tick should trigger recovery for stopped session"
    );

    // After spawn, `spawn_coworker` should mark the session as `is_running = true`.
    // The next snapshot is built from `persistent_state.sessions`. If the fix is in place,
    // the session record will have `is_running = true`, and dispatch will skip recovery.
    //
    // Simulate the corrected post-spawn state (what spawn_coworker should produce after fix):
    let session_running = SessionRecord {
        is_running: true, // Fixed: spawn_coworker marks session running via and_modify
        ..session.clone()
    };
    let snap_after_spawn = snapshot::WorldSnapshot {
        in_progress_tasks: snap_with_stopped_session.in_progress_tasks.clone(),
        sessions: [(
            "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string(),
            session_running,
        )]
        .into_iter()
        .collect(),
        session_task_map: snap_with_stopped_session.session_task_map.clone(),
        ..snapshot::minimal_snapshot_for_test()
    };

    // Tick 2: recovery must NOT fire — session is now running.
    let effects_tick2 = dispatch_via_sessions_for_test(&snap_after_spawn, None, |_| None);
    assert!(
        !effects_tick2
            .iter()
            .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. })),
        "After spawn, session is running — dispatch must NOT trigger recovery again. Got: {:?}",
        effects_tick2
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

/// Helper to create minimal DaemonState for testing (duplicated from dispatch_tests.rs
/// because test module boundaries prevent sharing private helpers).
fn make_test_state() -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;
    use tempfile::TempDir;

    let midtown_dir = TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

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

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
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
    .expect("daemon state");
    (state, temp_dir, _guard)
}

#[test]
fn test_session_dispatch_skips_channel_lead_owned_tasks() {
    // Given: a channel lead has an in-progress task with a stopped session.
    // The session recovery loop must NOT resume it as a regular coworker.
    let session = SessionRecord {
        coworker_type: "channel-lead".to_string(),
        ..make_session_record("sess-cl-123", Some("99"), Some("canal-lead"), false)
    };

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "99".to_string(),
            "Maintain canal channel".to_string(),
            "canal-lead".to_string(),
        )],
        active_names: HashSet::new(), // canal-lead is NOT active
        sessions: [("sess-cl-123".to_string(), session)].into_iter().collect(),
        session_task_map: [("99".to_string(), "sess-cl-123".to_string())]
            .into_iter()
            .collect(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        // canal-lead is registered as a channel lead
        channel_lead_sessions: [("canal-lead".to_string(), "sess-cl-123".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions_for_test(&snap, None, |_| None);

    // Then: no spawn effects — channel leads must not be recovered as coworkers
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        !has_spawn,
        "Should NOT spawn a coworker for a channel lead's task, got effects: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_session_dispatch_skips_active_reviewer() {
    // Regression test for: daemon reassigning coworker mid-review to a task.
    //
    // Scenario: "columbus" was previously working on task !1675 (stopped session),
    // then was called in to review PR #1384 (new running reviewer session).
    // dispatch_via_sessions must NOT resume the stopped task session while
    // columbus is actively serving as a reviewer.
    let task_session = make_session_record("sess-task-1675", Some("1675"), Some("columbus"), false);

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1675".to_string(),
            "Fix some bug".to_string(),
            "columbus".to_string(),
        )],
        // columbus is active (running as reviewer in a separate session)
        active_names: ["columbus".to_string()].into_iter().collect(),
        active_reviewers: ["columbus".to_string()].into_iter().collect(),
        reviewer_pr_assignments: [("columbus".to_string(), 1384_u64)].into_iter().collect(),
        sessions: [("sess-task-1675".to_string(), task_session)]
            .into_iter()
            .collect(),
        session_task_map: [("1675".to_string(), "sess-task-1675".to_string())]
            .into_iter()
            .collect(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions_for_test(&snap, None, |_| None);

    // Then: no spawn effects — columbus is actively reviewing PR #1384
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
        "Should NOT spawn a coworker that is actively reviewing a PR, got: {:?}",
        spawn_effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

// ======================================================================
// Recovery loop prevention tests
// ======================================================================

/// Regression test for task !1693: stale is_running=false causes infinite recovery loop.
///
/// After a successful recovery spawn, `spawn_coworker` uses `or_insert_with` which
/// is a no-op for existing session records — leaving `is_running=false` in persistent
/// state. The next tick sees the session as stopped and spawns again, ad infinitum.
///
/// The fix: `dispatch_via_sessions` must also check `active_session_ids`. If the
/// session_id is in `active_session_ids`, the coworker is actually running (the
/// session_manager has a live process for it), so recovery must be skipped even
/// when `is_running` is stale.
#[test]
fn test_session_dispatch_skips_recovery_when_session_is_active_despite_stale_is_running() {
    // Given: task !1690 has session e2bafbb6 (is_running=false — stale persistent flag)
    // but the session is in active_session_ids (actually running after a prior recovery spawn).
    let session = make_session_record(
        "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834",
        Some("1690"),
        Some("riverside"),
        false, // is_running=false: stale flag not yet updated by spawn_coworker
    );

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1690".to_string(),
            "Rebase PR #1404, address review feedback, and merge".to_string(),
            "riverside".to_string(),
        )],
        active_names: ["riverside".to_string()].into_iter().collect(),
        // Session is live in active_session_ids (successful recovery spawn happened)
        active_session_ids: ["e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string()]
            .into_iter()
            .collect(),
        sessions: [("e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [(
            "1690".to_string(),
            "e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string(),
        )]
        .into_iter()
        .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions_for_test(&snap, None, |_| None);

    // Then: must NOT emit another recovery spawn — the session is already live.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::AssignAndSpawn { .. }
                    | Effect::ResumeCoworker { .. }
            )
        })
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Should NOT recover a session that is in active_session_ids (live process), \
         even when is_running=false is stale. Got effects: {:?}",
        spawn_effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

/// When a stopped session exists but the spawn will fail (e.g., worktree gone),
/// dispatch emits ClearSessionForTask in the on_failure effects to break the
/// retry loop. This test verifies the effect is present in the on_failure list.
#[test]
fn test_dispatch_via_sessions_emits_clear_session_on_failure() {
    let session = make_session_record("sess-stale-123", Some("42"), Some("lexington"), false);

    let task = in_progress_task_for_lookup("42", "Fix stale session bug", "lexington");

    let snap = snapshot::WorldSnapshot {
        sessions: [("sess-stale-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("42".to_string(), "sess-stale-123".to_string())]
            .into_iter()
            .collect(),
        in_progress_tasks: vec![(
            "42".to_string(),
            "Fix stale session bug".to_string(),
            "lexington".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let lookup = |id: &str| -> Option<crate::tasks::Task> {
        if id == "42" { Some(task.clone()) } else { None }
    };

    let effects = dispatch_via_sessions_with_task_lookup(&snap, None, lookup);

    // The outermost effect should be SpawnCoworkerWithCallbacks
    let spawn_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        spawn_effect.is_some(),
        "Expected SpawnCoworkerWithCallbacks effect"
    );

    if let Some(Effect::SpawnCoworkerWithCallbacks { on_failure, .. }) = spawn_effect {
        let has_clear = on_failure
            .iter()
            .any(|e| matches!(e, Effect::ClearSessionForTask { task_id } if task_id == "42"));
        assert!(
            has_clear,
            "on_failure should contain ClearSessionForTask for task 42, got: {:?}",
            on_failure
        );
    }
}

/// Regression test for task !1709: repeated session recovery log spam.
///
/// When a session recovery spawn succeeds but the session then dies quickly
/// (e.g., stale session_id, process exits within one tick window), the next tick
/// fires recovery again — causing "Session dispatch: recovered task !{}" to be
/// posted to the ops channel on every tick instead of once.
///
/// The fix: after a successful recovery spawn, record a per-session-id cooldown
/// ("session_recovered"). On the next tick, if the session_id is in
/// `recently_recovered_session_ids`, skip recovery even when `is_running=false`
/// and the session is not in `active_session_ids`.
#[test]
fn test_session_dispatch_skips_recovery_for_recently_recovered_session() {
    // Given: task !1703 has session "7659329f" (is_running=false — session died after recovery)
    // but the session was recently recovered (recovery cooldown is active).
    let session = make_session_record(
        "7659329f-dead-4ead-b00b-cafecafecafe",
        Some("1703"),
        None,  // current_name is None — session died, cleanup_coworker_state cleared it
        false, // is_running=false: session died after previous recovery spawn
    );

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1703".to_string(),
            "Fix session recovery spam".to_string(),
            "park".to_string(),
        )],
        // Session is stopped (not in active_session_ids or active_names)
        active_session_ids: HashSet::new(),
        active_names: HashSet::new(),
        sessions: [("7659329f-dead-4ead-b00b-cafecafecafe".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [(
            "1703".to_string(),
            "7659329f-dead-4ead-b00b-cafecafecafe".to_string(),
        )]
        .into_iter()
        .collect(),
        // Recovery was recently attempted — cooldown is active for this session_id
        recently_recovered_session_ids: ["7659329f-dead-4ead-b00b-cafecafecafe".to_string()]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions_for_test(&snap, None, |_| None);

    // Then: must NOT emit another recovery spawn — the session was recently recovered.
    // Without the fix, this fires on every tick causing log spam.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { .. }
                    | Effect::SpawnCoworker(_)
                    | Effect::AssignAndSpawn { .. }
                    | Effect::ResumeCoworker { .. }
            )
        })
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Should NOT re-recover a session that was recently recovered (per-session cooldown). \
         This fires on every tick causing log spam in task !1703. Got effects: {:?}",
        spawn_effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

// ======================================================================
// Pending task resume with cooldown tests
// ======================================================================

/// When a pending task has a stopped session AND the session is in
/// `recently_recovered_session_ids`, the pending task resume path
/// (in `spawn_for_pending_tasks_excluding`) must skip it. Without this,
/// a failed-resume session triggers an infinite spawn loop: each tick
/// sees the stopped session and re-attempts resume with no delay.
#[test]
fn test_pending_task_with_cooldown_active_skips_resume() {
    use crate::tasks::{Task, TaskStatus};

    let session = make_session_record("sess-loop-123", Some("77"), Some("park"), false);

    let snap = snapshot::WorldSnapshot {
        // Task appears as pending without owner (task was reset after failed resume)
        pending_tasks_without_owners: vec![Task {
            id: "77".to_string(),
            subject: "Fix auth bug".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        }],
        sessions: [("sess-loop-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("77".to_string(), "sess-loop-123".to_string())]
            .into_iter()
            .collect(),
        // Cooldown is active for this session
        recently_recovered_session_ids: ["sess-loop-123".to_string()].into_iter().collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects =
        spawn_for_pending_tasks_excluding(&snap, &state, &std::collections::HashSet::new());

    // The pending task resume path should skip sess-loop-123 due to cooldown.
    // It may fall through to the fresh-spawn path, but it must NOT emit
    // SpawnSession with resume=true for the cooldown-active session.
    let has_session_resume = effects.iter().any(|e| {
        matches!(e, Effect::SpawnSession { session_id, resume: true, .. } if session_id == "sess-loop-123")
    });
    assert!(
        !has_session_resume,
        "Should NOT resume session sess-loop-123 when cooldown is active, got effects: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

/// Positive case: when a pending task has a stopped session and NO cooldown,
/// the pending task resume path should emit a SpawnSession with resume=true.
#[test]
fn test_pending_task_stopped_session_resumes_when_no_cooldown() {
    use crate::tasks::{Task, TaskStatus};

    let session = make_session_record("sess-resume-456", Some("88"), Some("broadway"), false);

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "88".to_string(),
            subject: "Add logging feature".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        }],
        sessions: [("sess-resume-456".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("88".to_string(), "sess-resume-456".to_string())]
            .into_iter()
            .collect(),
        // No cooldown — recovery should proceed
        recently_recovered_session_ids: HashSet::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects =
        spawn_for_pending_tasks_excluding(&snap, &state, &std::collections::HashSet::new());

    // Should emit SpawnSession with resume=true for the stopped session
    let has_resume_spawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnSession { session_id, resume: true, .. } if session_id == "sess-resume-456")
    });
    assert!(
        has_resume_spawn,
        "Should emit SpawnSession(resume=true) for stopped session when no cooldown, got effects: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

/// Verify that on_success effects for session recovery include a per-session-id cooldown.
///
/// This ensures the `recently_recovered_session_ids` guard in the next tick is populated,
/// preventing the recovery loop described in task !1709.
#[test]
fn test_session_dispatch_on_success_includes_session_recovered_cooldown() {
    // Given: task !1703 has a stopped session with no recent recovery
    let session = make_session_record(
        "7659329f-dead-4ead-b00b-cafecafecafe",
        Some("1703"),
        Some("park"),
        false,
    );

    let task = in_progress_task_for_lookup("1703", "Fix session recovery spam", "park");

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1703".to_string(),
            "Fix session recovery spam".to_string(),
            "park".to_string(),
        )],
        sessions: [("7659329f-dead-4ead-b00b-cafecafecafe".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [(
            "1703".to_string(),
            "7659329f-dead-4ead-b00b-cafecafecafe".to_string(),
        )]
        .into_iter()
        .collect(),
        // No recent recovery — cooldown is not active
        recently_recovered_session_ids: HashSet::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let lookup = |id: &str| -> Option<crate::tasks::Task> {
        if id == "1703" {
            Some(task.clone())
        } else {
            None
        }
    };

    let effects = dispatch_via_sessions_with_task_lookup(&snap, None, lookup);

    // Find the SpawnCoworkerWithCallbacks effect
    let spawn_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        spawn_effect.is_some(),
        "Expected SpawnCoworkerWithCallbacks for stopped session"
    );

    if let Some(Effect::SpawnCoworkerWithCallbacks { on_success, .. }) = spawn_effect {
        let has_session_recovered_cooldown = on_success.iter().any(|e| {
            matches!(e, Effect::RecordCooldown { category, key }
                if category == "session_recovered" && key == "7659329f-dead-4ead-b00b-cafecafecafe")
        });
        assert!(
            has_session_recovered_cooldown,
            "on_success must contain RecordCooldown(session_recovered, session_id) \
             to prevent re-recovery spam on the next tick. Got: {:?}",
            on_success
        );
    }
}
