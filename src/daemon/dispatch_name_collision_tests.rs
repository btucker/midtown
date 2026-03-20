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

/// Reviewer sessions should be named after parent task's agent_name with -reviewer suffix.
#[test]
fn test_reviewer_task_named_after_parent() {
    let old = chrono::Utc::now() - chrono::Duration::seconds(120);

    #[allow(clippy::field_reassign_with_default)]
    let ps = {
        let mut ps = DaemonPersistentState::default();
        ps.tick_dir_key = "test-repo".to_string();
        ps.tick_project_name = "test-repo".to_string();
        ps.tick_default_channel = "test-repo".to_string();
        ps.tick_max_in_progress_tasks = 10;
        ps.tick_now = chrono::Utc::now();
        ps
    };

    let tasks = vec![
        // Parent task with a creative agent_name
        crate::task_store::Task {
            id: "400".to_string(),
            subject: "Implement feature X".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            agent_name: "patch-kit".to_string(),
            created_at: old,
            ..Default::default()
        },
        // Reviewer task pointing to parent
        crate::task_store::Task {
            id: "401".to_string(),
            subject: "Review PR #100".to_string(),
            status: crate::task_store::TaskStatus::Pending,
            agent_name: String::new(),
            agent_type: "midtown-code-reviewer".to_string(),
            parent: Some("400".to_string()),
            pr: Some(100),
            created_at: old,
            ..Default::default()
        },
    ];

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&ps, &tasks, &state);

    // Find the SpawnForTask effect and check the preferred_name
    let spawn = effects
        .iter()
        .find(|e| matches!(e, effects::Effect::SpawnForTask { .. }));
    assert!(spawn.is_some(), "Expected a SpawnForTask effect");

    if let Some(effects::Effect::SpawnForTask { preferred_name, .. }) = spawn {
        assert_eq!(
            preferred_name.as_deref(),
            Some("patch-kit-reviewer"),
            "Reviewer session should be named after parent's agent_name with -reviewer suffix"
        );
    }
}

/// When the reviewer name collides with an active session, a suffix should be appended.
#[test]
fn test_reviewer_task_name_collision_adds_suffix() {
    let old = chrono::Utc::now() - chrono::Duration::seconds(120);

    #[allow(clippy::field_reassign_with_default)]
    let ps = {
        let mut ps = DaemonPersistentState::default();
        ps.tick_dir_key = "test-repo".to_string();
        ps.tick_project_name = "test-repo".to_string();
        ps.tick_default_channel = "test-repo".to_string();
        ps.tick_max_in_progress_tasks = 10;
        ps.tick_now = chrono::Utc::now();
        // Simulate "patch-kit-reviewer" already running
        ps.tick_active_session_names = HashSet::from(["patch-kit-reviewer".to_string()]);
        ps
    };

    let tasks = vec![
        crate::task_store::Task {
            id: "500".to_string(),
            subject: "Implement feature Y".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            agent_name: "patch-kit".to_string(),
            created_at: old,
            ..Default::default()
        },
        crate::task_store::Task {
            id: "501".to_string(),
            subject: "Review PR #200".to_string(),
            status: crate::task_store::TaskStatus::Pending,
            agent_name: String::new(),
            agent_type: "midtown-code-reviewer".to_string(),
            parent: Some("500".to_string()),
            pr: Some(200),
            created_at: old,
            ..Default::default()
        },
    ];

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&ps, &tasks, &state);

    let spawn = effects
        .iter()
        .find(|e| matches!(e, effects::Effect::SpawnForTask { .. }));
    assert!(spawn.is_some(), "Expected a SpawnForTask effect");

    if let Some(effects::Effect::SpawnForTask { preferred_name, .. }) = spawn {
        let name = preferred_name.as_deref().unwrap();
        assert!(
            name.starts_with("patch-kit-reviewer-"),
            "Colliding reviewer name should have a random suffix, got: {}",
            name
        );
        assert_ne!(
            name, "patch-kit-reviewer",
            "Should not be the bare name when it collides"
        );
    }
}

/// When parent task has no agent_name, fall back to generate_task_session_name.
#[test]
fn test_reviewer_task_fallback_when_parent_has_no_name() {
    let old = chrono::Utc::now() - chrono::Duration::seconds(120);

    #[allow(clippy::field_reassign_with_default)]
    let ps = {
        let mut ps = DaemonPersistentState::default();
        ps.tick_dir_key = "test-repo".to_string();
        ps.tick_project_name = "test-repo".to_string();
        ps.tick_default_channel = "test-repo".to_string();
        ps.tick_max_in_progress_tasks = 10;
        ps.tick_now = chrono::Utc::now();
        ps
    };

    let tasks = vec![
        crate::task_store::Task {
            id: "600".to_string(),
            subject: "Implement feature Z".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            agent_name: String::new(), // no agent_name
            created_at: old,
            ..Default::default()
        },
        crate::task_store::Task {
            id: "601".to_string(),
            subject: "Review PR #300".to_string(),
            status: crate::task_store::TaskStatus::Pending,
            agent_name: String::new(),
            agent_type: "midtown-code-reviewer".to_string(),
            parent: Some("600".to_string()),
            pr: Some(300),
            created_at: old,
            ..Default::default()
        },
    ];

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&ps, &tasks, &state);

    let spawn = effects
        .iter()
        .find(|e| matches!(e, effects::Effect::SpawnForTask { .. }));
    assert!(spawn.is_some(), "Expected a SpawnForTask effect");

    if let Some(effects::Effect::SpawnForTask { preferred_name, .. }) = spawn {
        let name = preferred_name.as_deref().unwrap();
        // Should NOT contain "-reviewer" since parent has no agent_name
        assert!(
            !name.contains("-reviewer"),
            "Should fall back to generated name when parent has no agent_name, got: {}",
            name
        );
    }
}
