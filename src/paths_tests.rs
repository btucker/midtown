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
    assert!(path.to_string_lossy().contains(".midtown"));
    assert!(path.to_string_lossy().contains("coworkers"));
    assert!(path.to_string_lossy().ends_with("myproject"));
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
    assert!(path.ends_with("worktrees/myrepo/lead"));
    assert_eq!(path, worktrees_dir_for_repo("myrepo").join("lead"));
}

#[test]
fn test_migrate_returns_false_when_nothing_to_migrate() {
    // Non-existent repo should return false
    let result = migrate_directory_structure("nonexistent-test-repo-xyz123");
    assert!(result.is_ok());
    assert!(!result.unwrap());
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
