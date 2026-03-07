use super::*;

#[test]
fn test_daemon_socket_for_repo() {
    let path = daemon_socket_for_repo("myproject");
    assert!(path.to_string_lossy().contains("midtown"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("daemon.sock"));
}

#[test]
fn test_daemon_socket_different_repos() {
    let path1 = daemon_socket_for_repo("project-a");
    let path2 = daemon_socket_for_repo("project-b");
    assert_ne!(path1, path2);
}

#[test]
fn test_daemon_pid_file_for_repo() {
    let path = daemon_pid_file_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("projects"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("daemon.pid"));
}

#[test]
fn test_daemon_pid_file_different_repos() {
    let path1 = daemon_pid_file_for_repo("project-a");
    let path2 = daemon_pid_file_for_repo("project-b");
    assert_ne!(path1, path2);
}

#[test]
fn test_projects_dir_for_repo() {
    let path = projects_dir_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("projects"));
    assert!(path.to_string_lossy().ends_with("myproject"));
}

#[test]
fn test_legacy_coworkers_dir_for_repo() {
    let path = legacy_coworkers_dir_for_repo("myproject");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(
        s.contains("projects/myproject"),
        "should be under projects/<repo>: {s}"
    );
    assert!(s.ends_with("coworkers"), "should end with coworkers: {s}");
}

#[test]
fn test_lead_dir_for_repo() {
    let path = lead_dir_for_repo("myproject");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(
        s.contains("projects/myproject"),
        "should be under projects/<repo>: {s}"
    );
}

#[test]
fn test_lead_session_file_for_repo() {
    let path = lead_session_file_for_repo("myproject");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(
        s.contains("projects/myproject"),
        "should be under projects/<repo>: {s}"
    );
    assert!(
        s.ends_with("lead-session-id"),
        "should end with lead-session-id: {s}"
    );
}

#[test]
fn test_lead_session_file_not_under_lead_dir() {
    let path = lead_session_file_for_repo("myproject");
    let s = path.to_string_lossy();
    // Should NOT be under the old ~/.midtown/lead/ directory
    assert!(
        !s.contains("/lead/myproject"),
        "should NOT be under old lead/<repo> path: {s}"
    );
}

#[test]
fn test_task_list_id_for_repo() {
    let id = task_list_id_for_repo("myproject");
    assert_eq!(id, "midtown-myproject");
}

#[test]
fn test_channel_file_for_repo() {
    let path = channel_file_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("projects"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("current.jsonl"));
}

#[test]
fn test_channel_file_for_repo_uses_repo_as_channel_name() {
    // The channel directory should be derived from the repo name, not hardcoded "midtown".
    // For repo "myproject", the path should contain channels/myproject/, not channels/midtown/.
    let path = channel_file_for_repo("myproject");
    let path_str = path.to_string_lossy();
    assert!(
        path_str.contains("channels/myproject/"),
        "Expected channel dir to be 'myproject', got: {}",
        path_str
    );
}

#[test]
fn test_cursors_dir_for_repo() {
    let path = cursors_dir_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("projects"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("cursors"));
}

#[test]
fn test_daemon_log_dir_for_repo() {
    let path = daemon_log_dir_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("projects"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("logs"));
}

#[test]
fn test_atomic_rename_succeeds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target.json");
    let tmp_file = tmp.path().join("target.json.tmp");

    fs::write(&tmp_file, r#"{"ok": true}"#).unwrap();
    atomic_rename(&tmp_file, &target).unwrap();

    assert!(!tmp_file.exists(), "temp file should be gone after rename");
    assert!(target.exists(), "target should exist after rename");
    assert_eq!(fs::read_to_string(&target).unwrap(), r#"{"ok": true}"#);
}

#[test]
fn test_atomic_rename_cleans_tmp_on_failure() {
    let tmp = tempfile::TempDir::new().unwrap();
    let target = tmp.path().join("target.json");
    let tmp_file = tmp.path().join("target.json.tmp");

    // Make target a directory so rename(file, dir) fails
    fs::create_dir(&target).unwrap();
    fs::write(&tmp_file, r#"{"ok": true}"#).unwrap();
    assert!(tmp_file.exists());

    let result = atomic_rename(&tmp_file, &target);
    assert!(result.is_err(), "rename file → dir should fail");
    assert!(
        !tmp_file.exists(),
        "temp file should be cleaned up after failed rename"
    );
}

#[cfg(unix)]
#[test]
fn test_atomic_rename_leaks_tmp_when_cleanup_fails() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let subdir = tmp.path().join("restricted");
    fs::create_dir(&subdir).unwrap();

    let tmp_file = subdir.join("target.json.tmp");
    let target = subdir.join("target.json");

    fs::write(&tmp_file, "data").unwrap();
    // Make target a directory so rename would fail
    fs::create_dir(&target).unwrap();
    // Remove write permission on parent so remove_file also fails
    fs::set_permissions(&subdir, fs::Permissions::from_mode(0o555)).unwrap();

    let result = atomic_rename(&tmp_file, &target);
    assert!(result.is_err(), "rename should fail");

    // Restore permissions so we can inspect and clean up
    fs::set_permissions(&subdir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        tmp_file.exists(),
        "temp file should be leaked when cleanup fails"
    );
}

#[test]
fn test_lead_worktree_path() {
    let path = lead_worktree_path("myrepo");
    let s = path.to_string_lossy();
    assert!(
        s.ends_with("projects/myrepo/worktrees/lead"),
        "expected projects/<repo>/worktrees/lead, got: {s}"
    );
    assert_eq!(path, worktrees_dir_for_repo("myrepo").join("lead"));
}

#[test]
fn test_worktrees_dir_for_repo_new_path() {
    let path = worktrees_dir_for_repo("myproject");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(
        s.contains("projects/myproject/worktrees"),
        "should be under projects/<repo>/worktrees: {s}"
    );
    assert!(s.ends_with("worktrees"), "should end with worktrees: {s}");
}

#[test]
fn test_migrate_returns_false_when_nothing_to_migrate() {
    // Non-existent repo should return false
    let result = migrate_directory_structure("nonexistent-test-repo-xyz123");
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn test_migrate_directory_structure_moves_old_layout_without_recursion() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-recursive-migration-repo";
    let old_repo_dir = tmp.path().join(repo);
    fs::create_dir_all(old_repo_dir.join("logs")).unwrap();
    fs::create_dir_all(old_repo_dir.join("worktrees").join("task-42")).unwrap();
    fs::write(old_repo_dir.join("channel.jsonl"), "[]\n").unwrap();
    fs::write(old_repo_dir.join("lead-session-id"), "session-123\n").unwrap();
    fs::write(
        old_repo_dir
            .join("worktrees")
            .join("task-42")
            .join("README.md"),
        "hello\n",
    )
    .unwrap();

    // If recursion regresses, this call overflows the stack and the test never reaches assertions.
    let result = migrate_directory_structure(repo);
    assert!(result.is_ok(), "migration should succeed");
    assert!(result.unwrap(), "migration should report changes");

    let new_projects_dir = tmp.path().join("projects").join(repo);
    assert!(
        new_projects_dir.join("channel.jsonl").exists(),
        "channel file should move to projects/<repo>"
    );
    assert!(
        new_projects_dir
            .join("worktrees")
            .join("task-42")
            .join("README.md")
            .exists(),
        "worktree should move to projects/<repo>/worktrees"
    );
    assert!(
        new_projects_dir.join("logs").exists(),
        "logs should move to projects/<repo>"
    );
    assert!(
        new_projects_dir.join("lead-session-id").exists(),
        "lead session id should move to projects/<repo>/lead-session-id"
    );
}

#[test]
fn test_migrate_worktree_paths_moves_worktrees_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create old-style worktrees directory
    let old_worktrees = tmp.path().join("worktrees").join(repo);
    fs::create_dir_all(old_worktrees.join("task-42")).unwrap();
    fs::write(old_worktrees.join("task-42").join("README"), "test").unwrap();

    // Run migration
    let result = do_migrate_worktree_paths(repo);
    assert!(result.is_ok());
    assert!(result.unwrap(), "should have migrated");

    // Verify new path exists
    let new_worktrees = tmp.path().join("projects").join(repo).join("worktrees");
    assert!(new_worktrees.exists(), "new worktrees dir should exist");
    assert!(
        new_worktrees.join("task-42").join("README").exists(),
        "migrated content should exist"
    );

    // Verify old path is gone
    assert!(
        !old_worktrees.exists(),
        "old worktrees dir should be removed"
    );
}

