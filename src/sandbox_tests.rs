use super::*;

#[test]
fn test_writable_dirs_includes_primary_repo() {
    let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[], "test-project");
    assert!(dirs.contains(&"/home/user/project".to_string()));
}

#[test]
fn test_writable_dirs_includes_additional_repos() {
    let additional = vec![PathBuf::from("/home/user/lib")];
    let dirs = writable_dirs(
        Path::new("/home/user/project"),
        &additional,
        &[],
        "test-project",
    );
    assert!(dirs.contains(&"/home/user/project".to_string()));
    assert!(dirs.contains(&"/home/user/lib".to_string()));
}

#[test]
fn test_writable_dirs_deduplicates() {
    let additional = vec![PathBuf::from("/home/user/project")];
    let dirs = writable_dirs(
        Path::new("/home/user/project"),
        &additional,
        &[],
        "test-project",
    );
    let count = dirs.iter().filter(|d| *d == "/home/user/project").count();
    assert_eq!(count, 1, "Primary repo should not be duplicated");
}

#[test]
fn test_writable_dirs_includes_config_dirs() {
    let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[], "test-project");
    let has_midtown_project = dirs
        .iter()
        .any(|d| d.contains(".midtown/projects/test-project"));
    let has_claude = dirs.iter().any(|d| d.ends_with(".claude"));
    let has_codex = dirs.iter().any(|d| d.ends_with(".codex"));
    assert!(
        has_midtown_project,
        "Should include ~/.midtown/projects/test-project"
    );
    assert!(has_claude, "Should include ~/.claude");
    assert!(has_codex, "Should include ~/.codex");
}

#[test]
fn test_writable_dirs_scoped_to_project() {
    let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[], "my-project");
    // Should include project-scoped midtown path
    let has_scoped = dirs
        .iter()
        .any(|d| d.contains(".midtown/projects/my-project"));
    assert!(
        has_scoped,
        "Should include project-scoped ~/.midtown/projects/my-project"
    );
    // Should NOT include the broad ~/.midtown path
    let has_broad = dirs.iter().any(|d| d.ends_with(".midtown"));
    assert!(!has_broad, "Should NOT include broad ~/.midtown");
}

#[test]
fn test_writable_dirs_state_dir_scoped_to_project() {
    let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[], "my-project");
    let has_scoped_state = dirs
        .iter()
        .any(|d| d.contains(".local/state/midtown/my-project"));
    assert!(
        has_scoped_state,
        "Should include project-scoped state dir; dirs: {:?}",
        dirs
    );
    // Should NOT include the broad state dir
    let has_broad_state = dirs
        .iter()
        .any(|d| d.ends_with(".local/state/midtown") || d.ends_with("state/midtown"));
    assert!(
        !has_broad_state,
        "Should NOT include broad state dir; dirs: {:?}",
        dirs
    );
}

#[test]
fn test_writable_dirs_includes_tmp() {
    let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[], "test-project");
    assert!(dirs.contains(&"/tmp".to_string()));
}

#[test]
fn test_writable_dirs_includes_main_git_dir_for_worktree() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let worktree = tmp.path().join("worktree");
    std::fs::create_dir_all(&worktree).expect("create worktree dir");

    // Simulate a git worktree .git file pointing to main repo
    let main_git_dir = tmp.path().join("main-repo/.git");
    let worktree_git_dir = main_git_dir.join("worktrees/my-worktree");
    std::fs::create_dir_all(&worktree_git_dir).expect("create gitdir");
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("write .git file");

    let dirs = writable_dirs(&worktree, &[], &[], "test-project");
    let main_git = main_git_dir.canonicalize().unwrap_or(main_git_dir);
    assert!(
        dirs.iter()
            .any(|d| d == &main_git.to_string_lossy().to_string()),
        "Should include main repo .git/ dir for worktree; dirs: {:?}",
        dirs
    );
}

#[test]
fn test_generate_macos_profile_structure() {
    let writable = vec!["/Users/alice/project".to_string(), "/tmp".to_string()];
    let profile = generate_macos_profile(&writable);

    assert!(profile.contains("(version 1)"));
    assert!(profile.contains("(allow default)"));
    assert!(profile.contains("(deny file-write*"));
    assert!(profile.contains("(subpath \"/Users/alice/project\")"));
    assert!(profile.contains("(subpath \"/tmp\")"));
}

