//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::cli::Response;

/// Validate that a project name contains only safe characters.
///
/// Allowed: alphanumeric, hyphens, underscores, dots.
/// This prevents shell injection when the name is embedded in shell commands
/// (e.g., `export CLAUDE_CODE_TASK_LIST_ID='midtown-{name}'`).
fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(format!(
            "Invalid project name '{}': only alphanumeric characters, hyphens, underscores, and dots are allowed",
            name
        ));
    }
    Ok(())
}

/// Resolve the project name from an explicit flag, config, or repo directory name.
///
/// Priority: explicit flag > config.toml `[project].name` > git repo directory name.
/// Validates user-provided names to prevent shell injection.
fn resolve_project_name(project: &Option<String>) -> Option<String> {
    if let Some(name) = project {
        return Some(name.clone());
    }
    midtown::paths::detect_project_name().or_else(|| {
        repo_root()
            .ok()
            .and_then(|r| r.file_name().map(|s| s.to_string_lossy().to_string()))
    })
}

/// Get the tmux session name for an explicit or inferred project.
/// Format: midtown-{project_name}
///
/// Returns an error if no project name can be determined.
fn session_name_for(project: &Option<String>) -> Result<String, String> {
    match resolve_project_name(project) {
        Some(name) => Ok(format!("midtown-{}", name)),
        None => Err(
            "Not in a git repository. Run midtown from within a git repo or use --repo."
                .to_string(),
        ),
    }
}

/// Get the tmux session name based on the project name.
/// Format: midtown-{project_name}
///
/// The project name is resolved from config.toml `[project].name`,
/// falling back to the git repo directory name.
///
/// Returns an error if not in a git repository, since a tmux session
/// requires a valid project context.
fn session_name() -> Result<String, String> {
    session_name_for(&None)
}

/// Get the socket path for the daemon.
fn socket_path() -> PathBuf {
    midtown::paths::daemon_socket()
}

/// Check if the Claude CLI is installed and executable.
fn claude_cli_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The official Claude plugins marketplace on GitHub.
const OFFICIAL_MARKETPLACE: &str = "anthropics/claude-plugins-official";
const OFFICIAL_MARKETPLACE_NAME: &str = "claude-plugins-official";
const SANDBOX_ENTRY_ENV: &str = "MIDTOWN_IN_SANDBOX_CONTAINER";
const SANDBOX_IMAGE_ENV: &str = "MIDTOWN_SANDBOX_IMAGE";
const DEFAULT_SANDBOX_IMAGE: &str = "ghcr.io/btucker/midtown:latest";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SandboxEngine {
    AppleContainer,
    Docker,
}