#[test]
fn test_migrate_worktree_paths_moves_coworkers_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create old-style coworkers directory
    let old_coworkers = tmp.path().join("coworkers").join(repo);
    fs::create_dir_all(old_coworkers.join("alice")).unwrap();
    fs::write(old_coworkers.join("alice").join("Cargo.toml"), "test").unwrap();

    // Run migration
    let result = do_migrate_worktree_paths(repo);
    assert!(result.is_ok());
    assert!(result.unwrap(), "should have migrated");

    // Verify new path exists
    let new_coworkers = tmp.path().join("projects").join(repo).join("coworkers");
    assert!(new_coworkers.exists(), "new coworkers dir should exist");
    assert!(
        new_coworkers.join("alice").join("Cargo.toml").exists(),
        "migrated content should exist"
    );

    // Verify old path is gone
    assert!(
        !old_coworkers.exists(),
        "old coworkers dir should be removed"
    );
}

#[test]
fn test_migrate_worktree_paths_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create new-style directory already in place (no old dir)
    let new_worktrees = tmp.path().join("projects").join(repo).join("worktrees");
    fs::create_dir_all(new_worktrees.join("task-99")).unwrap();

    // Migration should return false (nothing to migrate)
    let result = do_migrate_worktree_paths(repo);
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "should not migrate when new path already exists"
    );

    // Content should still be there
    assert!(new_worktrees.join("task-99").exists());
}

#[test]
fn test_migrate_worktree_paths_skips_if_new_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create both old and new worktrees dirs
    let old_worktrees = tmp.path().join("worktrees").join(repo);
    fs::create_dir_all(old_worktrees.join("old-task")).unwrap();
    let new_worktrees = tmp.path().join("projects").join(repo).join("worktrees");
    fs::create_dir_all(new_worktrees.join("new-task")).unwrap();

    // Migration should not overwrite existing new dir
    let result = do_migrate_worktree_paths(repo);
    assert!(result.is_ok());

    // New content preserved
    assert!(new_worktrees.join("new-task").exists());
    // Old content still there (rename was skipped because new exists)
    assert!(old_worktrees.join("old-task").exists());
}

#[test]
fn test_migrate_worktree_paths_propagates_rename_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create old worktrees dir with content
    let old_worktrees = tmp.path().join("worktrees").join(repo);
    fs::create_dir_all(old_worktrees.join("some-task")).unwrap();

    // Create a *file* at the target path where a directory rename would need
    // to land — this makes fs::rename fail (can't rename dir over file).
    let projects_dir = tmp.path().join("projects").join(repo);
    fs::create_dir_all(&projects_dir).unwrap();
    fs::write(projects_dir.join("worktrees"), "blocking-file").unwrap();

    let result = do_migrate_worktree_paths(repo);
    // The new path exists as a file, so the rename should be skipped
    // (guard: `!new_worktrees.exists()`). The function should succeed but
    // report no migration was performed, since old content remains.
    assert!(result.is_ok());
    assert!(!result.unwrap()); // No migration happened — new path already exists
    // Old content should still be in place
    assert!(old_worktrees.join("some-task").exists());
}

// ── migrate_lead_to_project ────────────────────────────────────────────

