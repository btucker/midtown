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
fn test_coworkers_dir_for_repo() {
    let path = coworkers_dir_for_repo("myproject");
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
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("lead"));
    assert!(path.to_string_lossy().ends_with("myproject"));
}

#[test]
fn test_lead_session_file_for_repo() {
    let path = lead_session_file_for_repo("myproject");
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("lead"));
    assert!(path.to_string_lossy().contains("myproject"));
    assert!(path.to_string_lossy().ends_with("session-id"));
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
