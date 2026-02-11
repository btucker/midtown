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
/// - `~/.midtown` (daemon state, channel logs, worktrees)
/// - `~/.claude` (Claude Code config, sessions, tasks)
/// - `~/.codex` (Codex config)
/// - `/tmp` and platform-specific temp directories
pub fn writable_dirs(primary_repo: &Path, additional_repos: &[PathBuf]) -> Vec<String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));

    let mut dirs = Vec::new();

    // Primary project repo
    dirs.push(primary_repo.to_string_lossy().to_string());

    // Additional repos (multi-repo projects)
    for repo in additional_repos {
        let s = repo.to_string_lossy().to_string();
        if !dirs.contains(&s) {
            dirs.push(s);
        }
    }

    // Midtown state and config directories
    dirs.push(home.join(".midtown").to_string_lossy().to_string());
    dirs.push(home.join(".claude").to_string_lossy().to_string());
    dirs.push(home.join(".codex").to_string_lossy().to_string());

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
/// The file is written to `/tmp/midtown-sandbox-<pid>.sb` so it persists
/// for the lifetime of the calling process.
fn write_profile_to_tempfile(profile: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("midtown-sandbox-{}.sb", std::process::id()));
    std::fs::write(&path, profile)
        .map_err(|e| format!("Failed to write sandbox profile: {}", e))?;
    Ok(path)
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

/// Wrap a `tokio::process::Command` with sandbox-exec on macOS.
///
/// Instead of running `claude ...` directly, runs:
/// `sandbox-exec -f <profile> claude ...`
///
/// Returns the modified args to prepend to the command, and the profile path
/// that must outlive the child process.
pub fn sandbox_exec_prefix(writable: &[String]) -> Result<(PathBuf, Vec<String>), String> {
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
        let dirs = writable_dirs(Path::new("/home/user/project"), &[]);
        assert!(dirs.contains(&"/home/user/project".to_string()));
    }

    #[test]
    fn test_writable_dirs_includes_additional_repos() {
        let additional = vec![PathBuf::from("/home/user/lib")];
        let dirs = writable_dirs(Path::new("/home/user/project"), &additional);
        assert!(dirs.contains(&"/home/user/project".to_string()));
        assert!(dirs.contains(&"/home/user/lib".to_string()));
    }

    #[test]
    fn test_writable_dirs_deduplicates() {
        let additional = vec![PathBuf::from("/home/user/project")];
        let dirs = writable_dirs(Path::new("/home/user/project"), &additional);
        let count = dirs.iter().filter(|d| *d == "/home/user/project").count();
        assert_eq!(count, 1, "Primary repo should not be duplicated");
    }

    #[test]
    fn test_writable_dirs_includes_config_dirs() {
        let dirs = writable_dirs(Path::new("/home/user/project"), &[]);
        let has_midtown = dirs.iter().any(|d| d.ends_with(".midtown"));
        let has_claude = dirs.iter().any(|d| d.ends_with(".claude"));
        let has_codex = dirs.iter().any(|d| d.ends_with(".codex"));
        assert!(has_midtown, "Should include ~/.midtown");
        assert!(has_claude, "Should include ~/.claude");
        assert!(has_codex, "Should include ~/.codex");
    }

    #[test]
    fn test_writable_dirs_includes_tmp() {
        let dirs = writable_dirs(Path::new("/home/user/project"), &[]);
        assert!(dirs.contains(&"/tmp".to_string()));
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
        let writable = vec!["/tmp".to_string()];
        let result = sandbox_exec_prefix(&writable);
        assert!(result.is_ok());
        let (path, prefix) = result.unwrap();
        assert!(path.to_string_lossy().contains("midtown-sandbox"));
        assert_eq!(prefix[0], "-f");
        // Clean up
        let _ = std::fs::remove_file(&path);
    }
}