#[test]
fn test_migrate_lead_to_project_moves_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create old-style lead directory with files
    let old_lead_dir = tmp.path().join("lead").join(repo);
    fs::create_dir_all(&old_lead_dir).unwrap();
    fs::write(old_lead_dir.join("session-id"), "session-123").unwrap();
    fs::write(old_lead_dir.join("system-prompt.txt"), "# System Prompt").unwrap();
    fs::write(old_lead_dir.join("lead-initialized"), "").unwrap();

    // Ensure project dir exists
    let projects_dir = tmp.path().join("projects").join(repo);
    fs::create_dir_all(&projects_dir).unwrap();

    // Run migration
    let result = do_migrate_lead_to_project(repo);
    assert!(result.is_ok());
    assert!(result.unwrap(), "should have migrated");

    // Verify files moved to project directory
    assert!(
        projects_dir.join("lead-session-id").exists(),
        "session-id should move to projects/<repo>/lead-session-id"
    );
    assert_eq!(
        fs::read_to_string(projects_dir.join("lead-session-id")).unwrap(),
        "session-123"
    );
    assert!(
        projects_dir.join("lead-system-prompt.txt").exists(),
        "system-prompt.txt should move to projects/<repo>/lead-system-prompt.txt"
    );
    assert!(
        projects_dir.join("lead-initialized").exists(),
        "lead-initialized should move to projects/<repo>/lead-initialized"
    );

    // Old lead directory should be cleaned up
    assert!(!old_lead_dir.exists(), "old lead dir should be removed");
}

#[test]
fn test_migrate_lead_to_project_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // No old lead dir exists
    let result = do_migrate_lead_to_project(repo);
    assert!(result.is_ok());
    assert!(!result.unwrap(), "nothing to migrate");
}

#[test]
fn test_migrate_lead_to_project_skips_if_target_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";

    // Create old lead dir with session-id
    let old_lead_dir = tmp.path().join("lead").join(repo);
    fs::create_dir_all(&old_lead_dir).unwrap();
    fs::write(old_lead_dir.join("session-id"), "old-session").unwrap();

    // Create target file already in project dir
    let projects_dir = tmp.path().join("projects").join(repo);
    fs::create_dir_all(&projects_dir).unwrap();
    fs::write(projects_dir.join("lead-session-id"), "new-session").unwrap();

    let result = do_migrate_lead_to_project(repo);
    assert!(result.is_ok());

    // Target file should NOT be overwritten
    assert_eq!(
        fs::read_to_string(projects_dir.join("lead-session-id")).unwrap(),
        "new-session",
        "existing file should not be overwritten"
    );
}

// ── workflow_script_for_channel ────────────────────────────────────────

#[test]
fn test_workflow_script_none_when_no_files_exist() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    let result = workflow_script_for_channel("my-channel", &project_root, "myrepo");
    assert!(result.is_none());
}

#[test]
fn test_workflow_script_channel_specific_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    let script = project_root
        .join(".midtown")
        .join("channels")
        .join("my-channel")
        .join("workflow.py");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "# channel-specific repo workflow").unwrap();

    let result = workflow_script_for_channel("my-channel", &project_root, "myrepo");
    assert_eq!(result, Some(script));
}

#[test]
fn test_workflow_script_channel_specific_local() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    let script = tmp
        .path()
        .join("home")
        .join("projects")
        .join("myrepo")
        .join("channels")
        .join("my-channel")
        .join("workflow.py");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "# channel-specific local workflow").unwrap();

    let result = workflow_script_for_channel("my-channel", &project_root, "myrepo");
    assert_eq!(result, Some(script));
}

#[test]
fn test_workflow_script_project_default_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    let script = project_root.join(".midtown").join("workflow.py");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "# project default repo workflow").unwrap();

    let result = workflow_script_for_channel("my-channel", &project_root, "myrepo");
    assert_eq!(result, Some(script));
}

#[test]
fn test_workflow_script_project_default_local() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    fs::create_dir_all(&project_root).unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    let script = tmp
        .path()
        .join("home")
        .join("projects")
        .join("myrepo")
        .join("workflow.py");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "# project default local workflow").unwrap();

    let result = workflow_script_for_channel("my-channel", &project_root, "myrepo");
    assert_eq!(result, Some(script));
}

#[test]
fn test_workflow_script_priority_channel_specific_repo_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    // Create all 4 candidates
    let candidates = [
        project_root
            .join(".midtown")
            .join("channels")
            .join("ch")
            .join("workflow.py"),
        tmp.path()
            .join("home")
            .join("projects")
            .join("repo")
            .join("channels")
            .join("ch")
            .join("workflow.py"),
        project_root.join(".midtown").join("workflow.py"),
        tmp.path()
            .join("home")
            .join("projects")
            .join("repo")
            .join("workflow.py"),
    ];
    for s in &candidates {
        fs::create_dir_all(s.parent().unwrap()).unwrap();
        fs::write(s, "# script").unwrap();
    }

    // Highest priority (index 0) should win
    let result = workflow_script_for_channel("ch", &project_root, "repo");
    assert_eq!(result, Some(candidates[0].clone()));
}

#[test]
fn test_workflow_script_priority_channel_specific_local_over_project_defaults() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    // Only create candidates 2, 3, 4 (skip channel-specific repo)
    let channel_local = tmp
        .path()
        .join("home")
        .join("projects")
        .join("repo")
        .join("channels")
        .join("ch")
        .join("workflow.py");
    let project_default_repo = project_root.join(".midtown").join("workflow.py");
    let project_default_local = tmp
        .path()
        .join("home")
        .join("projects")
        .join("repo")
        .join("workflow.py");

    for s in [
        &channel_local,
        &project_default_repo,
        &project_default_local,
    ] {
        fs::create_dir_all(s.parent().unwrap()).unwrap();
        fs::write(s, "# script").unwrap();
    }

    let result = workflow_script_for_channel("ch", &project_root, "repo");
    assert_eq!(result, Some(channel_local));
}