impl SandboxEngine {
    fn binary(self) -> &'static str {
        match self {
            SandboxEngine::AppleContainer => "container",
            SandboxEngine::Docker => "docker",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            SandboxEngine::AppleContainer => "Apple container",
            SandboxEngine::Docker => "Docker",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SandboxRuntimeState {
    engine: SandboxEngine,
    container_name: String,
}

fn running_inside_sandbox() -> bool {
    std::env::var(SANDBOX_ENTRY_ENV).ok().as_deref() == Some("1")
}

fn sandbox_runtime_file_for_repo(repo: &str) -> PathBuf {
    midtown::paths::projects_dir_for_repo(repo).join("sandbox-runtime.json")
}

fn write_sandbox_runtime_state(repo: &str, state: &SandboxRuntimeState) {
    let path = sandbox_runtime_file_for_repo(repo);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

fn clear_sandbox_runtime_state(repo: &str) {
    let _ = std::fs::remove_file(sandbox_runtime_file_for_repo(repo));
}

fn load_sandbox_runtime_state_for_current_repo() -> Option<SandboxRuntimeState> {
    let repo = midtown::paths::detect_repo_name()?;
    let path = sandbox_runtime_file_for_repo(&repo);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<SandboxRuntimeState>(&data).ok()
}

fn command_in_path(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(command);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

fn apple_container_is_running() -> bool {
    // Check if Apple container system is already running without starting it
    Command::new("container")
        .args(["list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ensure_apple_container_system_started() -> Result<(), String> {
    let status = Command::new("container")
        .args(["system", "start"])
        .status()
        .map_err(|e| format!("Failed to run 'container system start': {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(
            "'container system start' failed. Install and initialize Apple container runtime."
                .to_string(),
        )
    }
}

fn docker_is_running() -> bool {
    // Check if Docker is already running
    command_in_path("docker")
        && Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

fn docker_is_usable() -> Result<(), String> {
    if !command_in_path("docker") {
        return Err("docker command not found".to_string());
    }
    let status = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to run 'docker info': {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err("Docker is installed but not running. Start Docker Desktop and retry.".to_string())
    }
}

fn sanitize_container_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

fn sandbox_container_name(project_name: &str) -> String {
    let slug = sanitize_container_name(project_name);
    if slug.is_empty() {
        "midtown-sandbox-default".to_string()
    } else {
        format!("midtown-sandbox-{}", slug)
    }
}

fn sandbox_image_name() -> String {
    std::env::var(SANDBOX_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_SANDBOX_IMAGE.to_string())
}

fn host_uid_gid() -> Option<(String, String)> {
    let uid = Command::new("id").arg("-u").output().ok()?;
    let gid = Command::new("id").arg("-g").output().ok()?;
    if !uid.status.success() || !gid.status.success() {
        return None;
    }
    Some((
        String::from_utf8_lossy(&uid.stdout).trim().to_string(),
        String::from_utf8_lossy(&gid.stdout).trim().to_string(),
    ))
}

fn collect_mounts(home: &Path, primary_repo: &Path, repos: &[PathBuf]) -> Vec<String> {
    let mut mounts = vec![home.to_string_lossy().to_string()];
    let mut add_mount = |path: &Path| {
        let s = path.to_string_lossy().to_string();
        if !mounts.contains(&s) {
            mounts.push(s);
        }
    };
    add_mount(primary_repo);
    for repo in repos {
        add_mount(repo);
    }
    mounts
}

fn engine_exec_success(engine: SandboxEngine, container_name: &str, command: &str) -> bool {
    Command::new(engine.binary())
        .args(["exec", container_name, "sh", "-lc", command])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn engine_start_container(engine: SandboxEngine, container_name: &str) -> bool {
    Command::new(engine.binary())
        .args(["start", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn engine_remove_container(engine: SandboxEngine, container_name: &str) {
    let _ = Command::new(engine.binary())
        .args(["rm", "-f", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn engine_run_sandbox_container(
    engine: SandboxEngine,
    container_name: &str,
    image: &str,
    working_dir: &Path,
    mounts: &[String],
    home: &Path,
) -> Result<(), String> {
    let mut args: Vec<String> = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "-w".to_string(),
        working_dir.to_string_lossy().to_string(),
        "-e".to_string(),
        format!("HOME={}", home.display()),
        "-e".to_string(),
        format!("XDG_STATE_HOME={}", home.join(".local/state").display()),
        "-e".to_string(),
        "MIDTOWN_SANDBOX_CONTAINER=1".to_string(),
    ];

    if matches!(engine, SandboxEngine::Docker)
        && let Some((uid, gid)) = host_uid_gid()
    {
        args.push("--user".to_string());
        args.push(format!("{}:{}", uid, gid));
    }

    for mount in mounts {
        args.push("-v".to_string());
        args.push(format!("{}:{}", mount, mount));
    }

    args.push(image.to_string());
    args.push("sleep".to_string());
    args.push("infinity".to_string());

    let output = Command::new(engine.binary())
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {} container: {}", engine.display_name(), e))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "{} run failed: {} {}",
        engine.display_name(),
        stderr.trim(),
        stdout.trim()
    ))
}

fn ensure_sandbox_container_running(
    engine: SandboxEngine,
    container_name: &str,
    image: &str,
    working_dir: &Path,
    mounts: &[String],
    home: &Path,
) -> Result<(), String> {
    if engine_exec_success(engine, container_name, "true") {
        return Ok(());
    }

    if engine_start_container(engine, container_name)
        && engine_exec_success(engine, container_name, "true")
    {
        return Ok(());
    }

    match engine_run_sandbox_container(engine, container_name, image, working_dir, mounts, home) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            engine_remove_container(engine, container_name);
            engine_run_sandbox_container(engine, container_name, image, working_dir, mounts, home)
                .map_err(|second_err| format!("{}; {}", first_err, second_err))
        }
    }
}

fn run_midtown_command_in_sandbox(
    runtime: &SandboxRuntimeState,
    working_dir: &Path,
    args: &[String],
) -> Result<Response, String> {
    let mut cmd_args: Vec<String> = vec![
        "exec".to_string(),
        "-e".to_string(),
        format!("{}=1", SANDBOX_ENTRY_ENV),
        "-w".to_string(),
        working_dir.to_string_lossy().to_string(),
        runtime.container_name.clone(),
        "midtown".to_string(),
    ];
    cmd_args.extend(args.to_vec());

    let output = Command::new(runtime.engine.binary())
        .args(&cmd_args)
        .output()
        .map_err(|e| {
            format!(
                "Failed to execute midtown inside {} sandbox: {}",
                runtime.engine.display_name(),
                e
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Sandbox command failed ({}): {} {}",
            runtime.engine.display_name(),
            stderr.trim(),
            stdout.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<Response>(stdout.trim()) {
        Ok(response) => Ok(response),
        Err(_) => Ok(Response::Message {
            message: stdout.trim().to_string(),
        }),
    }
}

fn exec_midtown_command_in_sandbox(
    runtime: &SandboxRuntimeState,
    working_dir: &Path,
    args: &[String],
) -> Result<Response, String> {
    let mut cmd = Command::new(runtime.engine.binary());
    cmd.arg("exec")
        .arg("-it")
        .arg("-e")
        .arg(format!("{}=1", SANDBOX_ENTRY_ENV))
        .arg("-w")
        .arg(working_dir.to_string_lossy().to_string())
        .arg(&runtime.container_name)
        .arg("midtown");
    for arg in args {
        cmd.arg(arg);
    }

    let err = cmd.exec();
    Err(format!(
        "Failed to exec into {} sandbox: {}",
        runtime.engine.display_name(),
        err
    ))
}

/// Ensure the official marketplace is configured and required plugins are installed.
fn ensure_plugins_installed() -> Result<(), String> {
    use midtown::daemon::REQUIRED_PLUGINS;

    if REQUIRED_PLUGINS.is_empty() {
        return Ok(());
    }

    // First ensure marketplace is configured
    ensure_marketplace_configured()?;

    // Get list of installed plugins
    let installed = get_installed_plugins()?;

    // Find missing plugins
    let missing: Vec<_> = REQUIRED_PLUGINS
        .iter()
        .filter(|p| !installed.contains(**p))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    eprintln!("Installing {} required plugins...", missing.len());

    // Install missing plugins
    for plugin in missing {
        eprint!("  Installing {}... ", plugin);
        match install_plugin(plugin) {
            Ok(()) => eprintln!("done"),
            Err(e) => eprintln!("failed: {}", e),
        }
    }

    Ok(())
}

/// Ensure the official Claude plugins marketplace is configured.
fn ensure_marketplace_configured() -> Result<(), String> {
    let output = Command::new("claude")
        .args(["plugin", "marketplace", "list"])
        .output()
        .map_err(|e| format!("Failed to run claude plugin marketplace list: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check if official marketplace is in the list
    if stdout.contains(OFFICIAL_MARKETPLACE_NAME) {
        return Ok(());
    }

    // Add the official marketplace
    eprintln!("Adding official Claude plugins marketplace...");
    let output = Command::new("claude")
        .args(["plugin", "marketplace", "add", OFFICIAL_MARKETPLACE])
        .output()
        .map_err(|e| format!("Failed to run claude plugin marketplace add: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to add marketplace: {}", stderr));
    }

    Ok(())
}

/// Get list of installed plugin IDs.
fn get_installed_plugins() -> Result<std::collections::HashSet<String>, String> {
    let output = Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .map_err(|e| format!("Failed to run claude plugin list: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude plugin list failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output - it's an array of objects with "id" field
    let plugins: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse plugin list JSON: {}", e))?;

    let ids: std::collections::HashSet<String> = plugins
        .iter()
        .filter_map(|p| p.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    Ok(ids)
}

/// Install a plugin by name.
fn install_plugin(name: &str) -> Result<(), String> {
    let output = Command::new("claude")
        .args(["plugin", "install", name])
        .output()
        .map_err(|e| format!("Failed to run claude plugin install: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    Ok(())
}

/// Check if the daemon is running by attempting to connect to its socket.
fn daemon_is_running() -> bool {
    let path = socket_path();
    if !path.exists() {
        return false;
    }
    // Try to connect - if successful, daemon is running
    UnixStream::connect(&path).is_ok()
}

/// Wait for the daemon socket to become available with retries.
///
/// Polls the socket every `interval_ms` milliseconds, up to `max_attempts` times.
/// Returns true if the socket became available, false if we timed out.
fn wait_for_daemon_socket(max_attempts: u32, interval_ms: u64) -> bool {
    for _ in 0..max_attempts {
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        if daemon_is_running() {
            return true;
        }
    }
    false
}

/// Clean up stale daemon state before starting a new daemon.
///
/// This handles the case where a previous daemon crashed or was killed
/// without cleaning up its PID file. It reads the PID file, checks if
/// that process is still running, and kills it if so.
fn cleanup_stale_daemon() {
    let pid_path = midtown::paths::daemon_pid_file();

    // Read the PID file if it exists
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => return, // No PID file, nothing to clean up
    };

    // Parse the PID
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            // Invalid PID file, remove it
            let _ = std::fs::remove_file(&pid_path);
            return;
        }
    };

    // Check if the process is still running using kill -0 (suppress stderr)
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status();

    if status.map(|s| s.success()).unwrap_or(false) {
        // Process is still running, try to kill it gracefully
        eprintln!("Cleaning up stale daemon process (PID {})", pid);
        let _ = Command::new("kill")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status();

        // Wait briefly for it to exit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Force kill if still running
        let still_running = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if still_running {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
    }

    // Clean up stale files
    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(socket_path());
}

/// Check if the project's tmux session exists.
fn session_exists(session: &str) -> bool {
    let output = Command::new("tmux")
        .args(["has-session", "-t", session])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Find the git repository root by walking up the directory tree.
///
/// Looks for a `.git` directory or file (worktrees use a file) starting
/// from the current directory and climbing up until found or hitting root.
fn find_git_root() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;

    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            // Found it - could be a directory (normal repo) or file (worktree)
            return Some(current);
        }

        // Try parent directory
        if !current.pop() {
            // Reached filesystem root
            return None;
        }
    }
}

/// Get the repository root directory.
///
/// Uses git rev-parse for accuracy (handles worktrees correctly),
/// but falls back to manual detection if git command fails.
fn repo_root() -> Result<PathBuf, String> {
    // First, check if we're even in a git repo by walking up
    if find_git_root().is_none() {
        return Err(
            "Not in a git repository. Run midtown from within a git repo, or use --repo <path>."
                .to_string(),
        );
    }

    // Use git rev-parse for accurate root detection (handles worktrees)
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(
            "Not in a git repository. Run midtown from within a git repo, or use --repo <path>."
                .to_string(),
        );
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();

    Ok(PathBuf::from(path))
}

/// Get the path to the Lead session ID file for a project.
fn lead_session_file(repo: &Path) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s: &std::ffi::OsStr| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    midtown::paths::lead_session_file_for_repo(&repo_name)
}

/// Resolve additional repo paths from CLI flags, falling back to saved config.
///
/// If repos are provided on the CLI, they are returned directly.
/// Otherwise, reads saved repos from the project's config.toml.
fn resolve_repos(repos: &[PathBuf], project_name: &str) -> Vec<PathBuf> {
    if !repos.is_empty() {
        return repos.to_vec();
    }
    parse_saved_repos(project_name)
}

/// Parse saved repos from a project's config.toml.
///
/// Reads the `[project].repos` list and returns all entries
/// except the primary repo (which is handled separately).
fn parse_saved_repos(project_name: &str) -> Vec<PathBuf> {
    let full_config = midtown::config::load_full_project_config(project_name);
    match full_config {
        Some(config) => {
            let primary = config.project.primary_repo().map(|s| s.to_string());
            config
                .project
                .repos()
                .into_iter()
                .filter(|r| Some(r.to_string()) != primary)
                .map(PathBuf::from)
                .collect()
        }
        None => vec![],
    }
}

/// Update the project config.toml with project name, primary repo, and additional repos.
fn update_project_config(
    project_name: &str,
    primary_repo: &Path,
    additional_repos: &[PathBuf],
) -> Result<(), String> {
    let config_path = midtown::config::project_config_path(project_name);
    let mut config =
        midtown::config::FullProjectConfig::load_from(&config_path).unwrap_or_default();

    // Set project name
    config.project.name = Some(project_name.to_string());

    // Set primary repo
    let primary_str = primary_repo.to_string_lossy().to_string();
    config.project.primary_repo = Some(primary_str.clone());

    // Build full repos list: primary + additional
    let mut all_repos = vec![primary_str];
    for r in additional_repos {
        let s = r.to_string_lossy().to_string();
        if !all_repos.contains(&s) {
            all_repos.push(s);
        }
    }
    config.project.repos = all_repos;

    config
        .save_to(&config_path)
        .map_err(|e| format!("Failed to save project config: {}", e))
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates tmux session for the project
/// 3. Launches Claude Code with Lead config in that session
pub fn handle_start(
    daemon_only: bool,
    dangerously_run_without_sandbox: bool,
    project: Option<String>,
    repos: Vec<PathBuf>,
) -> Result<Response, String> {
    // Validate explicit project name if provided
    if let Some(ref name) = project {
        validate_project_name(name)?;
    }

    // Verify we're in a git repo first
    let primary_repo = repo_root()?;
    let project_name = resolve_project_name(&project).unwrap_or_else(|| {
        primary_repo
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });
    let repo_name = primary_repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let additional_repos = resolve_repos(&repos, &project_name);
    let session = session_name_for(&Some(project_name.clone()))?;

    // On macOS, require sandboxed start by default unless we're already inside
    // the sandbox container or the user explicitly opts out.
    if cfg!(target_os = "macos") && !running_inside_sandbox() {
        let home = dirs::home_dir().ok_or("Failed to determine home directory")?;
        let image = sandbox_image_name();
        let container_name = sandbox_container_name(&project_name);
        let mounts = collect_mounts(&home, &primary_repo, &repos);
        let build_start_args = || {
            let mut start_args = vec!["--format".to_string(), "json".to_string()];
            start_args.push("start".to_string());
            if daemon_only {
                start_args.push("--daemon-only".to_string());
            }
            if let Some(ref p) = project {
                start_args.push("--project".to_string());
                start_args.push(p.clone());
            }
            for repo in &repos {
                start_args.push("--add-repo".to_string());
                start_args.push(repo.to_string_lossy().to_string());
            }
            start_args
        };

        // Determine which container runtimes to try and in what order.
        // Prefer whichever is already running to avoid unnecessary starts.
        let docker_running = docker_is_running();
        let apple_running = command_in_path("container") && apple_container_is_running();

        let engines_to_try: Vec<SandboxEngine> = if docker_running && !apple_running {
            // Docker is running, try it first
            vec![SandboxEngine::Docker, SandboxEngine::AppleContainer]
        } else if apple_running && !docker_running {
            // Apple container is running, try it first
            vec![SandboxEngine::AppleContainer, SandboxEngine::Docker]
        } else {
            // Either both are running or neither is running - use default preference order
            vec![SandboxEngine::AppleContainer, SandboxEngine::Docker]
        };

        let mut errors = Vec::new();
        for engine in engines_to_try {
            let runtime = SandboxRuntimeState {
                engine,
                container_name: container_name.clone(),
            };

            let result = match engine {
                SandboxEngine::AppleContainer => {
                    if !command_in_path("container") {
                        Err("container command not found".to_string())
                    } else {
                        ensure_apple_container_system_started().and_then(|_| {
                            ensure_sandbox_container_running(
                                runtime.engine,
                                &runtime.container_name,
                                &image,
                                &primary_repo,
                                &mounts,
                                &home,
                            )
                        })
                    }
                }
                SandboxEngine::Docker => {
                    if !command_in_path("docker") {
                        Err("docker command not found".to_string())
                    } else {
                        docker_is_usable().and_then(|_| {
                            ensure_sandbox_container_running(
                                runtime.engine,
                                &runtime.container_name,
                                &image,
                                &primary_repo,
                                &mounts,
                                &home,
                            )
                        })
                    }
                }
            };

            match result {
                Ok(()) => {
                    let start_args = build_start_args();
                    let inner =
                        run_midtown_command_in_sandbox(&runtime, &primary_repo, &start_args)?;
                    write_sandbox_runtime_state(&repo_name, &runtime);
                    return Ok(match inner {
                        Response::Message { message } => Response::Message {
                            message: format!(
                                "{}. Sandbox: {} ({})",
                                message,
                                runtime.engine.display_name(),
                                runtime.container_name
                            ),
                        },
                        other => other,
                    });
                }
                Err(e) => errors.push((engine, e)),
            }
        }

        let apple_error = errors
            .iter()
            .find(|(e, _)| *e == SandboxEngine::AppleContainer)
            .map(|(_, err)| err.clone())
            .unwrap_or_else(|| "not attempted".to_string());

        let docker_error = errors
            .iter()
            .find(|(e, _)| *e == SandboxEngine::Docker)
            .map(|(_, err)| err.clone())
            .unwrap_or_else(|| "not attempted".to_string());

        if dangerously_run_without_sandbox {
            clear_sandbox_runtime_state(&repo_name);
            eprintln!(
                "Warning: Starting without sandbox because no container runtime was usable.\n\
                 Apple container: {}\n\
                 Docker: {}",
                apple_error, docker_error
            );
        } else {
            return Err(format!(
                "No usable container runtime found on macOS.\n\
                 Apple container: {}\n\
                 Docker: {}\n\
                 Install Apple container (https://github.com/apple/container) or Docker, \
                 or rerun with --dangerously-run-without-sandbox.",
                apple_error, docker_error
            ));
        }
    }

    // Verify Claude CLI is installed (unless using a stub command or daemon-only mode)
    if !daemon_only && std::env::var("MIDTOWN_LEAD_COMMAND").is_err() && !claude_cli_available() {
        return Err(
            "Claude CLI is not installed. Install it with: curl -fsSL https://claude.ai/install.sh | bash"
                .to_string(),
        );
    }

    // Ensure required plugins are installed (unless using a stub command)
    if std::env::var("MIDTOWN_LEAD_COMMAND").is_err() {
        ensure_plugins_installed()?;
    }

    let mut messages = Vec::new();

    // Update project config with repo information
    let _ = update_project_config(&project_name, &primary_repo, &additional_repos);

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
    } else {
        // Clean up any stale PID file or orphaned daemon before starting
        cleanup_stale_daemon();

        // Start the daemon in the background using `midtown daemon`
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable: {}", e))?;

        let mut cmd = Command::new(&exe);
        cmd.arg("daemon");
        cmd.current_dir(&primary_repo);
        cmd.arg("--workdir").arg(&primary_repo);
        if project.is_some() {
            cmd.arg("--project").arg(&project_name);
        }

        // Spawn detached
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        cmd.spawn()
            .map_err(|e| format!("Failed to start daemon: {}", e))?;

        // Wait for daemon to start, polling the socket with retries.
        // The daemon startup includes plugin checking and gh CLI auth which
        // can take several seconds, so we use a generous timeout (15s total).
        // In containerized environments, startup can be even slower.
        let started = wait_for_daemon_socket(75, 200);

        if started {
            messages.push("Started daemon".to_string());
        } else {
            return Err("Daemon failed to start".to_string());
        }
    }

    // Step 2: Launch tmux session (unless --daemon-only)
    if daemon_only {
        messages.push("Skipping tmux session (--daemon-only)".to_string());
    } else if session_exists(&session) {
        messages.push(format!("Session '{}' already exists", session));
    } else {
        // Get project name for status bar (uppercase)
        let display_name = project_name.to_uppercase();

        // Create empty tmux session (no command — spawn_lead creates the window)
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-c",
                &primary_repo.to_string_lossy(),
            ])
            .status()
            .map_err(|e| format!("Failed to create session: {}", e))?;

        if !status.success() {
            return Err(format!("Failed to create session '{}'", session));
        }

        // Use spawn_lead() to create the Lead window with proper config,
        // auth profile, settings, and system prompt.
        midtown::tmux::spawn_lead(
            &session,
            &primary_repo.to_string_lossy(),
            &project_name,
            &additional_repos,
        )
        .map_err(|e| format!("Failed to spawn lead: {}", e))?;

        // Kill the default empty window created by new-session (window 0)
        // spawn_lead creates its own "lead" window, so the default is redundant.
        let _ = Command::new("tmux")
            .args(["kill-window", "-t", &format!("{}:0", session)])
            .status();

        // Configure tmux session-level options (status bar, titles, passthrough)
        let _ = Command::new("tmux")
            .args(["set-option", "-t", &session, "allow-passthrough", "on"])
            .status();

        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "status-style",
                "bg=colour236,fg=yellow",
            ])
            .status();

        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "status-left",
                &format!(" {} ", display_name),
            ])
            .status();

        let _ = Command::new("tmux")
            .args(["set-option", "-t", &session, "set-titles", "on"])
            .status();
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-t",
                &session,
                "set-titles-string",
                &format!("Midtown: {}", display_name),
            ])
            .status();

        let _ = midtown::tmux::setup_status_bar_hook(&session);

        // Set up chat TUI (split pane or separate window based on config)
        midtown::tmux::setup_chat_pane(&session);

        // Write marker file indicating Lead was initialized by midtown
        let marker_path = lead_initialized_marker(&primary_repo);
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

        messages.push(format!("Started Lead session in '{}'", session));
    }

    // Step 3: Auto-launch shared webserver if not running
    if !webserver_is_running() {
        match launch_webserver() {
            Ok(()) => messages.push(format!(
                "Started webserver on http://localhost:{}",
                midtown::webserver::DEFAULT_WEBSERVER_PORT
            )),
            Err(e) => messages.push(format!("Warning: Failed to start webserver: {}", e)),
        }
    } else {
        messages.push(format!(
            "Webserver running at http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        ));
    }

    // Build response message
    let attach_hint = "Attach with: midtown attach".to_string();
    messages.push(attach_hint);

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Path to the shared webserver PID file.
fn webserver_pid_file() -> PathBuf {
    midtown::paths::midtown_base_dir().join("webserver.pid")
}

/// Check if the shared webserver is running by testing its PID file lock.
fn webserver_is_running() -> bool {
    let pid_file = webserver_pid_file();
    if !pid_file.exists() {
        return false;
    }
    is_daemon_running(&pid_file)
}

/// Launch the shared webserver as a background process.
fn launch_webserver() -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {}", e))?;

    let mut cmd = Command::new(&exe);
    cmd.args(["webserver", "run"]);
    // Don't pass --foreground so it daemonizes itself
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn()
        .map_err(|e| format!("Failed to spawn webserver: {}", e))?;

    // Brief wait for it to start
    std::thread::sleep(std::time::Duration::from_millis(300));

    Ok(())
}

/// Stop the shared webserver by sending SIGTERM to its PID.
fn stop_webserver() -> Result<bool, String> {
    let pid_file = webserver_pid_file();
    if !pid_file.exists() {
        return Ok(false);
    }

    if !webserver_is_running() {
        // Stale PID file, clean up
        let _ = std::fs::remove_file(&pid_file);
        return Ok(false);
    }

    let pid_str = std::fs::read_to_string(&pid_file)
        .map_err(|e| format!("Failed to read webserver PID file: {}", e))?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|e| format!("Invalid PID in webserver PID file: {}", e))?;

    // Send SIGTERM
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status();

    // Poll until the process exits or timeout after 2 seconds
    let poll_interval = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();

    while webserver_is_running() && start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
    }

    // Force kill if still running after graceful timeout
    if webserver_is_running() {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stderr(Stdio::null())
            .status();

        // Poll again after SIGKILL
        let kill_start = std::time::Instant::now();
        let kill_timeout = std::time::Duration::from_secs(1);
        while webserver_is_running() && kill_start.elapsed() < kill_timeout {
            std::thread::sleep(poll_interval);
        }
    }

    // Clean up PID file
    let _ = std::fs::remove_file(&pid_file);

    Ok(true)
}

/// Handle `midtown webserver stop` command.
pub fn handle_webserver_stop() -> Result<Response, String> {
    match stop_webserver()? {
        true => Ok(Response::message("Stopped webserver")),
        false => Ok(Response::message("Webserver was not running")),
    }
}

/// Handle `midtown webserver restart` command.
pub fn handle_webserver_restart() -> Result<Response, String> {
    let was_running = stop_webserver()?;
    // stop_webserver() polls until confirmed dead, no extra sleep needed
    launch_webserver()?;
    if was_running {
        Ok(Response::message(format!(
            "Restarted webserver on http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    } else {
        Ok(Response::message(format!(
            "Started webserver on http://localhost:{}",
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    }
}

/// Handle `midtown stop` command.
///
/// Stops the daemon, webserver, and optionally the tmux session.
/// Also cleans up any orphaned `gh webhook forward` processes.
pub fn handle_stop(keep_session: bool) -> Result<Response, String> {
    if !running_inside_sandbox()
        && let Some(runtime) = load_sandbox_runtime_state_for_current_repo()
    {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?;
        let mut args = vec![
            "--format".to_string(),
            "json".to_string(),
            "stop".to_string(),
        ];
        if keep_session {
            args.push("--keep-session".to_string());
        }
        return run_midtown_command_in_sandbox(&runtime, &cwd, &args);
    }

    let mut messages = Vec::new();

    // Get session name (if in a git repo)
    if let Ok(session) = session_name() {
        // Stop tmux session (unless --keep-session)
        if !keep_session && session_exists(&session) {
            // SIGTERM all pane processes first — Claude Code survives SIGHUP
            // (which is what tmux kill-session sends), leaving orphaned processes
            // that consume memory and cause contention with other instances.
            midtown::tmux::terminate_session_processes(&session);

            let status = Command::new("tmux")
                .args(["kill-session", "-t", &session])
                .status()
                .map_err(|e| format!("Failed to kill session: {}", e))?;

            if status.success() {
                messages.push(format!("Stopped session '{}'", session));
            } else {
                messages.push(format!("Warning: Failed to stop session '{}'", session));
            }
        } else if session_exists(&session) {
            messages.push(format!(
                "Keeping session '{}' (use without --keep-session to stop)",
                session
            ));
        }
    }

    // Step 2: Stop daemon (this also signals the gh webhook forwarder to stop)
    if daemon_is_running() {
        // Read the PID and send SIGTERM for a clean shutdown
        let pid_path = midtown::paths::daemon_pid_file();
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
            && let Ok(pid) = pid_str.trim().parse::<u32>()
        {
            // Send SIGTERM for graceful shutdown
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stderr(Stdio::null())
                .status();

            // Poll until the daemon exits or timeout after 2 seconds.
            // This matches the webserver stop behavior and ensures the daemon
            // has fully cleaned up (released socket, written state) before we
            // proceed with restart or other operations.
            let poll_interval = std::time::Duration::from_millis(50);
            let timeout = std::time::Duration::from_secs(2);
            let start = std::time::Instant::now();
            while daemon_is_running() && start.elapsed() < timeout {
                std::thread::sleep(poll_interval);
            }

            // Force kill if still running after graceful timeout
            if daemon_is_running() {
                let _ = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stderr(Stdio::null())
                    .status();

                // Brief poll after SIGKILL
                let kill_timeout = std::time::Duration::from_secs(1);
                let kill_start = std::time::Instant::now();
                while daemon_is_running() && kill_start.elapsed() < kill_timeout {
                    std::thread::sleep(poll_interval);
                }
            }
        }

        // Clean up stale socket file only if daemon is now stopped.
        // The daemon should clean up its own socket during normal shutdown,
        // but if it crashed or was force-killed, the socket may remain.
        if !daemon_is_running() {
            let path = socket_path();
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
        }
        messages.push("Stopped daemon".to_string());
    } else {
        messages.push("Daemon was not running".to_string());
    }

    // Step 3: Kill any orphaned `gh webhook forward` processes.
    // The daemon's SIGTERM handler should have already stopped these, but
    // if the daemon exited uncleanly they may be left behind.
    kill_orphaned_webhook_forwarders(&mut messages);

    // Step 4: Kill any orphaned claude processes.
    // Claude Code handles SIGHUP, so if the tmux session was killed directly
    // (without going through `midtown stop`), processes may still be running.
    kill_orphaned_claude_processes(&mut messages);

    // Step 5: Stop the standalone webserver
    if webserver_is_running() {
        match stop_webserver() {
            Ok(true) => messages.push("Stopped webserver".to_string()),
            Ok(false) => {}
            Err(e) => messages.push(format!("Warning: Failed to stop webserver: {}", e)),
        }
    }

    Ok(Response::Message {
        message: messages.join(". "),
    })
}

/// Kill any orphaned Claude processes that were started by midtown.
///
/// Claude Code (node) handles SIGHUP, so when a tmux session is killed directly
/// (without going through `midtown stop`), claude processes survive and become
/// orphaned (PPID=1). This function finds and kills only those orphans.
///
/// We're conservative here: only kill processes that:
/// 1. Match midtown's settings file pattern
/// 2. Have PPID=1 (truly orphaned, no legitimate parent)
///
/// This avoids killing claude processes the user started manually or in other projects.
fn kill_orphaned_claude_processes(messages: &mut Vec<String>) {
    // Pattern matches claude processes using midtown settings files
    let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";

    let count = midtown::tmux::kill_orphaned_processes(pattern);
    if count > 0 {
        messages.push(format!("Killed {} orphaned claude process(es)", count));
    }
}

/// Kill any orphaned `gh webhook forward` processes for the current project.
///
/// Uses `pkill` to find and terminate processes matching the project-specific
/// webhook URL (e.g., `localhost:47023/webhook`). This avoids killing forwarders
/// belonging to other running projects in a multi-project setup.
///
/// Falls back to a broad `gh webhook forward` pattern only if the project's
/// webhook port cannot be determined.
fn kill_orphaned_webhook_forwarders(messages: &mut Vec<String>) {
    // Try to scope the pattern to this project's webhook port
    let pattern = resolve_project_name(&None)
        .map(|name| midtown::config::get_project_daemon_config(&name))
        .and_then(|daemon_cfg| daemon_cfg.webhook_port)
        .map(|port| format!("localhost:{}/webhook", port))
        .unwrap_or_else(|| "gh webhook forward".to_string());

    let output = Command::new("pgrep").args(["-f", &pattern]).output();

    match output {
        Ok(out) if out.status.success() => {
            // There are running gh webhook forward processes — kill them
            let _ = Command::new("pkill")
                .args(["-f", &pattern])
                .stderr(Stdio::null())
                .status();
            messages.push("Stopped gh webhook forwarder".to_string());
        }
        _ => {
            // No matching processes or pgrep failed — nothing to clean up
        }
    }
}

/// Handle `midtown restart` command.
///
/// Gracefully restarts the daemon and webserver while preserving the tmux
/// session and all running Claude processes (Lead and coworkers). The daemon
/// and webserver processes are restarted so they pick up new code, while
/// the chat pane is also respawned.
///
/// For a full fresh start, use `midtown stop && midtown start`.
pub fn handle_restart() -> Result<Response, String> {
    if !running_inside_sandbox()
        && let Some(runtime) = load_sandbox_runtime_state_for_current_repo()
    {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?;
        let args = vec![
            "--format".to_string(),
            "json".to_string(),
            "restart".to_string(),
        ];
        return run_midtown_command_in_sandbox(&runtime, &cwd, &args);
    }

    // Stop daemon and webserver, keep the tmux session running.
    // handle_stop also cleans up orphaned gh webhook forwarders.
    // Both daemon and webserver stop functions now poll until processes exit,
    // ensuring clean shutdown before restart.
    handle_stop(true)?;

    // Final verification that both daemon and webserver are stopped.
    // This guards against race conditions where the processes are still
    // cleaning up even after handle_stop returned.
    let poll_interval = std::time::Duration::from_millis(50);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();
    while (daemon_is_running() || webserver_is_running()) && start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
    }

    // Fail if processes are still running after timeout
    if daemon_is_running() {
        return Err("Restart failed: daemon did not stop within timeout".to_string());
    }
    if webserver_is_running() {
        return Err("Restart failed: webserver did not stop within timeout".to_string());
    }

    // Start daemon and webserver only — the tmux session and lead window
    // already exist (we kept them above). Passing daemon_only=true prevents
    // handle_start from entering the session-creation path, which could
    // race with check_and_respawn_lead to create duplicate lead windows.
    let result = handle_start(true, false, None, vec![])?;

    // Restart the chat pane to pick up code changes.
    // Use respawn-pane -k to atomically kill the old process and start a new
    // one, avoiding a race where send-keys characters (like 'i' in "midtown")
    // are intercepted by the still-running TUI's input mode handler.
    if let Ok(session) = session_name() {
        let chat_pane = format!("{}:lead.1", session);
        let bin_command = midtown::config::get_bin_command();
        let chat_cmd = format!("{} chat", bin_command);
        let _ = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &chat_pane, &chat_cmd])
            .status();
    }

    // Enhance the message to clarify graceful restart
    match result {
        Response::Message { message } => Ok(Response::Message {
            message: format!(
                "{}. Resumed Lead session in '{}'. Attach with: midtown attach",
                message,
                session_name().unwrap_or_else(|_| "midtown".to_string())
            ),
        }),
        other => Ok(other),
    }
}

/// Clear stale Lead session ID file for a project.
///
/// When no tmux session is running, the session-id file from a previous
/// session is stale. Removing it ensures a fresh Claude Code session
/// is created instead of resuming the old one.
fn clear_stale_lead_session(repo: &Path) {
    let session_file = lead_session_file(repo);
    if session_file.exists() {
        let _ = std::fs::remove_file(&session_file);
    }
}

/// Handle `midtown attach` command.
///
/// Attaches to the project's tmux session.
/// If the session doesn't exist, it is automatically created first.
/// If the session exists but Lead wasn't started with midtown settings, reinitialize it.
pub fn handle_attach(project: Option<&str>) -> Result<Response, String> {
    if !running_inside_sandbox()
        && let Some(runtime) = load_sandbox_runtime_state_for_current_repo()
    {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?;
        let mut args = vec!["attach".to_string()];
        if let Some(name) = project {
            args.push(name.to_string());
        }
        return exec_midtown_command_in_sandbox(&runtime, &cwd, &args);
    }

    let session = match project {
        // Explicit project name: construct session name directly
        Some(name) => format!("midtown-{}", name),
        // No project: infer from cwd
        None => session_name()?,
    };

    // For cwd-based attach, we can get the repo root for stale session cleanup
    let repo = if project.is_none() {
        repo_root().ok()
    } else {
        None
    };

    // Auto-create session if it doesn't exist
    if !session_exists(&session) {
        if project.is_some() {
            // Named project: don't auto-create, just error
            return Err(format!(
                "No tmux session '{}' found. Start the project first with 'midtown start'.",
                session
            ));
        }

        // No active tmux session means Lead is not running.
        // Clear any stale session-id file so we start a fresh
        // Claude Code session instead of resuming the old one.
        if let Some(ref repo) = repo {
            clear_stale_lead_session(repo);
        }

        // Start midtown (daemon + tmux session)
        handle_start(false, false, None, vec![])?;

        // Wait briefly for the session to be ready
        std::thread::sleep(std::time::Duration::from_millis(200));
    } else if let Some(ref repo) = repo {
        // Session exists - ensure Lead has proper settings
        ensure_lead_has_settings(&session, repo)?;
    }

    // Execute tmux attach - this replaces the current process
    let err = Command::new("tmux").args(["attach", "-t", &session]).exec();

    // If we get here, exec failed
    Err(format!("Failed to attach to session: {}", err))
}

/// List all known projects and their running status.
pub fn handle_project_list() -> Result<Response, String> {
    let projects_dir = midtown::paths::midtown_base_dir().join("projects");

    if !projects_dir.exists() {
        return Ok(Response::message("No projects found."));
    }

    let mut entries: Vec<_> = std::fs::read_dir(&projects_dir)
        .map_err(|e| format!("Failed to read projects directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Ok(Response::message("No projects found."));
    }

    let mut lines = Vec::new();
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let pid_file = entry.path().join("daemon.pid");

        // Check if daemon is running by testing PID file lock
        let status = if pid_file.exists() {
            match is_daemon_running(&pid_file) {
                true => "running",
                false => "stopped",
            }
        } else {
            "stopped"
        };

        // Check tmux session
        let session = format!("midtown-{}", name);
        let has_session = session_exists(&session);

        let status_display = if status == "running" && has_session {
            "running"
        } else if status == "running" {
            "daemon only"
        } else {
            "stopped"
        };

        lines.push(format!("{:<20} {}", name, status_display));
    }

    Ok(Response::message(lines.join("\n")))
}

/// Check if the daemon is running by testing the PID file lock.
fn is_daemon_running(pid_file: &Path) -> bool {
    use std::fs::OpenOptions;

    let file = match OpenOptions::new().read(true).open(pid_file) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // Try to get an exclusive lock. If we can't, the daemon holds it.
    use fs2::FileExt;
    match file.try_lock_exclusive() {
        Ok(_) => {
            // We got the lock => daemon is NOT running
            let _ = file.unlock();
            false
        }
        Err(_) => {
            // Lock held => daemon IS running
            true
        }
    }
}

/// Ensure the Lead pane has proper midtown settings.
/// Checks for a marker file; if missing, uses spawn_lead() to restart with settings.
fn ensure_lead_has_settings(session: &str, repo: &Path) -> Result<(), String> {
    let marker_path = lead_initialized_marker(repo);

    // Check if Lead was properly initialized
    if marker_path.exists() {
        // Check marker version matches current
        let marker_version = std::fs::read_to_string(&marker_path).unwrap_or_default();
        if marker_version.trim() == env!("CARGO_PKG_VERSION") {
            return Ok(()); // Already initialized with current version
        }
    }

    // Need to reinitialize Lead with proper settings
    eprintln!("Reinitializing Lead with midtown settings...");

    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    // spawn_lead kills existing lead windows and creates a fresh one
    midtown::tmux::spawn_lead(session, &repo.to_string_lossy(), &repo_name, &[])
        .map_err(|e| format!("Failed to re-launch lead: {}", e))?;

    // Set up chat pane (split or separate window based on config)
    midtown::tmux::setup_chat_pane(session);

    // Write the marker file
    if let Some(parent) = marker_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

    Ok(())
}

/// Path to the marker file indicating Lead was initialized by midtown.
fn lead_initialized_marker(repo: &Path) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    midtown::paths::lead_dir_for_repo(&repo_name).join("lead-initialized")
}

/// Get session status for status command enhancement.
#[allow(dead_code)]
pub fn get_session_status() -> (bool, bool) {
    let exists = session_name().map(|s| session_exists(&s)).unwrap_or(false);
    (daemon_is_running(), exists)
}

/// Handle `midtown lead register-session` command.
///
/// Detects the Lead's Claude Code session UUID and saves it so coworkers
/// can link their task directories to share tasks.
pub fn handle_register_session() -> Result<Response, String> {
    use std::fs;

    let repo = repo_root()?;
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    // Find the Lead's session UUID (most recently modified task directory)
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let tasks_dir = home.join(".claude").join("tasks");

    if !tasks_dir.exists() {
        return Err(
            "No Claude Code task directories found. Create a task first with TaskCreate."
                .to_string(),
        );
    }

    let lead_uuid = find_newest_dir(&tasks_dir)?;

    // Save to ~/.midtown/lead/<repo>/session-id
    let lead_dir = midtown::paths::lead_dir_for_repo(&repo_name);
    fs::create_dir_all(&lead_dir).map_err(|e| format!("Failed to create lead directory: {}", e))?;

    let session_file = midtown::paths::lead_session_file_for_repo(&repo_name);
    fs::write(&session_file, &lead_uuid)
        .map_err(|e| format!("Failed to write session file: {}", e))?;

    Ok(Response::Message {
        message: format!("Registered Lead session: {}", lead_uuid),
    })
}

/// Find the most recently modified directory in the given path.
fn find_newest_dir(dir: &std::path::Path) -> Result<String, String> {
    use std::fs;

    let entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("Cannot read directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && !e.path().is_symlink())
        .collect();

    if entries.is_empty() {
        return Err("No directories found".to_string());
    }

    entries
        .iter()
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        .ok_or_else(|| "Failed to determine newest directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Mutex to serialize tests that change CWD
    static CWD_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to create a fake git repo in a temp directory
    fn create_git_repo(dir: &std::path::Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
    }

    /// Helper to create a git worktree marker (file instead of directory)
    fn create_git_worktree(dir: &std::path::Path) {
        fs::write(
            dir.join(".git"),
            "gitdir: /some/other/path/.git/worktrees/foo",
        )
        .unwrap();
    }

    /// Canonicalize path to handle macOS /var -> /private/var symlink
    fn canonical(path: &std::path::Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    /// Run a test with a temporary working directory, restoring CWD after
    fn with_temp_cwd<F, T>(temp_path: &std::path::Path, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _lock = CWD_MUTEX.lock().unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();
        let result = f();
        std::env::set_current_dir(original_dir).unwrap();
        result
    }

    #[test]
    fn test_find_git_root_in_repo_root() {
        let temp = TempDir::new().unwrap();
        create_git_repo(temp.path());

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_some());
            assert_eq!(canonical(&result.unwrap()), canonical(temp.path()));
        });
    }

    #[test]
    fn test_find_git_root_in_subdirectory() {
        let temp = TempDir::new().unwrap();
        create_git_repo(temp.path());

        // Create nested subdirectory
        let subdir = temp.path().join("src").join("lib").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        with_temp_cwd(&subdir, || {
            let result = find_git_root();
            assert!(result.is_some());
            // Just verify .git exists at the found root
            let found_root = result.unwrap();
            assert!(found_root.join(".git").exists());
        });
    }

    #[test]
    fn test_find_git_root_with_worktree() {
        let temp = TempDir::new().unwrap();
        // Worktrees have a .git file instead of directory
        create_git_worktree(temp.path());

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_some());
            assert_eq!(canonical(&result.unwrap()), canonical(temp.path()));
        });
    }

    #[test]
    fn test_find_git_root_not_in_repo() {
        let temp = TempDir::new().unwrap();
        // Don't create .git

        with_temp_cwd(temp.path(), || {
            let result = find_git_root();
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_session_name_in_fake_repo() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("my-project");
        fs::create_dir_all(&repo_dir).unwrap();
        create_git_repo(&repo_dir);

        with_temp_cwd(&repo_dir, || {
            // find_git_root should work, but session_name uses git rev-parse
            // which won't work in a fake repo. Just test find_git_root here.
            let result = find_git_root();
            assert!(result.is_some());
        });
    }

    #[test]
    fn test_session_name_not_in_repo() {
        let temp = TempDir::new().unwrap();
        // Don't create .git

        with_temp_cwd(temp.path(), || {
            let result = session_name();
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("Not in a git repository"));
        });
    }

    #[test]
    fn test_repo_root_not_in_repo() {
        let temp = TempDir::new().unwrap();

        with_temp_cwd(temp.path(), || {
            let result = repo_root();
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.contains("Not in a git repository"));
            assert!(err.contains("--repo"));
        });
    }

    #[test]
    fn test_socket_path_format() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("midtown"));
        assert!(path.to_string_lossy().ends_with("daemon.sock"));
    }

    #[test]
    fn test_session_exists_nonexistent() {
        // Random session name that definitely doesn't exist
        assert!(!session_exists("midtown-nonexistent-test-session-12345"));
    }

    #[test]
    fn test_handle_start_clears_stale_session_id() {
        let temp = TempDir::new().unwrap();
        let unique_name = format!("test-project-{}", uuid::Uuid::new_v4());
        let repo_path = temp.path().join(&unique_name);
        std::fs::create_dir_all(&repo_path).unwrap();

        // Simulate a stale session-id file from a previous session
        let session_file = lead_session_file(&repo_path);
        if let Some(parent) = session_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&session_file, "old-session-id-12345").unwrap();
        assert!(session_file.exists());

        // clear_stale_lead_session should remove the stale file
        clear_stale_lead_session(&repo_path);

        assert!(
            !session_file.exists(),
            "Stale session-id file should be deleted when no tmux session is running"
        );

        // Clean up parent dir
        if let Some(parent) = session_file.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn test_resolve_project_name_explicit() {
        let result = resolve_project_name(&Some("my-project".to_string()));
        assert_eq!(result, Some("my-project".to_string()));
    }

    #[test]
    fn test_resolve_project_name_none_outside_repo() {
        let temp = TempDir::new().unwrap();
        // No .git directory

        with_temp_cwd(temp.path(), || {
            let result = resolve_project_name(&None);
            // Outside a git repo, detect_project_name returns None and repo_root fails
            // so result should be None
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_session_name_for_explicit_project() {
        let result = session_name_for(&Some("myapp".to_string()));
        assert_eq!(result.unwrap(), "midtown-myapp");
    }

    #[test]
    fn test_session_name_for_none_outside_repo() {
        let temp = TempDir::new().unwrap();

        with_temp_cwd(temp.path(), || {
            let result = session_name_for(&None);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_resolve_repos_uses_cli_when_provided() {
        let repos = vec![PathBuf::from("/path/a"), PathBuf::from("/path/b")];
        let result = resolve_repos(&repos, "nonexistent-project");
        assert_eq!(result, repos);
    }

    #[test]
    fn test_resolve_repos_empty_cli_returns_saved() {
        // With no CLI repos and no config, should return empty
        let result = resolve_repos(&[], &format!("no-such-project-{}", uuid::Uuid::new_v4()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_saved_repos_nonexistent_project() {
        let result = parse_saved_repos(&format!("no-such-project-{}", uuid::Uuid::new_v4()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_update_project_config_creates_config() {
        let dir = tempfile::tempdir().unwrap();
        let project_name = format!("test-update-{}", uuid::Uuid::new_v4());
        let primary_repo = dir.path().join("main-repo");
        let additional = vec![dir.path().join("extra-repo")];

        // This will try to write to ~/.midtown/projects/<name>/config.toml
        // We test that it doesn't panic/error
        let result = update_project_config(&project_name, &primary_repo, &additional);

        // Clean up
        let config_path = midtown::config::project_config_path(&project_name);
        let _ = std::fs::remove_file(&config_path);
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_project_name_valid() {
        assert!(validate_project_name("my-project").is_ok());
        assert!(validate_project_name("my_project").is_ok());
        assert!(validate_project_name("myproject123").is_ok());
        assert!(validate_project_name("my.project").is_ok());
        assert!(validate_project_name("A").is_ok());
    }

    #[test]
    fn test_validate_project_name_invalid() {
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name("my'project").is_err());
        assert!(validate_project_name("my project").is_err());
        assert!(validate_project_name("my/project").is_err());
        assert!(validate_project_name("my;project").is_err());
        assert!(validate_project_name("$(whoami)").is_err());
    }

    #[test]
    fn test_claude_cli_available_returns_bool() {
        // This test verifies the function runs without panicking.
        // The actual result depends on whether claude is installed in the test environment.
        let _result: bool = claude_cli_available();
        // If we get here without panicking, the function works correctly
    }

    #[test]
    fn test_sanitize_container_name() {
        assert_eq!(
            sanitize_container_name("My Project.Name"),
            "my-project-name"
        );
        assert_eq!(sanitize_container_name("___"), "___");
        assert_eq!(sanitize_container_name(""), "");
    }

    #[test]
    fn test_sandbox_runtime_state_roundtrip() {
        let state = SandboxRuntimeState {
            engine: SandboxEngine::Docker,
            container_name: "midtown-sandbox-test".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: SandboxRuntimeState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, state);
    }
}
