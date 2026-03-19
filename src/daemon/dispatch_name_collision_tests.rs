use super::*;
use std::collections::HashSet;

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
#[test]
fn test_dispatch_excludes_active_session_names() {
    #[allow(clippy::field_reassign_with_default)]
    let ps = {
        let mut ps = DaemonPersistentState::default();
        ps.tick_dir_key = "test-repo".to_string();
        ps.tick_project_name = "test-repo".to_string();
        ps.tick_default_channel = "test-repo".to_string();
        ps.tick_max_in_progress_tasks = 10;
        ps.tick_now = chrono::Utc::now();
        // "park" has an active session but is NOT in CoworkerManager
        ps.tick_active_session_names = HashSet::from(["park".to_string()]);
        ps
    };

    let tasks = vec![crate::task_store::Task {
        id: "300".to_string(),
        subject: "New feature".to_string(),
        status: crate::task_store::TaskStatus::Pending,
        agent_name: String::new(),
        blocked_by: vec![],
        description: None,
        channel: None,
        pr: None,
        ..Default::default()
    }];

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&ps, &tasks, &state);

    // The task should still be dispatched (generated names are available).
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, effects::Effect::SpawnForTask { .. }));

    let has_nudge = effects
        .iter()
        .any(|e| matches!(e, effects::Effect::NudgeSessionWithCallbacks { .. }));

    assert!(
        has_spawn || !has_nudge,
        "Expected task to be dispatched via SpawnForTask, not via a nudge to a wrong session."
    );

    // Verify the preferred_name on the SpawnForTask is not "park"
    if let Some(effects::Effect::SpawnForTask { preferred_name, .. }) = effects
        .iter()
        .find(|e| matches!(e, effects::Effect::SpawnForTask { .. }))
    {
        assert_ne!(
            preferred_name.as_deref().unwrap_or(""),
            "park",
            "Dispatch should NOT allocate 'park' — it has an active session."
        );
    }

    assert!(
        !has_nudge,
        "No nudge should be emitted — 'park' should be excluded from allocation."
    );
}