#[test]
fn test_workflow_script_project_default_repo_over_local() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().join("project");
    let _guard = set_test_midtown_base_dir(tmp.path().join("home"));

    // Only create candidates 3 and 4 (both project defaults)
    let project_default_repo = project_root.join(".midtown").join("workflow.py");
    let project_default_local = tmp
        .path()
        .join("home")
        .join("projects")
        .join("repo")
        .join("workflow.py");

    for s in [&project_default_repo, &project_default_local] {
        fs::create_dir_all(s.parent().unwrap()).unwrap();
        fs::write(s, "# script").unwrap();
    }

    let result = workflow_script_for_channel("ch", &project_root, "repo");
    assert_eq!(result, Some(project_default_repo));
}

// ── assets_dir_for_repo ───────────────────────────────────────────────

#[test]
fn test_assets_dir_for_repo_path_structure() {
    let path = assets_dir_for_repo("myproject");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(s.contains("projects"), "should be under projects/: {s}");
    assert!(s.contains("myproject"), "should include repo name: {s}");
    assert!(s.ends_with("assets"), "should end with 'assets': {s}");
}

#[test]
fn test_assets_dir_for_repo_is_under_projects_dir() {
    let assets = assets_dir_for_repo("myproject");
    let projects = projects_dir_for_repo("myproject");
    assert_eq!(assets, projects.join("assets"));
}

#[test]
fn test_assets_dir_different_repos_differ() {
    let path_a = assets_dir_for_repo("repo-a");
    let path_b = assets_dir_for_repo("repo-b");
    assert_ne!(path_a, path_b);
}

// ── enumerate_daemon_sockets ───────────────────────────────────────────

#[test]
fn test_enumerate_daemon_sockets_empty_when_dir_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nonexistent = tmp.path().join("nonexistent");
    let result = enumerate_daemon_sockets_in(&nonexistent);
    assert!(result.is_empty());
}

#[test]
fn test_enumerate_daemon_sockets_finds_sockets() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = tmp.path();

    // Create two project dirs with daemon.sock files
    let proj_a = state.join("project-a");
    let proj_b = state.join("project-b");
    fs::create_dir_all(&proj_a).unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    fs::write(proj_a.join("daemon.sock"), "").unwrap();
    fs::write(proj_b.join("daemon.sock"), "").unwrap();

    let mut result = enumerate_daemon_sockets_in(state);
    result.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].0, "project-a");
    assert_eq!(result[0].1, proj_a.join("daemon.sock"));
    assert_eq!(result[1].0, "project-b");
    assert_eq!(result[1].1, proj_b.join("daemon.sock"));
}

#[test]
fn test_enumerate_daemon_sockets_skips_dirs_without_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = tmp.path();

    // One dir with socket, one without
    let proj_a = state.join("has-daemon");
    let proj_b = state.join("no-daemon");
    fs::create_dir_all(&proj_a).unwrap();
    fs::create_dir_all(&proj_b).unwrap();
    fs::write(proj_a.join("daemon.sock"), "").unwrap();
    // proj_b has no daemon.sock

    let result = enumerate_daemon_sockets_in(state);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "has-daemon");
}

#[test]
fn test_enumerate_daemon_sockets_skips_files_not_dirs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let state = tmp.path();

    // Create a regular file (not a dir) at the top level
    fs::write(state.join("not-a-dir"), "").unwrap();
    // And a real project dir with socket
    let proj = state.join("real-project");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join("daemon.sock"), "").unwrap();

    let result = enumerate_daemon_sockets_in(state);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "real-project");
}

// ── workflow_state_file ────────────────────────────────────────────────

#[test]
fn test_workflow_state_file_path_structure() {
    let path = workflow_state_file("my-channel", "myrepo");
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"), "should be under .midtown: {s}");
    assert!(s.contains("projects"), "should be under projects/: {s}");
    assert!(s.contains("myrepo"), "should include repo name: {s}");
    assert!(s.contains("channels"), "should be under channels/: {s}");
    assert!(s.contains("my-channel"), "should include channel name: {s}");
    assert!(
        s.ends_with("workflow-state.json"),
        "should end with workflow-state.json: {s}"
    );
}

#[test]
fn test_workflow_state_file_different_channels_differ() {
    let path_a = workflow_state_file("channel-a", "myrepo");
    let path_b = workflow_state_file("channel-b", "myrepo");
    assert_ne!(path_a, path_b);
}

#[test]
fn test_workflow_state_file_different_repos_differ() {
    let path_a = workflow_state_file("my-channel", "repo-a");
    let path_b = workflow_state_file("my-channel", "repo-b");
    assert_ne!(path_a, path_b);
}

// ── Plugin discovery tests ───────────────────────────────────────────────────

#[test]
fn test_discover_plugin_dirs_empty_when_no_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert!(dirs.is_empty());
}

#[test]
fn test_discover_plugin_dirs_finds_repo_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("my_plugin.py"), "# plugin").unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0], plugin_dir);
}

#[test]
fn test_discover_plugin_dirs_skips_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    // No .py files — should be skipped.

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert!(dirs.is_empty());
}

#[test]
fn test_discover_plugin_dirs_skips_underscore_files() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    // Only __init__.py — should be skipped.
    std::fs::write(plugin_dir.join("__init__.py"), "").unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert!(dirs.is_empty());
}

#[test]
fn test_discover_plugin_dirs_finds_non_underscore_py() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("__init__.py"), "").unwrap();
    std::fs::write(plugin_dir.join("hooks.py"), "# hooks").unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert_eq!(dirs.len(), 1);
}