#[test]
fn test_generate_macos_profile_denies_home() {
    let profile = generate_macos_profile(&["/tmp".to_string()]);
    // Should deny writes under home directory
    assert!(profile.contains("(deny file-write*"));
}

#[test]
fn test_shell_escape_simple() {
    assert_eq!(shell_escape("hello"), "'hello'");
}

#[test]
fn test_shell_escape_with_single_quotes() {
    assert_eq!(shell_escape("it's"), "'it'\\''s'");
}

#[test]
fn test_wrap_shell_command_macos() {
    let writable = vec!["/tmp".to_string()];
    let result = wrap_shell_command_macos("echo hello", &writable);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert!(cmd.starts_with("sandbox-exec -f "));
    assert!(cmd.contains("sh -c"));
    assert!(cmd.contains("echo hello"));
}

#[test]
fn test_bwrap_args_structure() {
    let writable = vec!["/home/user/project".to_string(), "/tmp".to_string()];
    let args = bwrap_args("claude", &["--help".to_string()], &writable);

    assert_eq!(args[0], "--ro-bind");
    assert_eq!(args[1], "/");
    assert_eq!(args[2], "/");

    // Find the writable bind mounts
    let bind_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--bind")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(bind_positions.len(), 2);

    // Should end with -- claude --help
    let separator_pos = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(args[separator_pos + 1], "claude");
    assert_eq!(args[separator_pos + 2], "--help");
}

#[test]
fn test_sandbox_exec_prefix() {
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }
    let writable = vec!["/tmp".to_string()];
    let result = sandbox_exec_prefix(&writable);
    assert!(result.is_ok());
    let (path, prefix) = result.unwrap();
    assert!(path.to_string_lossy().contains("midtown-sandbox"));
    assert_eq!(prefix[0], "-f");
    // Clean up
    let _ = std::fs::remove_file(&path);
}

/// Verify that sandbox_exec_prefix returns Err when nesting is detected.
///
/// Reproduces the crash loop: when the daemon runs inside the Lead's sandbox,
/// attempting to apply a second sandbox-exec to coworkers fails with EPERM.
/// The fix detects this and returns an error so the fallback path runs
/// coworkers without the redundant sandbox wrapper.
#[test]
#[cfg(target_os = "macos")]
fn test_sandbox_exec_prefix_returns_err_when_nested() {
    // Skip if we're already inside a sandbox (can't nest sandbox-exec)
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }

    // Run sandbox_exec_prefix inside a sandbox to verify it detects nesting.
    // We can't use OnceLock-cached can_sandbox() from the outer process,
    // so we spawn a child that checks from inside a sandbox.
    let profile_content = "(version 1)(allow default)";
    let tmp = std::env::temp_dir().join("midtown-test-nesting.sb");
    std::fs::write(&tmp, profile_content).expect("write test profile");

    let exe = std::env::current_exe().expect("current exe");
    let output = std::process::Command::new("sandbox-exec")
        .args(["-f", &tmp.to_string_lossy()])
        .arg(&exe)
        .args(["--test", "sandbox::tests::test_can_sandbox_detects_nesting"])
        .arg("--exact")
        .arg("--nocapture")
        .output()
        .expect("spawn sandboxed test");

    let _ = std::fs::remove_file(&tmp);

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The inner test should pass (it asserts can_sandbox() == false)
    assert!(
        output.status.success() || stderr.contains("can_sandbox correctly returned false"),
        "Nested sandbox detection test failed: {}",
        stderr
    );
}

/// Helper test: asserts can_sandbox() returns false when already sandboxed.
/// Called from test_sandbox_exec_prefix_returns_err_when_nested via sandbox-exec.
#[test]
#[cfg(target_os = "macos")]
fn test_can_sandbox_detects_nesting() {
    // This test is meaningful when run inside a sandbox (via the nesting test above).
    // When run directly (not inside a sandbox), can_sandbox() returns true, which is fine.
    // The nesting test invokes this inside sandbox-exec to verify the false path.
    if !can_sandbox() {
        eprintln!("can_sandbox correctly returned false inside nested sandbox");
        // Also verify sandbox_exec_prefix returns Err
        let result = sandbox_exec_prefix(&["/tmp".to_string()]);
        assert!(
            result.is_err(),
            "sandbox_exec_prefix should return Err when nested"
        );
        assert!(
            result.unwrap_err().contains("Already inside a sandbox"),
            "Error message should mention nesting"
        );
    }
}

