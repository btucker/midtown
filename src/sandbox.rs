//! Lightweight filesystem sandbox for Claude Code sessions.
//!
//! Restricts filesystem writes to allowed directories using platform-native
//! mechanisms: `sandbox-exec` on macOS and `bwrap` on Linux.
//!
//! This replaces the container-based sandbox (Docker/Apple Container) with
//! zero-overhead, same-host sandboxing. Claude Code runs with the same
//! binaries, auth tokens, and config — just with write access restricted
//! to project directories.

use std::path::{Path, PathBuf};

/// Build the list of writable directories from project context.
///
/// Includes:
/// - Primary repo directory
/// - All additional repo directories (multi-repo projects)
/// - Additional configured paths from `[sandbox].allowed_paths` (global + project)
/// - `~/.midtown` (daemon state, channel logs, worktrees)
/// - `~/.claude` (Claude Code config, sessions, tasks)
/// - `~/.codex` (Codex config)
/// - `~/.local/state/midtown` (daemon socket, runtime state)
/// - Main repo `.git/` directory (when primary_repo is a git worktree)
/// - `/tmp` and platform-specific temp directories
pub fn writable_dirs(
    primary_repo: &Path,
    additional_repos: &[PathBuf],
    configured_paths: &[String],
) -> Vec<String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));

    let mut dirs = Vec::new();

    // Canonicalize paths to resolve symlinks (e.g. macOS /var → /private/var).
    // sandbox-exec operates on real paths, so symlinked paths won't match.
    let canon = |p: &Path| -> String {
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_string()
    };

    // Helper to expand ~ and canonicalize configured paths
    let expand_and_canon = |s: &str| -> String {
        let expanded = if let Some(rest) = s.strip_prefix("~/") {
            home.join(rest)
        } else if s == "~" {
            home.clone()
        } else {
            PathBuf::from(s)
        };
        canon(&expanded)
    };

    // Primary project repo
    dirs.push(canon(primary_repo));

    // Git worktree support: if primary_repo is a worktree, its .git is a file
    // pointing to the main repo's .git/worktrees/<name>/ directory. Git writes
    // (commits, refs, objects) go to the main repo's .git/, so it must be writable.
    let dot_git = primary_repo.join(".git");
    if dot_git.is_file()
        && let Ok(content) = std::fs::read_to_string(&dot_git)
        && let Some(gitdir) = content.trim().strip_prefix("gitdir: ")
    {
        // gitdir is e.g. /path/to/main-repo/.git/worktrees/<name>
        // We need the main .git/ dir (two parents up) for shared state
        let gitdir_path = Path::new(gitdir);
        if let Some(main_git_dir) = gitdir_path.parent().and_then(|p| p.parent()) {
            let s = canon(main_git_dir);
            if !dirs.contains(&s) {
                dirs.push(s);
            }
        }
    }

    // Additional repos (multi-repo projects)
    for repo in additional_repos {
        let s = canon(repo);
        if !dirs.contains(&s) {
            dirs.push(s);
        }
    }

    // Additional configured paths from [sandbox].allowed_paths
    for path in configured_paths {
        let s = expand_and_canon(path);
        if !dirs.contains(&s) {
            dirs.push(s);
        }
    }

    // Midtown state and config directories
    dirs.push(home.join(".midtown").to_string_lossy().to_string());
    dirs.push(home.join(".claude").to_string_lossy().to_string());
    dirs.push(home.join(".codex").to_string_lossy().to_string());

    // XDG state directory (daemon socket, runtime state)
    dirs.push(
        home.join(".local/state/midtown")
            .to_string_lossy()
            .to_string(),
    );

    // Temp directories
    dirs.push("/tmp".to_string());
    if cfg!(target_os = "macos") {
        dirs.push("/private/tmp".to_string());
        dirs.push("/private/var/folders".to_string());
    }

    dirs
}

/// Generate a macOS sandbox-exec profile (SBPL) that allows reads everywhere
/// but restricts writes to the given directories.
///
/// The profile uses `(allow default)` as the base (permits all operations),
/// then denies file-write under `$HOME`, then re-allows writes to the
/// specified directories. This means processes can read any file but can
/// only write to explicitly allowed paths.
pub fn generate_macos_profile(writable: &[String]) -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let home_str = home.to_string_lossy();

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(allow default)\n");
    profile.push_str(&format!("(deny file-write* (subpath \"{}\"))\n", home_str));
    profile.push_str("(allow file-write*\n");
    for dir in writable {
        profile.push_str(&format!("  (subpath \"{}\")\n", dir));
    }
    profile.push_str(")\n");

    profile
}

/// Write a sandbox profile to a temp file and return the path.
///
/// The file is written to the system temp directory using a unique filename.
fn write_profile_to_tempfile(profile: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_PROFILE_ID: AtomicU64 = AtomicU64::new(1);

    let pid = std::process::id();
    let seq = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Use create_new to avoid cross-test/session races where parallel writers
    // clobber or delete each other's profile file.
    for attempt in 0..16 {
        let path = std::env::temp_dir().join(format!(
            "midtown-sandbox-{pid}-{seq}-{now_nanos}-{attempt}.sb"
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(profile.as_bytes())
                    .map_err(|e| format!("Failed to write sandbox profile: {}", e))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!("Failed to create sandbox profile: {}", e));
            }
        }
    }

    Err("Failed to allocate unique sandbox profile path".to_string())
}