#[test]
fn test_discover_plugin_dirs_channel_specific() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");

    // Set up channel-specific plugin dir
    let channel_plugin_dir = project_root
        .join(".midtown")
        .join("channels")
        .join("proj-auth")
        .join("plugins");
    std::fs::create_dir_all(&channel_plugin_dir).unwrap();
    std::fs::write(channel_plugin_dir.join("review.py"), "# review").unwrap();

    // Set up project-wide plugin dir
    let project_plugin_dir = project_root.join(".midtown").join("plugins");
    std::fs::create_dir_all(&project_plugin_dir).unwrap();
    std::fs::write(project_plugin_dir.join("default.py"), "# default").unwrap();

    // With channel: both dirs found, channel-specific first
    let dirs = discover_plugin_dirs(
        &project_root,
        "nonexistent-repo-discover-test",
        Some("proj-auth"),
    );
    assert_eq!(dirs.len(), 2);
    assert_eq!(dirs[0], channel_plugin_dir);
    assert_eq!(dirs[1], project_plugin_dir);

    // Without channel: only project-wide dir
    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0], project_plugin_dir);
}

#[test]
fn test_discover_plugin_dirs_agentskills_format() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");

    // Create an AgentSkills-format plugin (directory with SKILL.md)
    let skill_dir = plugin_dir.join("tdw");
    let scripts_dir = skill_dir.join("scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: tdw\n---\n# TDW Plugin",
    )
    .unwrap();
    std::fs::write(scripts_dir.join("hooks.py"), "# hooks").unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0], plugin_dir);
}

#[test]
fn test_discover_plugin_dirs_skips_dir_without_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let plugin_dir = project_root.join(".midtown").join("plugins");

    // Create a subdirectory WITHOUT SKILL.md — should not count as a plugin
    let subdir = plugin_dir.join("not-a-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join("README.md"), "# Not a plugin").unwrap();

    let dirs = discover_plugin_dirs(&project_root, "nonexistent-repo-discover-test", None);
    assert!(dirs.is_empty());
}

// ── AGENTS.md discovery tests ───────────────────────────────────────────────

#[test]
fn test_agents_md_for_channel_none_when_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();

    let result = agents_md_for_channel("web", &project_root, "nonexistent-repo-agents-test");
    assert!(result.is_none());
}

#[test]
fn test_agents_md_for_channel_finds_channel_specific_in_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let agents_dir = project_root.join(".midtown").join("channels").join("web");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("AGENTS.md"), "# Web Workflow").unwrap();

    let result = agents_md_for_channel("web", &project_root, "nonexistent-repo-agents-test");
    assert_eq!(result.as_deref(), Some("# Web Workflow"));
}

#[test]
fn test_agents_md_for_channel_finds_project_wide_in_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let midtown_dir = project_root.join(".midtown");
    std::fs::create_dir_all(&midtown_dir).unwrap();
    std::fs::write(midtown_dir.join("AGENTS.md"), "# Project Workflow").unwrap();

    let result = agents_md_for_channel("web", &project_root, "nonexistent-repo-agents-test");
    assert_eq!(result.as_deref(), Some("# Project Workflow"));
}

#[test]
fn test_agents_md_for_channel_channel_specific_wins_over_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");

    // Create channel-specific AGENTS.md
    let channel_dir = project_root.join(".midtown").join("channels").join("auth");
    std::fs::create_dir_all(&channel_dir).unwrap();
    std::fs::write(channel_dir.join("AGENTS.md"), "# Auth Channel").unwrap();

    // Create project-wide AGENTS.md
    let midtown_dir = project_root.join(".midtown");
    std::fs::write(midtown_dir.join("AGENTS.md"), "# Project Wide").unwrap();

    let result = agents_md_for_channel("auth", &project_root, "nonexistent-repo-agents-test");
    assert_eq!(result.as_deref(), Some("# Auth Channel"));
}

#[test]
fn test_agents_md_for_channel_skips_empty_files() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let channel_dir = project_root.join(".midtown").join("channels").join("web");
    std::fs::create_dir_all(&channel_dir).unwrap();
    std::fs::write(channel_dir.join("AGENTS.md"), "   \n  ").unwrap();

    let result = agents_md_for_channel("web", &project_root, "nonexistent-repo-agents-test");
    assert!(result.is_none());
}

// ── SKILL.md body collection tests ──────────────────────────────────────────

#[test]
fn test_collect_skill_md_bodies_empty_dirs() {
    let results = collect_skill_md_bodies(&[]);
    assert!(results.is_empty());
}

#[test]
fn test_collect_skill_md_bodies_extracts_body_and_name() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");
    let tdw_dir = plugin_dir.join("tdw");
    std::fs::create_dir_all(&tdw_dir).unwrap();
    std::fs::write(
        tdw_dir.join("SKILL.md"),
        "---\nname: tdw\ndescription: Test-Driven Writing\n---\n# TDW\n\nTDW is great.",
    )
    .unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "tdw");
    assert!(results[0].1.contains("# TDW"));
    assert!(results[0].1.contains("TDW is great."));
    // Body should NOT contain frontmatter
    assert!(!results[0].1.contains("---"));
}

#[test]
fn test_collect_skill_md_bodies_falls_back_to_dir_name() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");
    let my_plugin = plugin_dir.join("my-plugin");
    std::fs::create_dir_all(&my_plugin).unwrap();
    // SKILL.md with no name in frontmatter
    std::fs::write(
        my_plugin.join("SKILL.md"),
        "---\ndescription: A plugin\n---\n# My Plugin Body",
    )
    .unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "my-plugin");
    assert!(results[0].1.contains("# My Plugin Body"));
}

#[test]
fn test_collect_skill_md_bodies_skips_empty_body() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");
    let skill_dir = plugin_dir.join("empty");
    std::fs::create_dir_all(&skill_dir).unwrap();
    // SKILL.md with frontmatter but no body
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: empty\n---\n").unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert!(results.is_empty());
}

#[test]
fn test_collect_skill_md_bodies_no_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");
    let skill_dir = plugin_dir.join("plain");
    std::fs::create_dir_all(&skill_dir).unwrap();
    // SKILL.md with no frontmatter at all
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Plain Plugin\n\nJust content.",
    )
    .unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "plain"); // falls back to dir name
    assert!(results[0].1.contains("# Plain Plugin"));
}