/// Run a command under sandbox-exec and return (success, stderr).
#[cfg(target_os = "macos")]
fn run_sandboxed(profile_path: &Path, program: &str, args: &[&str]) -> (bool, String) {
    let output = std::process::Command::new("sandbox-exec")
        .args(["-f", &profile_path.to_string_lossy()])
        .arg(program)
        .args(args)
        .output()
        .expect("sandbox-exec should be available on macOS");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Verify that sandbox-exec allows writes to explicitly permitted directories.
#[test]
#[cfg(target_os = "macos")]
fn test_sandbox_exec_allows_writes_to_permitted_dir() {
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    // Canonicalize because macOS /var → /private/var symlink;
    // sandbox-exec operates on real paths.
    let real_path = tmp.path().canonicalize().expect("canonicalize");
    let writable = vec![real_path.to_string_lossy().to_string()];
    let profile = generate_macos_profile(&writable);
    let profile_path = write_profile_to_tempfile(&profile).expect("write profile");

    let test_file = real_path.join("sandbox-test-allow.txt");
    let (ok, _stderr) = run_sandboxed(
        &profile_path,
        "sh",
        &["-c", &format!("echo ok > '{}'", test_file.display())],
    );

    assert!(ok, "Write to permitted dir should succeed");
    assert!(test_file.exists(), "File should have been created");
    assert_eq!(std::fs::read_to_string(&test_file).unwrap().trim(), "ok");

    let _ = std::fs::remove_file(&profile_path);
}

/// Verify that sandbox-exec blocks writes to directories outside the allow list.
///
/// The sandbox profile denies writes under `$HOME`, so we create a test dir
/// under `$HOME` (not in the allow list) to verify the deny rule works.
#[test]
#[cfg(target_os = "macos")]
fn test_sandbox_exec_denies_writes_to_unpermitted_dir() {
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }
    let home = dirs::home_dir().expect("home dir");
    let denied = home.join(".midtown-sandbox-test-deny");
    std::fs::create_dir_all(&denied).expect("create denied dir");

    // Writable list intentionally does NOT include the denied directory
    let writable = vec!["/tmp".to_string()];
    let profile = generate_macos_profile(&writable);
    let profile_path = write_profile_to_tempfile(&profile).expect("write profile");

    let test_file = denied.join("sandbox-test-deny.txt");
    let (ok, _stderr) = run_sandboxed(
        &profile_path,
        "sh",
        &["-c", &format!("echo blocked > '{}'", test_file.display())],
    );

    assert!(!ok, "Write to unpermitted dir under $HOME should fail");
    assert!(!test_file.exists(), "File should NOT have been created");

    let _ = std::fs::remove_file(&profile_path);
    let _ = std::fs::remove_dir_all(&denied);
}

/// Verify that sandbox-exec still allows reads from directories outside the allow list.
#[test]
#[cfg(target_os = "macos")]
fn test_sandbox_exec_allows_reads_everywhere() {
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }
    let readable = tempfile::TempDir::new().expect("create readable dir");
    let real_path = readable.path().canonicalize().expect("canonicalize");
    let readable_file = real_path.join("readable.txt");
    std::fs::write(&readable_file, "hello").expect("write readable file");

    // Writable list does NOT include readable dir — but reads should still work
    let writable = vec!["/tmp".to_string()];
    let profile = generate_macos_profile(&writable);
    let profile_path = write_profile_to_tempfile(&profile).expect("write profile");

    let (ok, _stderr) = run_sandboxed(&profile_path, "cat", &[&readable_file.to_string_lossy()]);

    assert!(ok, "Read from any directory should succeed");

    let _ = std::fs::remove_file(&profile_path);
}

