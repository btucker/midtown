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
/// - `~/.midtown/projects/<project>/` (project-scoped daemon state, channel logs, worktrees)
/// - `~/.midtown/auth/` (auth profile dirs used as CLAUDE_CONFIG_DIR / CODEX_HOME)
/// - `~/.midtown/platforms/` (shared platform state — symlink target for auth profile data)
/// - `~/.claude` (Claude Code config, sessions, tasks)
/// - `~/.codex` (Codex config)
/// - `~/.local/state/midtown/<project>/` (project-scoped daemon socket, runtime state)
/// - Main repo `.git/` directory (when primary_repo is a git worktree)
/// - `/tmp` and platform-specific temp directories
pub fn writable_dirs(
    primary_repo: &Path,
    additional_repos: &[PathBuf],
    configured_paths: &[String],
    project_name: &str,
) -> Vec<String> {
    // Defense-in-depth: validate project_name before using it in path construction.
    // In practice, detect_repo_name() uses .file_name() which strips path separators,
    // but this function is public and security-critical — validate independently.
    assert!(!project_name.is_empty(), "project_name must not be empty");
    assert!(
        !project_name.contains('/') && !project_name.contains('\\') && project_name != "..",
        "project_name must not contain path separators or be a traversal component, got: {project_name:?}"
    );

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

    // Midtown project-scoped state directory.
    // Only the current project's data is writable — prevents cross-project writes.
    // Global config (~/.midtown/config.toml) and agents (~/.midtown/agents/) remain
    // readable (sandbox only restricts writes).
    dirs.push(
        home.join(".midtown/projects")
            .join(project_name)
            .to_string_lossy()
            .to_string(),
    );
    // Auth profiles and shared platform state live under ~/.midtown/platforms/.
    // Profiles are set as CLAUDE_CONFIG_DIR or CODEX_HOME; Claude Code writes
    // session data, project settings, and tasks there — blocking writes causes
    // immediate process death. Shared entries (projects, plans, tasks) are
    // symlinked from profile dirs into the shared/ subdirectory.
    dirs.push(
        home.join(".midtown/platforms")
            .to_string_lossy()
            .to_string(),
    );
    // Legacy: Codex profiles still live under ~/.midtown/auth/providers/.
    // Remove this entry once all providers are migrated to the platforms/ layout.
    dirs.push(home.join(".midtown/auth").to_string_lossy().to_string());
    dirs.push(home.join(".claude").to_string_lossy().to_string());
    dirs.push(home.join(".codex").to_string_lossy().to_string());

    // XDG state directory — project-scoped (daemon socket, runtime state)
    dirs.push(
        home.join(".local/state/midtown")
            .join(project_name)
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

/// Wrap a shell command string with `sandbox-exec` for macOS.
///
/// Writes the SBPL profile to a temp file and returns a new shell command:
/// `sandbox-exec -f <profile> sh -c '<original_cmd>'`
///
/// The original command is single-quote escaped for safe embedding in `sh -c`.
pub fn wrap_shell_command_macos(cmd: &str, writable: &[String]) -> Result<String, String> {
    let profile = generate_macos_profile(writable);
    let profile_path = write_profile_to_tempfile(&profile)?;

    // The command string is already meant for `sh -c`, so we wrap
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

#[path = "sandbox_tests.rs"]
#[cfg(test)]
mod tests;
