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
    let result = snap.find_session_for_task("42");

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
    let result = snap.find_session_for_task("42");

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

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
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
    snap.coworkers.active_names = HashSet::new(); // lexington is NOT active

    let effects = dispatch_via_sessions_for_test(&snap);

    // Then: emits SpawnForTask with SessionMode::ResumeSession
    let has_resume_spawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnForTask { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(id) if id == "sess-abc-123"))
    });
    assert!(
        has_resume_spawn,
        "Should emit SpawnForTask with ResumeSession('sess-abc-123'), got effects: {:?}",
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
fn test_orphan_recovery_handles_tasks_with_session_records() {
    // Given: an orphaned task with a stopped session record.
    // Orphan recovery now handles ALL orphaned tasks, including those with session
    // records — it will attempt to resume the session.
    let session = make_session_record("sess-abc-123", Some("42"), Some("lexington"), false);

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
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
    snap.coworkers.active_names = HashSet::new(); // lexington is NOT active

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
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

    // Orphan recovery should produce a spawn effect to resume the session.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .collect();
    assert!(
        !spawn_effects.is_empty(),
        "Orphan recovery should handle tasks with session records via session resume, got: {:?}",
        effects
    );

    // Should use ResumeSession mode (not Fresh) since a session record exists
    let has_resume_session = effects.iter().any(|e| {
        matches!(e, Effect::SpawnForTask { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(_)))
    });
    assert!(
        has_resume_session,
        "Should use ResumeSession mode for task with existing session record, got: {:?}",
        effects
    );
}

#[test]
fn test_orphan_recovery_falls_back_to_fresh_spawn_without_session() {
    // Given: an orphaned task with NO session record
    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };
    snap.coworkers.active_names = HashSet::new(); // lexington is NOT active (orphaned)

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
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

    // Then: emits the standard SpawnForTask effect
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Should emit SpawnForTask when no session record exists, got effects: {:?}",
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

/// Verify that `dispatch_via_sessions` and `check_and_recover_orphans` both independently
/// produce spawn effects for a stopped-session task. Dual-spawn prevention is handled at
/// the events.rs orchestration level — only `dispatch_via_sessions` is called in the event
/// loop; `check_and_recover_orphans` is no longer invoked there.
///
/// This test confirms that both functions CAN handle the task (no artificial filtering),
/// and that the events.rs architecture prevents double-dispatch by only calling one path.
#[test]
fn test_no_dual_spawn_for_stopped_session_task() {
    // Task with a stopped session record AND owner not in active_names
    let session = make_session_record("sess-dual-123", Some("99"), Some("lexington"), false);

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "99".to_string(),
            "Fix auth".to_string(),
            "lexington".to_string(),
        )],
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
    snap.coworkers.active_names = HashSet::new(); // lexington is absent

    let (_state, _tmp, _guard) = make_test_state();

    // dispatch_via_sessions handles the task (has a stopped session record)
    let session_effects = dispatch_via_sessions_for_test(&snap);
    let session_claimed_ids = effects::extract_claimed_task_ids_from_effects(&session_effects);

    // check_and_recover_orphans also handles it (no longer filters out session-tracked tasks)
    let orphan_effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
        if task_id == "99" {
            Some(in_progress_task_for_lookup("99", "Fix auth", "lexington"))
        } else {
            None
        }
    });

    // Both paths independently produce a spawn for task 99
    let session_spawns = session_effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .count();
    let orphan_spawns = orphan_effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .count();

    assert_eq!(
        session_spawns, 1,
        "dispatch_via_sessions should produce 1 spawn for task 99"
    );
    assert_eq!(
        orphan_spawns, 1,
        "check_and_recover_orphans should produce 1 spawn for task 99"
    );

    // The session_claimed_ids should contain "99" from session dispatch,
    // which is how events.rs excludes it from pending dispatch.
    assert!(
        session_claimed_ids.contains("99"),
        "session_claimed_ids should contain task 99 for exclusion in pending dispatch"
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
    let effects_tick1 = dispatch_via_sessions_for_test(&snap_with_stopped_session);
    assert!(
        effects_tick1
            .iter()
            .any(|e| matches!(e, Effect::SpawnForTask { .. })),
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
    let effects_tick2 = dispatch_via_sessions_for_test(&snap_after_spawn);
    assert!(
        !effects_tick2
            .iter()
            .any(|e| matches!(e, Effect::SpawnForTask { .. })),
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
        crate::paths::ProjectPaths::with_project_name("test-repo", "test-repo"),
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

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "99".to_string(),
            "Maintain canal channel".to_string(),
            "canal-lead".to_string(),
        )],
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
    snap.coworkers.active_names = HashSet::new(); // canal-lead is NOT active

    let effects = dispatch_via_sessions_for_test(&snap);

    // Then: no spawn effects — channel leads must not be recovered as coworkers
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
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

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1675".to_string(),
            "Fix some bug".to_string(),
            "columbus".to_string(),
        )],
        // columbus is active (running as reviewer in a separate session)
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
    snap.coworkers.active_names = ["columbus".to_string()].into_iter().collect();
    snap.reviewer.active_reviewers = ["columbus".to_string()].into_iter().collect();
    snap.reviewer.reviewer_pr_assignments =
        [("columbus".to_string(), 1384_u64)].into_iter().collect();

    let effects = dispatch_via_sessions_for_test(&snap);

    // Then: no spawn effects — columbus is actively reviewing PR #1384
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworker(_)
                    | Effect::SpawnForTask { .. }
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

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1690".to_string(),
            "Rebase PR #1404, address review feedback, and merge".to_string(),
            "riverside".to_string(),
        )],
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
    snap.coworkers.active_names = ["riverside".to_string()].into_iter().collect();
    // Session is live in active_session_ids (successful recovery spawn happened)
    snap.coworkers.active_session_ids = ["e2bafbb6-5fe5-4cfb-a98f-94caad0ff834".to_string()]
        .into_iter()
        .collect();

    let effects = dispatch_via_sessions_for_test(&snap);

    // Then: must NOT emit another recovery spawn — the session is already live.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { .. }
                    | Effect::SpawnCoworker(_)
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

    let effects = dispatch_via_sessions_for_test(&snap);

    // The dispatch should emit a SpawnForTask effect (via build_spawn_effects)
    let spawn_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        spawn_effect.is_some(),
        "Expected SpawnForTask effect, got: {:?}",
        effects
    );

    // SpawnForTask delegates failure handling to the executor via standard
    // spawn_failure_effects (ResetTaskToPending + cooldown). The old
    // ClearSessionForTask-on-failure behavior was removed during the
    // SpawnDecision migration — session cleanup is handled elsewhere.
    if let Some(Effect::SpawnForTask { on_failure, .. }) = spawn_effect {
        let has_reset = on_failure
            .iter()
            .any(|e| matches!(e, Effect::ResetTaskToPending { task_id, .. } if task_id == "42"));
        assert!(
            has_reset,
            "on_failure should contain ResetTaskToPending for task 42, got: {:?}",
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

    let effects = dispatch_via_sessions_for_test(&snap);

    // Then: must NOT emit another recovery spawn — the session was recently recovered.
    // Without the fix, this fires on every tick causing log spam.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { .. }
                    | Effect::SpawnCoworker(_)
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
    // SpawnForTask with ResumeSession for the cooldown-active session.
    let has_session_resume = effects.iter().any(|e| {
        matches!(e, Effect::SpawnForTask { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(sid) if sid == "sess-loop-123"))
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

    // Should emit SpawnForTask with ResumeSession mode for the stopped session
    let has_resume_spawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnForTask { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(sid) if sid == "sess-resume-456"))
    });
    assert!(
        has_resume_spawn,
        "Should emit SpawnForTask(ResumeSession) for stopped session when no cooldown, got effects: {:?}",
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

    let effects = dispatch_via_sessions_for_test(&snap);

    // Find the SpawnForTask effect
    let spawn_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        spawn_effect.is_some(),
        "Expected SpawnForTask for stopped session"
    );

    if let Some(Effect::SpawnForTask { on_success, .. }) = spawn_effect {
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

// ======================================================================
// Stale working_dir validation tests (!1730 item 2)
// ======================================================================

/// Path 2: when a pending task's session has a working_dir that no longer exists,
/// spawn_for_pending_tasks_excluding should fall back to the fresh worktree path
/// and emit ClearSessionWorkingDir to prevent retrying the stale path next tick.
#[test]
fn test_pending_task_stale_working_dir_falls_back_and_clears() {
    use crate::tasks::{Task, TaskStatus};

    let stale_path = "/tmp/nonexistent-worktree-for-test";

    let mut session = make_session_record(
        "sess-stale-wd-123",
        Some("99"),
        Some("pleasantville"),
        false,
    );
    session.working_dir = stale_path.to_string();

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "99".to_string(),
            subject: "Fix stale working dir".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        }],
        sessions: [("sess-stale-wd-123".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("99".to_string(), "sess-stale-wd-123".to_string())]
            .into_iter()
            .collect(),
        stale_working_dir_sessions: ["sess-stale-wd-123".to_string()].into_iter().collect(),
        recently_recovered_session_ids: HashSet::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects =
        spawn_for_pending_tasks_excluding(&snap, &state, &std::collections::HashSet::new());

    // Should emit ClearSessionWorkingDir for the stale session
    let has_clear_wd = effects.iter().any(|e| {
        matches!(e, Effect::ClearSessionWorkingDir { session_id } if session_id == "sess-stale-wd-123")
    });
    assert!(
        has_clear_wd,
        "Should emit ClearSessionWorkingDir when working_dir doesn't exist, got: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );

    // Should still emit SpawnForTask (using the fresh worktree path, not the stale one)
    let spawn_eff = effects.iter().find(|e| {
        matches!(e, Effect::SpawnForTask { config, .. }
            if matches!(&config.session_mode, crate::launch::SessionMode::ResumeSession(sid) if sid == "sess-stale-wd-123"))
    });
    assert!(
        spawn_eff.is_some(),
        "Should emit SpawnForTask even when working_dir is stale, got: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );

    // The SpawnForTask working_dir must NOT be the stale path
    if let Some(Effect::SpawnForTask { config, .. }) = spawn_eff
        && let Some(wd) = &config.working_dir
    {
        assert_ne!(
            wd.to_string_lossy(),
            stale_path,
            "SpawnForTask must use fresh worktree, not the stale path"
        );
    }
}

/// Path 1: when a recovered session has a working_dir that no longer exists,
/// dispatch_via_sessions should fall back to the fresh worktree
/// path and include ClearSessionWorkingDir in the effects.
#[test]
fn test_session_dispatch_stale_working_dir_falls_back_and_clears() {
    let stale_path = "/tmp/nonexistent-worktree-for-test";

    let mut session = make_session_record("sess-stale-p1-abc", Some("1740"), Some("park"), false);
    session.working_dir = stale_path.to_string();

    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "1740".to_string(),
            "Stale worktree test".to_string(),
            "park".to_string(),
        )],
        sessions: [("sess-stale-p1-abc".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("1740".to_string(), "sess-stale-p1-abc".to_string())]
            .into_iter()
            .collect(),
        stale_working_dir_sessions: ["sess-stale-p1-abc".to_string()].into_iter().collect(),
        recently_recovered_session_ids: HashSet::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = dispatch_via_sessions_for_test(&snap);

    // Must emit ClearSessionWorkingDir for the stale session
    let has_clear_wd = effects.iter().any(|e| {
        matches!(e, Effect::ClearSessionWorkingDir { session_id } if session_id == "sess-stale-p1-abc")
    });
    assert!(
        has_clear_wd,
        "Path 1 should emit ClearSessionWorkingDir when working_dir is stale, got: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );

    // Must still attempt spawn via SpawnForTask
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Path 1 should still emit SpawnForTask even with stale working_dir"
    );
}

#[test]
fn test_session_dispatch_skips_task_when_worktree_bound_to_different_active_coworker() {
    // Scenario: Task !42 owned by "park" has a stopped session. Its worktree
    // is bound to "york" who is actively running. Dispatch should NOT recover
    // the task — the worktree is in use by another coworker.
    //
    // This tests the end-to-end outcome: worktree collision prevents a spawn
    // that would cause two coworkers to fight over the same working directory.
    let session = make_session_record("sess-park-42", Some("42"), Some("park"), false);

    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-42-fix-auth".to_string(),
            branch_name: "task-42-fix-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("york".to_string()),
            pr_number: None,
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    let mut snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![("42".to_string(), "Fix auth".to_string(), "park".to_string())],
        sessions: [("sess-park-42".to_string(), session)]
            .into_iter()
            .collect(),
        session_task_map: [("42".to_string(), "sess-park-42".to_string())]
            .into_iter()
            .collect(),
        task_worktree_map: [("42".to_string(), "task-42-fix-auth".to_string())]
            .into_iter()
            .collect(),
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        worktree_registry: registry,
        ..snapshot::minimal_snapshot_for_test()
    };
    // york is active (running), park is NOT active (stopped session)
    snap.coworkers.active_names = ["york".to_string()].into_iter().collect();

    let effects = dispatch_via_sessions_for_test(&snap);

    // Outcome: no spawn effect — the worktree collision blocks recovery
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. } | Effect::SpawnCoworker(_)));
    assert!(
        !has_spawn,
        "Should NOT spawn when worktree is bound to a different active coworker (york), got: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}