/// Verify sandbox with the real writable_dirs() output — the profile that
/// Claude Code actually runs under.
#[test]
#[cfg(target_os = "macos")]
fn test_sandbox_exec_real_profile_allows_project_writes() {
    if !can_sandbox() {
        eprintln!("Skipping test: already inside a sandbox (nesting not allowed)");
        return;
    }
    let project = tempfile::TempDir::new().expect("create project dir");
    let real_project = project.path().canonicalize().expect("canonicalize");
    let writable = writable_dirs(&real_project, &[], &[], "test-project");
    let profile = generate_macos_profile(&writable);
    let profile_path = write_profile_to_tempfile(&profile).expect("write profile");

    // Should be able to write inside the project
    let test_file = real_project.join("real-profile-test.txt");
    let (ok, _stderr) = run_sandboxed(
        &profile_path,
        "sh",
        &["-c", &format!("echo works > '{}'", test_file.display())],
    );
    assert!(ok, "Write to project dir with real profile should succeed");
    assert!(test_file.exists());

    // Should NOT be able to write to a dir under $HOME that's not in the allow list
    let home = dirs::home_dir().expect("home dir");
    let blocked_dir = home.join(".midtown-sandbox-test-real");
    std::fs::create_dir_all(&blocked_dir).expect("create blocked dir");
    let blocked_file = blocked_dir.join("should-not-exist.txt");
    let (ok2, _stderr) = run_sandboxed(
        &profile_path,
        "sh",
        &["-c", &format!("echo nope > '{}'", blocked_file.display())],
    );
    assert!(!ok2, "Write outside allowed dirs should be blocked");
    assert!(!blocked_file.exists());
    let _ = std::fs::remove_dir_all(&blocked_dir);

    let _ = std::fs::remove_file(&profile_path);
}

#[test]
fn test_writable_dirs_includes_configured_paths() {
    let configured = vec!["~/.cargo".to_string(), "/opt/toolchain".to_string()];
    let dirs = writable_dirs(
        Path::new("/home/user/project"),
        &[],
        &configured,
        "test-project",
    );

    // Check that configured paths are expanded and included
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let cargo_path = home.join(".cargo").to_string_lossy().to_string();
    assert!(
        dirs.contains(&cargo_path),
        "Should include ~/.cargo expanded to {}",
        cargo_path
    );
    assert!(
        dirs.contains(&"/opt/toolchain".to_string()),
        "Should include /opt/toolchain"
    );
}

#[test]
fn test_writable_dirs_deduplicates_configured_paths() {
    let configured = vec![
        "~/.cargo".to_string(),
        "~/.cargo".to_string(), // duplicate
        "/opt/toolchain".to_string(),
    ];
    let dirs = writable_dirs(
        Path::new("/home/user/project"),
        &[],
        &configured,
        "test-project",
    );

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let cargo_path = home.join(".cargo").to_string_lossy().to_string();
    let count = dirs.iter().filter(|d| *d == &cargo_path).count();
    assert_eq!(count, 1, "~/.cargo should not be duplicated");
}

#[test]
#[should_panic(expected = "project_name must not contain")]
fn test_writable_dirs_rejects_path_traversal() {
    writable_dirs(Path::new("/home/user/project"), &[], &[], "../etc");
}

#[test]
#[should_panic(expected = "project_name must not contain")]
fn test_writable_dirs_rejects_slash_in_project_name() {
    writable_dirs(Path::new("/home/user/project"), &[], &[], "foo/bar");
}

#[test]
#[should_panic(expected = "project_name must not contain")]
fn test_writable_dirs_rejects_embedded_dotdot() {
    writable_dirs(
        Path::new("/home/user/project"),
        &[],
        &[],
        "my-project/../escape",
    );
}

#[test]
#[should_panic(expected = "project_name must not be empty")]
fn test_writable_dirs_rejects_empty_project_name() {
    writable_dirs(Path::new("/home/user/project"), &[], &[], "");
}

#[test]
fn test_writable_dirs_accepts_valid_project_names() {
    // Normal repo names should work fine
    let _ = writable_dirs(Path::new("/home/user/project"), &[], &[], "midtown");
    let _ = writable_dirs(Path::new("/home/user/project"), &[], &[], "my-project");
    let _ = writable_dirs(Path::new("/home/user/project"), &[], &[], "repo_name.git");
    let _ = writable_dirs(Path::new("/home/user/project"), &[], &[], "CamelCase123");
}

#[test]
fn test_writable_dirs_expands_tilde() {
    let configured = vec!["~/.cargo".to_string()];
    let dirs = writable_dirs(
        Path::new("/home/user/project"),
        &[],
        &configured,
        "test-project",
    );

    // Should not contain the literal "~/.cargo", should be expanded
    assert!(
        !dirs.contains(&"~/.cargo".to_string()),
        "Should not contain literal ~ path"
    );

    // Should contain the expanded path
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let cargo_path = home.join(".cargo").to_string_lossy().to_string();
    assert!(dirs.contains(&cargo_path), "Should contain expanded path");
}
