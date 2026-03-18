use super::*;
use std::collections::HashSet;
use std::time::SystemTime;

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

/// Regression test: dispatch must not allocate a name that is in active_names
/// even if it's not registered in CoworkerManager.
///
/// Scenario: "park" finished its task and was cleaned up from CoworkerManager,
/// but still has an active session (appears in snap.coworkers.active_names).
/// A new task dispatch should NOT allocate "park" because the session is still running.
///
/// Before fix: next_available_name_excluding only checked CoworkerManager's internal
/// HashMap, so "park" appeared free and got allocated, causing a name collision.
#[test]
fn test_dispatch_excludes_active_session_names() {
    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![crate::tasks::Task {
            id: "300".to_string(),
            subject: "New feature".to_string(),
            status: crate::tasks::TaskStatus::Pending,
            owner: None,
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            created_at: Some(SystemTime::now()),
        }],
        coworkers: snapshot::SnapshotCoworkerState {
            // "park" has an active session but is NOT in CoworkerManager
            active_names: HashSet::from(["park".to_string()]),
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    // Register all AVENUE_NAMES except "park" in CoworkerManager.
    // This makes "park" the only "free" name from CoworkerManager's perspective.
    for (i, name) in crate::coworker::AVENUE_NAMES
        .iter()
        .filter(|&&n| n != "park")
        .enumerate()
    {
        state
            .coworkers
            .register(
                &format!("slot-{i}"),
                name,
                "/tmp".to_string(),
                None,
                "claude-sonnet".to_string(),
                crate::auth::AuthProvider::Claude,
                "default".to_string(),
            )
            .unwrap();
    }

    let effects = spawn_for_pending_tasks(&snap, &state);

    // The task should still be dispatched (overflow names are available).
    // Check both SpawnForTask and NudgeSessionWithCallbacks — before the fix,
    // "park" would be allocated and since it's in active_names, dispatch would
    // try to nudge it (wrong session).
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, effects::Effect::SpawnForTask { .. }));

    // Check no nudge was emitted (which would mean "park" was incorrectly allocated
    // as a running coworker and nudged)
    let has_nudge = effects
        .iter()
        .any(|e| matches!(e, effects::Effect::NudgeSessionWithCallbacks { .. }));

    assert!(
        has_spawn || !has_nudge,
        "Expected task to be dispatched via SpawnForTask (overflow names available), \
         not via a nudge to a wrong session. Before fix: 'park' was allocated from \
         CoworkerManager (appeared free) then nudged because it was in active_names."
    );

    // Verify the preferred_name on the SpawnForTask is not "park"
    if let Some(effects::Effect::SpawnForTask { preferred_name, .. }) = effects
        .iter()
        .find(|e| matches!(e, effects::Effect::SpawnForTask { .. }))
    {
        assert_ne!(
            preferred_name.as_deref().unwrap_or(""),
            "park",
            "Dispatch should NOT allocate 'park' — it has an active session (in active_names) \
             even though it's not in CoworkerManager."
        );
    }

    assert!(
        !has_nudge,
        "No nudge should be emitted — the only name that could trigger a nudge is 'park' \
         (in active_names), which should be excluded from allocation by the fix."
    );
}