#[test]
fn test_collect_skill_md_bodies_multiple_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");

    // Plugin A
    let a_dir = plugin_dir.join("alpha");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::write(
        a_dir.join("SKILL.md"),
        "---\nname: alpha\n---\n# Alpha Plugin",
    )
    .unwrap();

    // Plugin B
    let b_dir = plugin_dir.join("beta");
    std::fs::create_dir_all(&b_dir).unwrap();
    std::fs::write(
        b_dir.join("SKILL.md"),
        "---\nname: beta\n---\n# Beta Plugin",
    )
    .unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert_eq!(results.len(), 2);
    let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
}

#[test]
fn test_headless_output_file_in_sessions_dir() {
    let path = headless_output_file("myproject", "york");
    let s = path.to_string_lossy();
    assert!(
        s.contains("projects/myproject/sessions/"),
        "should be under projects/<repo>/sessions/: {s}"
    );
    assert!(
        s.ends_with("headless-york.jsonl"),
        "should end with headless-<name>.jsonl: {s}"
    );
}

#[test]
fn test_headless_output_method_in_sessions_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());
    let paths = ProjectPaths::with_project_name("myproject", "myproject");
    let path = paths.headless_output("york");
    let s = path.to_string_lossy();
    assert!(
        s.contains("sessions/headless-york.jsonl"),
        "should be under sessions/: {s}"
    );
}

#[test]
fn test_collect_skill_md_bodies_skips_bare_py_files() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    // Bare .py files should be ignored (no SKILL.md to extract body from)
    std::fs::write(plugin_dir.join("hooks.py"), "# hooks").unwrap();

    let results = collect_skill_md_bodies(&[plugin_dir]);
    assert!(results.is_empty());
}

// ── migrate_headless_transcripts_to_sessions ─────────────────────────

#[test]
fn test_migrate_headless_transcripts_moves_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";
    let project_dir = tmp.path().join("projects").join(repo);
    fs::create_dir_all(&project_dir).unwrap();

    // Create old-style headless transcript files at the project root
    fs::write(project_dir.join("headless-york.jsonl"), "line1\n").unwrap();
    fs::write(project_dir.join("headless-park.jsonl"), "line2\n").unwrap();
    // Non-headless file should NOT be moved
    fs::write(project_dir.join("daemon.pid"), "12345").unwrap();

    let result = migrate_headless_transcripts_to_sessions(repo);
    assert!(result.is_ok());
    assert!(result.unwrap(), "should have migrated");

    let sessions_dir = project_dir.join("sessions");
    assert!(sessions_dir.join("headless-york.jsonl").exists());
    assert!(sessions_dir.join("headless-park.jsonl").exists());
    assert_eq!(
        fs::read_to_string(sessions_dir.join("headless-york.jsonl")).unwrap(),
        "line1\n"
    );
    // Original files should be gone
    assert!(!project_dir.join("headless-york.jsonl").exists());
    assert!(!project_dir.join("headless-park.jsonl").exists());
    // Non-headless file should remain
    assert!(project_dir.join("daemon.pid").exists());
}

#[test]
fn test_migrate_headless_transcripts_noop_when_no_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";
    let project_dir = tmp.path().join("projects").join(repo);
    fs::create_dir_all(&project_dir).unwrap();

    let result = migrate_headless_transcripts_to_sessions(repo);
    assert!(result.is_ok());
    assert!(!result.unwrap(), "nothing to migrate");
    // sessions/ should NOT be created when there's nothing to move
    assert!(!project_dir.join("sessions").exists());
}

#[test]
fn test_migrate_headless_transcripts_skips_existing_target() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());

    let repo = "test-repo";
    let project_dir = tmp.path().join("projects").join(repo);
    let sessions_dir = project_dir.join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    // Old file at project root
    fs::write(project_dir.join("headless-york.jsonl"), "old-content\n").unwrap();
    // Newer file already at sessions/
    fs::write(sessions_dir.join("headless-york.jsonl"), "new-content\n").unwrap();

    let result = migrate_headless_transcripts_to_sessions(repo);
    assert!(result.is_ok());

    // Target should NOT be overwritten
    assert_eq!(
        fs::read_to_string(sessions_dir.join("headless-york.jsonl")).unwrap(),
        "new-content\n"
    );
}

#[test]
fn test_detect_repo_name_uses_midtown_dir_key_env_var() {
    // When MIDTOWN_DIR_KEY is set, detect_repo_name() should return it
    // regardless of the current working directory.
    unsafe {
        std::env::set_var("MIDTOWN_DIR_KEY", "pinned-project");
    }
    let result = detect_repo_name();
    unsafe {
        std::env::remove_var("MIDTOWN_DIR_KEY");
    }
    assert_eq!(result, Some("pinned-project".to_string()));
}

#[test]
fn test_detect_repo_name_ignores_empty_midtown_dir_key() {
    // An empty MIDTOWN_DIR_KEY should fall through to CWD-based detection.
    unsafe {
        std::env::set_var("MIDTOWN_DIR_KEY", "");
    }
    let result = detect_repo_name();
    unsafe {
        std::env::remove_var("MIDTOWN_DIR_KEY");
    }
    // Should fall through to CWD-based detection (we're in a git repo,
    // so it should return something, but the key point is it didn't
    // return an empty string).
    assert_ne!(result, Some(String::new()));
}

#[test]
fn test_detect_repo_name_falls_back_to_cwd_without_env_var() {
    // Without MIDTOWN_DIR_KEY, detect_repo_name() should use CWD-based git detection.
    // Since tests run inside this git repo, we should get a repo name.
    unsafe {
        std::env::remove_var("MIDTOWN_DIR_KEY");
    }
    let result = detect_repo_name();
    // We're in a git repo, so this should return Some value
    assert!(result.is_some());
    assert!(!result.unwrap().is_empty());
}