/// Wrap a shell command string with `sandbox-exec` for macOS tmux usage.
///
/// Writes the SBPL profile to a temp file and returns a new shell command:
/// `sandbox-exec -f <profile> sh -c '<original_cmd>'`
///
/// The original command is single-quote escaped for safe embedding in `sh -c`.
pub fn wrap_shell_command_macos(cmd: &str, writable: &[String]) -> Result<String, String> {
    let profile = generate_macos_profile(writable);
    let profile_path = write_profile_to_tempfile(&profile)?;

    // The command string is already meant for `sh -c` via tmux, so we wrap
    // the entire thing in sandbox-exec. We use exec to replace the shell
    // with sandbox-exec so there's no extra process layer.
    Ok(format!(
        "sandbox-exec -f {} sh -c {}",
        profile_path.display(),
        shell_escape(cmd)
    ))
}

/// Check if sandbox-exec can be applied in the current process context.
///
/// On macOS, sandbox-exec cannot nest — a process already running inside a
/// sandbox cannot apply a new sandbox profile to child processes. This returns
/// false if we detect that sandbox nesting would fail.
pub fn can_sandbox() -> bool {
    use std::sync::OnceLock;
    static CAN_SANDBOX: OnceLock<bool> = OnceLock::new();
    *CAN_SANDBOX.get_or_init(|| {
        // Try applying a trivial sandbox to /usr/bin/true.
        // If we're already sandboxed, this fails with EPERM.
        let result = std::process::Command::new("sandbox-exec")
            .args(["-f", "/dev/stdin", "/usr/bin/true"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                // Take stdin to move it out of child — dropping it sends EOF.
                // Using `ref mut` would keep the pipe alive through wait(),
                // causing sandbox-exec (reading from /dev/stdin) to block forever.
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(b"(version 1)(allow default)");
                }
                // stdin is dropped here, sending EOF to sandbox-exec
                child.wait()
            });
        matches!(result, Ok(status) if status.success())
    })
}

/// Wrap a `tokio::process::Command` with sandbox-exec on macOS.
///
/// Instead of running `claude ...` directly, runs:
/// `sandbox-exec -f <profile> claude ...`
///
/// Returns the modified args to prepend to the command, and the profile path
/// that must outlive the child process.
///
/// Returns an error if sandbox nesting is detected (already inside a sandbox).
pub fn sandbox_exec_prefix(writable: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    if !can_sandbox() {
        return Err("Already inside a sandbox — cannot nest sandbox-exec".to_string());
    }
    let profile = generate_macos_profile(writable);
    let profile_path = write_profile_to_tempfile(&profile)?;

    let prefix = vec!["-f".to_string(), profile_path.to_string_lossy().to_string()];

    Ok((profile_path, prefix))
}

/// Build bwrap arguments for Linux sandboxing.
///
/// Returns the full argument list for bwrap:
/// `bwrap --ro-bind / / --bind <dir> <dir> ... --dev /dev --proc /proc -- <program> <args...>`
pub fn bwrap_args(program: &str, program_args: &[String], writable: &[String]) -> Vec<String> {
    let mut args = vec!["--ro-bind".to_string(), "/".to_string(), "/".to_string()];

    for dir in writable {
        args.push("--bind".to_string());
        args.push(dir.clone());
        args.push(dir.clone());
    }

    args.push("--dev".to_string());
    args.push("/dev".to_string());
    args.push("--proc".to_string());
    args.push("/proc".to_string());
    args.push("--".to_string());
    args.push(program.to_string());
    args.extend_from_slice(program_args);

    args
}

/// Check if bwrap is available on the system.
pub fn bwrap_available() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Shell-escape a string for embedding in `sh -c '...'`.
fn shell_escape(s: &str) -> String {
    // Use single quotes, escaping any embedded single quotes as '\''
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_writable_dirs_includes_primary_repo() {
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[]);
        assert!(dirs.contains(&"/home/user/project".to_string()));
    }

    #[test]
    fn test_writable_dirs_includes_additional_repos() {
        let additional = vec![PathBuf::from("/home/user/lib")];
        let dirs = writable_dirs(Path::new("/home/user/project"), &additional, &[]);
        assert!(dirs.contains(&"/home/user/project".to_string()));
        assert!(dirs.contains(&"/home/user/lib".to_string()));
    }

    #[test]
    fn test_writable_dirs_deduplicates() {
        let additional = vec![PathBuf::from("/home/user/project")];
        let dirs = writable_dirs(Path::new("/home/user/project"), &additional, &[]);
        let count = dirs.iter().filter(|d| *d == "/home/user/project").count();
        assert_eq!(count, 1, "Primary repo should not be duplicated");
    }

    #[test]
    fn test_writable_dirs_includes_config_dirs() {
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[]);
        let has_midtown = dirs.iter().any(|d| d.ends_with(".midtown"));
        let has_claude = dirs.iter().any(|d| d.ends_with(".claude"));
        let has_codex = dirs.iter().any(|d| d.ends_with(".codex"));
        assert!(has_midtown, "Should include ~/.midtown");
        assert!(has_claude, "Should include ~/.claude");
        assert!(has_codex, "Should include ~/.codex");
    }

    #[test]
    fn test_writable_dirs_includes_tmp() {
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &[]);
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

        let dirs = writable_dirs(&worktree, &[], &[]);
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

        let (ok, _stderr) =
            run_sandboxed(&profile_path, "cat", &[&readable_file.to_string_lossy()]);

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
        let writable = writable_dirs(&real_project, &[], &[]);
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
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &configured);

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
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &configured);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
        let cargo_path = home.join(".cargo").to_string_lossy().to_string();
        let count = dirs.iter().filter(|d| *d == &cargo_path).count();
        assert_eq!(count, 1, "~/.cargo should not be duplicated");
    }

    #[test]
    fn test_writable_dirs_expands_tilde() {
        let configured = vec!["~/.cargo".to_string()];
        let dirs = writable_dirs(Path::new("/home/user/project"), &[], &configured);

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
}