// ── Workflow directory discovery ──────────────────────────────────────────

#[test]
fn test_discover_workflows_finds_valid_workflows() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");

    // Create two workflow directories
    fs::create_dir_all(workflows_dir.join("tdw")).unwrap();
    fs::write(workflows_dir.join("tdw/workflow.py"), "# tdw hooks").unwrap();
    fs::write(workflows_dir.join("tdw/AGENTS.md"), "# TDW").unwrap();

    fs::create_dir_all(workflows_dir.join("spec-review")).unwrap();
    fs::write(
        workflows_dir.join("spec-review/workflow.py"),
        "# spec hooks",
    )
    .unwrap();

    let result = discover_workflows(&workflows_dir);
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|w| w.name == "tdw"));
    assert!(result.iter().any(|w| w.name == "spec-review"));

    // tdw has AGENTS.md, spec-review does not
    let tdw = result.iter().find(|w| w.name == "tdw").unwrap();
    assert!(tdw.agents_md.is_some());
    let spec = result.iter().find(|w| w.name == "spec-review").unwrap();
    assert!(spec.agents_md.is_none());
}

#[test]
fn test_discover_workflows_empty_when_dir_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let nonexistent = tmp.path().join("nonexistent");
    let result = discover_workflows(&nonexistent);
    assert!(result.is_empty());
}

#[test]
fn test_discover_workflows_skips_dirs_without_workflow_py() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");

    // Directory exists but has no workflow.py
    fs::create_dir_all(workflows_dir.join("incomplete")).unwrap();
    fs::write(workflows_dir.join("incomplete/AGENTS.md"), "# Incomplete").unwrap();

    let result = discover_workflows(&workflows_dir);
    assert!(result.is_empty());
}

#[test]
fn test_discover_workflows_skips_files_not_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    // Create a file (not a directory) in workflows/
    fs::write(workflows_dir.join("not-a-workflow.txt"), "text").unwrap();

    let result = discover_workflows(&workflows_dir);
    assert!(result.is_empty());
}

#[test]
fn test_discover_workflows_sorted_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");

    for name in &["zebra", "alpha", "middle"] {
        fs::create_dir_all(workflows_dir.join(name)).unwrap();
        fs::write(workflows_dir.join(name).join("workflow.py"), "# hooks").unwrap();
    }

    let result = discover_workflows(&workflows_dir);
    let names: Vec<&str> = result.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "middle", "zebra"]);
}

#[test]
fn test_project_paths_workflows_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = set_test_midtown_base_dir(tmp.path().to_path_buf());
    let paths = ProjectPaths::with_project_name("myproject", "myproject");
    let wf_dir = paths.workflows_dir();
    let s = wf_dir.to_string_lossy();
    assert!(
        s.ends_with("projects/myproject/workflows"),
        "expected projects/<repo>/workflows, got: {s}"
    );
}

#[test]
fn test_detect_repo_name_rejects_path_traversal_in_midtown_dir_key() {
    // MIDTOWN_DIR_KEY values with path separators or traversal should be rejected.
    let malicious_values = vec!["../other", "foo/bar", "foo\\bar", "..", "."];
    for val in malicious_values {
        unsafe {
            std::env::set_var("MIDTOWN_DIR_KEY", val);
        }
        let result = detect_repo_name();
        unsafe {
            std::env::remove_var("MIDTOWN_DIR_KEY");
        }
        // Should NOT return the malicious value — falls back to CWD detection
        assert_ne!(
            result,
            Some(val.to_string()),
            "MIDTOWN_DIR_KEY={val:?} should have been rejected"
        );
    }
}

// ── workflow_agents_md_content tests ──────────────────────────────────

#[test]
fn workflow_agents_md_content_reads_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join("my-workflow");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("AGENTS.md"),
        "# Workflow Instructions\nDo stuff.",
    )
    .unwrap();

    let result = super::workflow_agents_md_content(tmp.path(), "my-workflow");
    assert_eq!(
        result,
        Some("# Workflow Instructions\nDo stuff.".to_string())
    );
}

#[test]
fn workflow_agents_md_content_returns_none_for_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let result = super::workflow_agents_md_content(tmp.path(), "nonexistent");
    assert_eq!(result, None);
}

#[test]
fn workflow_agents_md_content_returns_none_for_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let wf_dir = tmp.path().join("empty-wf");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join("AGENTS.md"), "   \n  ").unwrap();

    let result = super::workflow_agents_md_content(tmp.path(), "empty-wf");
    assert_eq!(result, None);
}

#[test]
fn workflow_agents_md_content_rejects_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    // Create a directory outside workflows_dir that would be reachable via traversal
    let secret_dir = tmp.path().join("secret");
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(secret_dir.join("AGENTS.md"), "SECRET CONTENT").unwrap();

    let workflows_dir = tmp.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    // Attempt path traversal
    assert_eq!(
        super::workflow_agents_md_content(&workflows_dir, "../secret"),
        None,
        "path traversal with ../ should be rejected"
    );
    assert_eq!(
        super::workflow_agents_md_content(&workflows_dir, "foo/../../secret"),
        None,
        "nested path traversal should be rejected"
    );
    assert_eq!(
        super::workflow_agents_md_content(&workflows_dir, "/tmp/x"),
        None,
        "absolute path should be rejected"
    );
}

// ── merge_workflow_agents_md tests ──────────────────────────────────

#[test]
fn merge_workflow_agents_md_both_present() {
    let existing = Some("Existing agents instructions".to_string());
    let workflow = Some("Workflow specific instructions".to_string());
    let state_summary = Some("- Task !42: phase = observe".to_string());

    let result =
        super::merge_workflow_agents_md(existing, workflow.as_deref(), state_summary.as_deref());
    let result = result.unwrap();
    assert!(result.contains("Existing agents instructions"));
    assert!(result.contains("## Assigned Workflow Instructions"));
    assert!(result.contains("Workflow specific instructions"));
    assert!(result.contains("## Current Workflow State"));
    assert!(result.contains("Task !42: phase = observe"));
}

#[test]
fn merge_workflow_agents_md_only_existing() {
    let existing = Some("Existing agents".to_string());
    let result = super::merge_workflow_agents_md(existing, None, None);
    assert_eq!(result, Some("Existing agents".to_string()));
}

#[test]
fn merge_workflow_agents_md_only_workflow() {
    let workflow = Some("Workflow instructions".to_string());
    let result = super::merge_workflow_agents_md(None, workflow.as_deref(), None);
    let result = result.unwrap();
    assert!(result.contains("## Assigned Workflow Instructions"));
    assert!(result.contains("Workflow instructions"));
}

#[test]
fn merge_workflow_agents_md_none() {
    let result = super::merge_workflow_agents_md(None, None, None);
    assert_eq!(result, None);
}

#[test]
fn merge_workflow_agents_md_state_without_workflow() {
    // State summary without workflow AGENTS.md should still be appended
    let existing = Some("Base agents".to_string());
    let result = super::merge_workflow_agents_md(existing, None, Some("- Task !1: phase = study"));
    let result = result.unwrap();
    assert!(result.contains("Base agents"));
    assert!(result.contains("## Current Workflow State"));
    assert!(result.contains("Task !1: phase = study"));
}

// ── AGENTS.md frontmatter parsing ─────────────────────────────────────────

#[test]
fn test_parse_frontmatter_full() {
    let content = "\
---
name: tdw
description: Test-Driven Writing
states: [study, do, observe, hone]
transitions:
  study: [do]
  do: [observe]
  observe: [hone, do]
  hone: [study]
---
# Body content here
";
    let meta = parse_agents_md_frontmatter(content).unwrap();
    assert_eq!(meta.name.as_deref(), Some("tdw"));
    assert_eq!(meta.description.as_deref(), Some("Test-Driven Writing"));
    assert_eq!(meta.states, vec!["study", "do", "observe", "hone"]);
    assert_eq!(meta.transitions.get("study").unwrap(), &vec!["do"]);
    assert_eq!(meta.transitions.get("do").unwrap(), &vec!["observe"]);
    assert_eq!(
        meta.transitions.get("observe").unwrap(),
        &vec!["hone", "do"]
    );
    assert_eq!(meta.transitions.get("hone").unwrap(), &vec!["study"]);
}

#[test]
fn test_parse_frontmatter_no_transitions() {
    let content = "\
---
name: simple
description: A simple workflow
states: [a, b, c]
---
";
    let meta = parse_agents_md_frontmatter(content).unwrap();
    assert_eq!(meta.name.as_deref(), Some("simple"));
    assert_eq!(meta.states, vec!["a", "b", "c"]);
    assert!(meta.transitions.is_empty());
}

#[test]
fn test_parse_frontmatter_no_frontmatter() {
    let content = "# Just a regular markdown file\nNo frontmatter here.";
    assert!(parse_agents_md_frontmatter(content).is_none());
}

#[test]
fn test_parse_frontmatter_unclosed() {
    let content = "\
---
name: broken
description: Missing closing marker
";
    assert!(parse_agents_md_frontmatter(content).is_none());
}

#[test]
fn test_parse_frontmatter_empty() {
    let content = "\
---
---
";
    let meta = parse_agents_md_frontmatter(content).unwrap();
    assert!(meta.name.is_none());
    assert!(meta.description.is_none());
    assert!(meta.states.is_empty());
    assert!(meta.transitions.is_empty());
}

#[test]
fn test_parse_frontmatter_name_and_description_only() {
    let content = "\
---
name: spec-review
description: Spec-driven review
---
";
    let meta = parse_agents_md_frontmatter(content).unwrap();
    assert_eq!(meta.name.as_deref(), Some("spec-review"));
    assert_eq!(meta.description.as_deref(), Some("Spec-driven review"));
    assert!(meta.states.is_empty());
}

#[test]
fn test_discover_workflows_populates_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");

    // Workflow with frontmatter
    fs::create_dir_all(workflows_dir.join("tdw")).unwrap();
    fs::write(workflows_dir.join("tdw/workflow.py"), "# hooks").unwrap();
    fs::write(
        workflows_dir.join("tdw/AGENTS.md"),
        "\
---
name: tdw
description: Test-Driven Writing
states: [study, do]
transitions:
  study: [do]
  do: [study]
---
# Body
",
    )
    .unwrap();

    // Workflow without AGENTS.md
    fs::create_dir_all(workflows_dir.join("plain")).unwrap();
    fs::write(workflows_dir.join("plain/workflow.py"), "# hooks").unwrap();

    let result = discover_workflows(&workflows_dir);
    assert_eq!(result.len(), 2);

    let tdw = result.iter().find(|w| w.name == "tdw").unwrap();
    let meta = tdw.metadata.as_ref().unwrap();
    assert_eq!(meta.name.as_deref(), Some("tdw"));
    assert_eq!(meta.states, vec!["study", "do"]);
    assert_eq!(meta.transitions.len(), 2);

    let plain = result.iter().find(|w| w.name == "plain").unwrap();
    assert!(plain.metadata.is_none());
}

#[test]
fn test_discover_workflows_bad_frontmatter_gives_none_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");

    fs::create_dir_all(workflows_dir.join("bad")).unwrap();
    fs::write(workflows_dir.join("bad/workflow.py"), "# hooks").unwrap();
    // AGENTS.md without valid frontmatter
    fs::write(
        workflows_dir.join("bad/AGENTS.md"),
        "# No frontmatter here\nJust regular markdown.",
    )
    .unwrap();

    let result = discover_workflows(&workflows_dir);
    assert_eq!(result.len(), 1);
    assert!(result[0].metadata.is_none());
}
