//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::Response;
use crate::client::DaemonClient;

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

/// Get the session name for an explicit or inferred project.
/// Format: midtown-{project_name}
/// Used for both Zellij and tmux sessions.
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

/// Get the session name based on the project name.
/// Format: midtown-{project_name}
/// Used for both Zellij and tmux sessions.
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

/// Track whether a progress line is currently displayed (needs clearing before other output).
static PROGRESS_LINE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "macos")]
static ACCESSIBILITY_SETTINGS_OPENED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Clear the in-progress gauge line so subsequent output (errors, warnings) doesn't overlap it.
fn clear_startup_progress() {
    use std::io::IsTerminal;
    use std::io::Write;

    if PROGRESS_LINE_ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst)
        && std::io::stderr().is_terminal()
    {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[K");
        let _ = stderr.flush();
    }
}

fn emit_startup_progress(percent: u16, message: &str) {
    use crossterm::style::{
        Attribute, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    };
    use crossterm::terminal;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
        widgets::{Gauge, Widget},
    };
    use std::io::IsTerminal;
    use std::io::Write;

    if std::io::stderr().is_terminal() {
        let percent = percent.min(100);
        let width = terminal::size()
            .map(|(w, _)| w.saturating_sub(1).clamp(24, 100))
            .unwrap_or(80);
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let label = format!("{:>3}% {}", percent, message);

        Gauge::default()
            .ratio(f64::from(percent) / 100.0)
            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
            .label(label)
            .render(area, &mut buffer);

        // Render buffer cells with ANSI colors so the gauge looks correct.
        let mut line = String::with_capacity(width as usize * 4);
        let mut prev_fg: Option<Color> = None;
        let mut prev_bg: Option<Color> = None;
        for x in 0..width {
            let cell = &buffer[(x, 0)];
            let fg = cell.fg;
            let bg = cell.bg;
            if prev_fg != Some(fg) || prev_bg != Some(bg) {
                if let Some(ct_fg) = ratatui_to_crossterm_color(fg) {
                    line.push_str(&format!("{}", SetForegroundColor(ct_fg)));
                }
                if let Some(ct_bg) = ratatui_to_crossterm_color(bg) {
                    line.push_str(&format!("{}", SetBackgroundColor(ct_bg)));
                }
                // Bold text on the filled portion for better readability
                if fg == Color::DarkGray {
                    line.push_str(&format!("{}", SetAttribute(Attribute::Bold)));
                } else {
                    line.push_str(&format!("{}", SetAttribute(Attribute::NormalIntensity)));
                }
                prev_fg = Some(fg);
                prev_bg = Some(bg);
            }
            line.push_str(cell.symbol());
        }
        line.push_str(&format!("{}", ResetColor));

        let mut stderr = std::io::stderr().lock();
        // Overwrite the current line (\r) and clear to end of line (\x1b[K)
        let _ = write!(stderr, "\r{}\x1b[K", line);
        if percent >= 100 {
            let _ = writeln!(stderr);
            PROGRESS_LINE_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        } else {
            PROGRESS_LINE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = stderr.flush();
    }
}

fn ratatui_to_crossterm_color(color: ratatui::style::Color) -> Option<crossterm::style::Color> {
    use crossterm::style::Color as CtColor;
    use ratatui::style::Color as RaColor;
    #[allow(unreachable_patterns)]
    match color {
        RaColor::Reset => Some(CtColor::Reset),
        RaColor::Black => Some(CtColor::Black),
        RaColor::Red => Some(CtColor::DarkRed),
        RaColor::Green => Some(CtColor::DarkGreen),
        RaColor::Yellow => Some(CtColor::DarkYellow),
        RaColor::Blue => Some(CtColor::DarkBlue),
        RaColor::Magenta => Some(CtColor::DarkMagenta),
        RaColor::Cyan => Some(CtColor::DarkCyan),
        RaColor::Gray => Some(CtColor::Grey),
        RaColor::DarkGray => Some(CtColor::DarkGrey),
        RaColor::LightRed => Some(CtColor::Red),
        RaColor::LightGreen => Some(CtColor::Green),
        RaColor::LightYellow => Some(CtColor::Yellow),
        RaColor::LightBlue => Some(CtColor::Blue),
        RaColor::LightMagenta => Some(CtColor::Magenta),
        RaColor::LightCyan => Some(CtColor::Cyan),
        RaColor::White => Some(CtColor::White),
        RaColor::Rgb(r, g, b) => Some(CtColor::Rgb { r, g, b }),
        RaColor::Indexed(i) => Some(CtColor::AnsiValue(i)),
        _ => None,
    }
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
    // Check Zellij first, then fall back to tmux
    if zellij_session_exists(session) {
        return true;
    }
    tmux_session_exists(session)
}

/// Check if a Zellij session with the given name exists.
/// Delegates to the shared implementation in `midtown::process`.
fn zellij_session_exists(session: &str) -> bool {
    midtown::process::zellij_session_exists(session)
}

/// Check if a Zellij session is actively running (not exited/resurrectable).
fn zellij_running_session_exists(session: &str) -> bool {
    midtown::process::zellij_running_session_exists(session)
}

/// Resolve Zellij's on-disk `session_info` directory using `zellij setup --check`.
///
/// Returns a best-effort path to the session info root, typically one of:
/// - `<cache-dir>/<version>/session_info`
/// - `<cache-dir>/session_info`
fn zellij_session_info_root() -> Option<PathBuf> {
    let output = Command::new("zellij")
        .args(["setup", "--check"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut cache_dir: Option<PathBuf> = None;
    let mut version: Option<String> = None;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("[CACHE DIR]:") {
            let p = rest.trim().trim_matches('"');
            if !p.is_empty() {
                cache_dir = Some(PathBuf::from(p));
            }
        } else if let Some(rest) = line.strip_prefix("[Version]:") {
            let v = rest.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                version = Some(v);
            }
        }
    }

    let cache_dir = cache_dir?;
    if let Some(version) = version {
        let versioned = cache_dir.join(version).join("session_info");
        if versioned.exists() {
            return Some(versioned);
        }
    }
    Some(cache_dir.join("session_info"))
}

/// Delete an exited/resurrectable Zellij session entry.
///
/// Zellij can report sessions as `EXITED - attach to resurrect`; these are not
/// killable with `kill-session` and can block Midtown startup if treated as live.
/// We first ask Zellij to delete it, then fall back to removing stale
/// `session_info/<name>` metadata if needed.
fn cleanup_exited_zellij_session(session: &str) -> Result<bool, String> {
    let _ = Command::new("zellij")
        .args(["delete-session", "--force", session])
        .status();

    if !matches!(
        midtown::process::zellij_session_state(session),
        Some(midtown::process::ZellijSessionState::Exited)
    ) {
        return Ok(true);
    }

    if let Some(root) = zellij_session_info_root() {
        let stale_dir = root.join(session);
        if stale_dir.exists() {
            std::fs::remove_dir_all(&stale_dir)
                .map_err(|e| format!("Failed to remove stale Zellij session metadata: {}", e))?;
        }
    }

    Ok(!matches!(
        midtown::process::zellij_session_state(session),
        Some(midtown::process::ZellijSessionState::Exited)
    ))
}

/// Check if a tmux session with the given name exists.
fn tmux_session_exists(session: &str) -> bool {
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

/// Ensure the official Claude plugins marketplace is configured.
///
/// Returns true if marketplace is configured (or was successfully added), false on failure.
fn ensure_official_marketplace() -> bool {
    // Check if any marketplace is configured
    let output = match Command::new("claude")
        .args(["plugin", "marketplace", "list"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to check marketplace configuration: {}", e);
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // If output contains "claude-plugins-official", marketplace is already configured
    if stdout.contains("claude-plugins-official") {
        return true;
    }

    // If output contains "No marketplaces configured", we need to add it
    if !stdout.contains("No marketplaces configured") {
        // Some other marketplace exists, but not the official one - still add it
        // (or if output is unexpected, try adding anyway)
    }

    // Add the official marketplace
    emit_startup_progress(8, "adding official plugins marketplace");
    let output = match Command::new("claude")
        .args([
            "plugin",
            "marketplace",
            "add",
            "https://github.com/anthropics/claude-plugins-official",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to add official marketplace: {}", e);
            return false;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Failed to add official marketplace: {}", stderr);
        return false;
    }

    true
}

/// Install a Claude Code plugin via `claude plugin install`.
///
/// Returns true on success, false on failure (logs error but doesn't block startup).
fn install_plugin(plugin_id: &str) -> bool {
    let output = match Command::new("claude")
        .args(["plugin", "install", plugin_id])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to run claude plugin install {}: {}", plugin_id, e);
            return false;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Failed to install plugin {}: {}", plugin_id, stderr);
        return false;
    }

    true
}

/// Check and install required Claude Code plugins.
///
/// Non-blocking: logs errors but doesn't stop startup if installation fails.
fn ensure_required_plugins() {
    // Ensure official marketplace is configured first
    if !ensure_official_marketplace() {
        eprintln!(
            "Warning: Could not configure official marketplace. Plugin installation may fail."
        );
        // Continue anyway - some plugins might be installed already
    }

    // Get list of installed plugins
    let output = match Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Warning: Failed to check installed plugins: {}", e);
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: Failed to list plugins: {}", stderr);
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output - it's an array of objects with "id" field
    let plugins: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Warning: Failed to parse plugin list JSON: {}", e);
            return;
        }
    };

    let installed: std::collections::HashSet<String> = plugins
        .iter()
        .filter_map(|p| p.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    // Required plugins from daemon/mod.rs
    let required = [
        "superpowers@claude-plugins-official",
        "code-review@claude-plugins-official",
        "pr-review-toolkit@claude-plugins-official",
        "commit-commands@claude-plugins-official",
        "feature-dev@claude-plugins-official",
        "explanatory-output-style@claude-plugins-official",
        "code-simplifier@claude-plugins-official",
    ];

    // Find missing plugins
    let missing: Vec<_> = required
        .iter()
        .filter(|p| !installed.contains(**p))
        .collect();

    if missing.is_empty() {
        return;
    }

    // Install missing plugins
    for plugin_id in &missing {
        emit_startup_progress(10, &format!("installing plugin {}", plugin_id));
        install_plugin(plugin_id);
    }
}

/// Handle `midtown start` command.
///
/// Starts Midtown services for the current project (daemon + shared webserver).
/// Interactive terminal UX now lives in `midtown view`.
pub fn handle_start(project: Option<String>, repos: Vec<PathBuf>) -> Result<Response, String> {
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
    let additional_repos = resolve_repos(&repos, &project_name);
    emit_startup_progress(5, &format!("starting project '{}'", project_name));

    // Check and install required plugins before starting daemon
    ensure_required_plugins();

    let mut messages = Vec::new();

    // Update project config with repo information
    let _ = update_project_config(&project_name, &primary_repo, &additional_repos);

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
        emit_startup_progress(65, "daemon already running");
    } else {
        // Clean up any stale PID file or orphaned daemon before starting
        emit_startup_progress(65, "starting daemon");
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
        emit_startup_progress(75, "waiting for daemon socket");
        let started = wait_for_daemon_socket(75, 200);

        if started {
            messages.push("Started daemon".to_string());
            emit_startup_progress(82, "daemon is ready");
        } else {
            clear_startup_progress();
            return Err("Daemon failed to start".to_string());
        }
    }

    // Step 2: Spawn the Lead as a headless session (idempotent).
    emit_startup_progress(88, "spawning headless lead session");
    let lead_provider = std::env::var("MIDTOWN_LEAD_PROVIDER")
        .ok()
        .and_then(|s| s.parse::<midtown::auth::AuthProvider>().ok())
        .unwrap_or(midtown::auth::AuthProvider::Claude);
    match DaemonClient::connect() {
        Ok(client) => match client.lead_spawn(lead_provider) {
            Ok(_) => messages.push("Lead session running".to_string()),
            Err(e) => messages.push(format!("Warning: Failed to spawn lead: {}", e)),
        },
        Err(e) => messages.push(format!("Warning: Could not connect to daemon: {}", e)),
    }

    // Step 3: Auto-launch shared webserver if not running
    if !webserver_is_running() {
        emit_startup_progress(96, "starting shared webserver");
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
        emit_startup_progress(96, "shared webserver already running");
    }

    // Build response message
    messages.push("Open view with: midtown view".to_string());
    emit_startup_progress(100, "startup complete");

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
    let mut messages = Vec::new();

    // Get session name (if in a git repo)
    if let Ok(session) = session_name() {
        // Stop session (unless --keep-session)
        if !keep_session && session_exists(&session) {
            // Try Zellij first, then tmux
            if zellij_session_exists(&session) {
                if zellij_running_session_exists(&session) {
                    // Kill the Zellij session — `zellij kill-session` sends SIGHUP to
                    // pane processes, which Claude Code may survive. Use
                    // `zellij kill-session` first, then clean up any orphaned processes.
                    let status = Command::new("zellij")
                        .args(["kill-session", &session])
                        .status()
                        .map_err(|e| format!("Failed to kill Zellij session: {}", e))?;

                    if status.success() {
                        messages.push(format!("Stopped Zellij session '{}'", session));
                    } else {
                        messages.push(format!(
                            "Warning: Failed to stop Zellij session '{}'",
                            session
                        ));
                    }
                } else if cleanup_exited_zellij_session(&session)? {
                    messages.push(format!("Deleted exited Zellij session '{}'", session));
                } else {
                    messages.push(format!(
                        "Warning: Failed to delete exited Zellij session '{}'",
                        session
                    ));
                }
            }

            // Also try tmux — both may exist in mixed environments, or only tmux
            if tmux_session_exists(&session) {
                // SIGTERM all pane processes first — Claude Code survives SIGHUP
                // (which is what tmux kill-session sends), leaving orphaned processes
                // that consume memory and cause contention with other instances.
                midtown::process::terminate_session_processes(&session);

                let status = Command::new("tmux")
                    .args(["kill-session", "-t", &session])
                    .status()
                    .map_err(|e| format!("Failed to kill tmux session: {}", e))?;

                if status.success() {
                    messages.push(format!("Stopped tmux session '{}'", session));
                } else {
                    messages.push(format!(
                        "Warning: Failed to stop tmux session '{}'",
                        session
                    ));
                }
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

    let count = midtown::process::kill_orphaned_processes(pattern);
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

/// Wait for coworkers to drain (reach stopped/stopping state) before shutdown.
///
/// First signals the daemon to enter draining mode (stops new task assignments).
/// Then polls coworker status every 500ms and displays progress. Returns once all
/// coworkers are stopped/stopping, or after the timeout expires (5 minutes).
fn wait_for_coworkers_to_drain(timeout_secs: u64) -> Result<(), String> {
    use std::collections::HashMap;

    let socket_path = socket_path();
    if !socket_path.exists() {
        // Daemon not running, nothing to drain
        return Ok(());
    }

    // First, signal the daemon to enter draining mode (stops assigning new tasks)
    let client = match crate::client::DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => {
            // Daemon stopped or connection failed
            return Ok(());
        }
    };

    if let Err(e) = client.enter_drain() {
        eprintln!("Warning: Failed to enter drain mode: {}", e);
        // Continue anyway - we'll still wait for coworkers to finish their current work
    }

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let mut last_status: HashMap<String, String> = HashMap::new();
    let mut first_iteration = true;

    loop {
        // Check timeout
        if start.elapsed() >= timeout {
            eprintln!("Timeout reached. Force-stopping remaining coworkers...");
            return Ok(()); // Proceed with force shutdown
        }

        // Query daemon status via RPC
        let client = match crate::client::DaemonClient::connect() {
            Ok(c) => c,
            Err(_) => {
                // Daemon stopped or connection failed
                return Ok(());
            }
        };

        let response = match client.status() {
            Ok(r) => r,
            Err(_) => {
                // Status query failed, assume daemon is stopping
                return Ok(());
            }
        };

        // Extract coworker info from the response
        let coworkers = match response {
            crate::cli::Response::Status(status_resp) => status_resp
                .full_status
                .as_ref()
                .map(|fs| fs.coworkers.clone())
                .unwrap_or_default(),
            _ => {
                // Unexpected response format
                return Err("Unexpected status response format".to_string());
            }
        };

        // If no coworkers exist, we're done immediately
        if coworkers.is_empty() {
            if first_iteration {
                eprintln!("No coworkers running.");
            }
            return Ok(());
        }

        // Print header on first iteration
        if first_iteration {
            eprintln!("Waiting for coworkers to finish...");
            first_iteration = false;
        }

        // Check if all coworkers are stopped or stopping
        let mut all_done = true;
        let mut current_status: HashMap<String, String> = HashMap::new();

        for coworker in &coworkers {
            let status = coworker.status.to_lowercase();
            current_status.insert(coworker.name.clone(), status.clone());

            // Consider stopped/stopping as done, and also treat running coworkers
            // with no current task as done (they're idle, waiting to be shut down).
            // CoworkerStatus has no Idle variant — idle state is inferred from
            // status == "running" with current_task == None.
            let is_done = status == "stopped"
                || status == "stopping"
                || (status == "running" && coworker.current_task.is_none());

            if !is_done {
                all_done = false;

                // Print status update if changed or first time seeing this coworker
                if last_status.get(&coworker.name) != Some(&status) {
                    let task_info = coworker
                        .current_task
                        .as_ref()
                        .map(|t| format!(" (task !{})", t))
                        .unwrap_or_default();
                    eprintln!("  {}: {}{}", coworker.name, status, task_info);
                }
            } else if last_status.get(&coworker.name) != Some(&status) {
                // Transitioned to done state - mark with checkmark
                eprintln!("  {}: {} ✓", coworker.name, status);
            }
        }

        // Report stopped coworkers (removed from the list)
        for name in last_status.keys() {
            if !current_status.contains_key(name) {
                eprintln!("  {} stopped. ✓", name);
            }
        }

        last_status = current_status;

        if all_done {
            eprintln!("All coworkers stopped.");
            return Ok(());
        }

        // Sleep before next poll
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Handle `midtown restart` command.
///
/// Gracefully restarts the daemon and webserver while preserving coworker state.
///
/// When `force` is false (default):
/// - Waits for all coworkers to finish their current work and reach stopped state
/// - Displays real-time status of which coworkers are still working
/// - Times out after 5 minutes and force-kills stuck coworkers
///
/// When `force` is true:
/// - Immediately stops all coworkers without waiting
/// - Used for urgent restarts or when daemon is unresponsive
///
/// For a full fresh start, use `midtown stop && midtown start`.
pub fn handle_restart(force: bool) -> Result<Response, String> {
    // If not forcing, wait for coworkers to drain gracefully
    if !force {
        // Timeout after 5 minutes (300 seconds)
        wait_for_coworkers_to_drain(300)?;
    }

    // Send exec-restart RPC to the daemon. The daemon will:
    // 1. Gracefully shut down (persist sessions, detach coworkers)
    // 2. Re-exec itself with --foreground, preserving its original process
    //    context. This is critical on macOS: the daemon was initially launched
    //    from an unsandboxed CLI, so re-exec preserves that unsandboxed state.
    //    If instead we stopped and re-spawned from a sandboxed agent process,
    //    the new daemon would inherit the sandbox and break coworker spawning.
    let client =
        DaemonClient::connect().map_err(|e| format!("Failed to connect to daemon: {}", e))?;

    // Stop the webserver first (it runs independently of the daemon)
    let _ = stop_webserver();

    // Tell the daemon to exec-restart
    if let Err(e) = client.exec_restart() {
        // Fallback: if RPC fails (e.g., old daemon without exec-restart support),
        // use the legacy stop+start path.
        eprintln!(
            "Warning: exec-restart RPC failed ({}), falling back to stop+start",
            e
        );
        drop(client);
        handle_stop(true)?;

        let poll_interval = std::time::Duration::from_millis(50);
        let timeout = std::time::Duration::from_secs(2);
        let start = std::time::Instant::now();
        while daemon_is_running() && start.elapsed() < timeout {
            std::thread::sleep(poll_interval);
        }

        let result = handle_start(None, vec![])?;
        return match result {
            Response::Message { message } => Ok(Response::Message {
                message: format!("{} (legacy restart). Open view with: midtown view", message),
            }),
            other => Ok(other),
        };
    }

    // Drop the client connection before waiting — the daemon is shutting down
    drop(client);

    // Wait for the daemon to come back up (exec-restart: socket disappears then reappears)
    let poll_interval = std::time::Duration::from_millis(100);

    // Phase 1: Wait for daemon to go down (socket becomes unavailable)
    let down_timeout = std::time::Duration::from_secs(10);
    let down_start = std::time::Instant::now();
    while daemon_is_running() && down_start.elapsed() < down_timeout {
        std::thread::sleep(poll_interval);
    }

    // Guard: if Phase 1 timed out without the daemon going down, the exec-restart
    // failed silently. Without this check, Phase 2 would immediately succeed
    // (daemon_is_running() returns true) and we'd report false success.
    if daemon_is_running() {
        return Err("Restart failed: daemon did not shut down after exec-restart RPC".to_string());
    }

    // Phase 2: Wait for daemon to come back up (socket becomes available)
    let up_timeout = std::time::Duration::from_secs(15);
    let up_start = std::time::Instant::now();
    while !daemon_is_running() && up_start.elapsed() < up_timeout {
        std::thread::sleep(poll_interval);
    }

    if !daemon_is_running() {
        return Err("Restart failed: daemon did not come back up after exec-restart".to_string());
    }

    // Restart the webserver
    launch_webserver().map_err(|e| format!("Failed to restart webserver: {}", e))?;

    Ok(Response::Message {
        message: "Daemon exec-restarted. Reopen with: midtown view".to_string(),
    })
}

#[derive(Clone)]
struct AttachContext {
    project_name: String,
    primary_repo: PathBuf,
    additional_repos: Vec<PathBuf>,
}

fn resolve_attach_context(project: Option<&str>) -> Result<AttachContext, String> {
    if let Some(name) = project {
        validate_project_name(name)?;
        let full = midtown::config::load_full_project_config(name).ok_or_else(|| {
            format!(
                "Unknown project '{}'. Start it first with `midtown start`.",
                name
            )
        })?;
        let primary_repo = full
            .project
            .primary_repo()
            .map(PathBuf::from)
            .ok_or_else(|| {
                format!(
                    "Project '{}' has no primary repo configured. Run `midtown start` from that repo first.",
                    name
                )
            })?;
        let primary_str = primary_repo.to_string_lossy().to_string();
        let additional_repos = full
            .project
            .repos()
            .into_iter()
            .filter(|repo| *repo != primary_str)
            .map(PathBuf::from)
            .collect();
        return Ok(AttachContext {
            project_name: name.to_string(),
            primary_repo,
            additional_repos,
        });
    }

    let primary_repo = repo_root()?;
    let project_name = resolve_project_name(&None).unwrap_or_else(|| {
        primary_repo
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });
    let additional_repos = resolve_repos(&[], &project_name);
    Ok(AttachContext {
        project_name,
        primary_repo,
        additional_repos,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttachHost {
    Tmux,
    Zellij,
    Ghostty,
    ITerm,
    Unknown,
}

impl AttachHost {
    pub(super) fn detect() -> Self {
        if std::env::var("ZELLIJ").is_ok() {
            return Self::Zellij;
        }
        if std::env::var("TMUX").is_ok() {
            return Self::Tmux;
        }

        let term_program = std::env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if term_program == "ghostty" {
            return Self::Ghostty;
        }
        if term_program == "iterm.app" {
            return Self::ITerm;
        }

        let lc_terminal = std::env::var("LC_TERMINAL")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if lc_terminal == "iterm2" {
            return Self::ITerm;
        }

        Self::Unknown
    }
}

fn shell_quote(input: &str) -> String {
    let escaped = input.replace('\'', "'\"'\"'");
    format!("'{}'", escaped)
}

fn escape_applescript_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse_ghostty_keybind_for_action(list_keybinds_output: &str, action: &str) -> Option<String> {
    for line in list_keybinds_output.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("keybind = ") else {
            continue;
        };
        let Some((binding, bound_action)) = rest.split_once('=') else {
            continue;
        };
        if bound_action.trim() == action {
            return Some(binding.trim().to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn is_accessibility_permission_error(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("not allowed to send keystrokes") || lower.contains("(1002)")
}

#[cfg(target_os = "macos")]
fn is_automation_permission_error(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("not authorized to send apple events") || lower.contains("(-1743)")
}

#[cfg(target_os = "macos")]
fn maybe_open_permission_settings(stderr: &str) -> Option<String> {
    let needs_accessibility = is_accessibility_permission_error(stderr);
    let needs_automation = is_automation_permission_error(stderr);
    if !needs_accessibility && !needs_automation {
        return None;
    }

    if !ACCESSIBILITY_SETTINGS_OPENED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        if needs_accessibility {
            let _ = Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status();
        }
        if needs_automation {
            let _ = Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
                .status();
        }
    }

    Some(
        "Ghostty does not expose a direct API to run commands in a new split, so Midtown uses \
macOS System Events to send your split keybinding and type the Lead command. \
This can require both Accessibility ('control your computer') and Automation permission. \
Opened the relevant System Settings privacy panes. \
If Ghostty does not appear automatically, use '+' in Accessibility to add Ghostty.app \
and the app running `midtown`, then rerun `midtown view`."
            .to_string(),
    )
}

fn trigger_ghostty_keybinding(binding: &str) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let mut modifiers: Vec<&str> = Vec::new();
        let mut key_token: Option<String> = None;

        for token in binding
            .split('+')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            match token.to_ascii_lowercase().as_str() {
                "super" | "cmd" | "command" => modifiers.push("command down"),
                "shift" => modifiers.push("shift down"),
                "alt" | "option" => modifiers.push("option down"),
                "ctrl" | "control" => modifiers.push("control down"),
                other => {
                    if key_token.is_some() {
                        return Ok(false);
                    }
                    key_token = Some(other.to_string());
                }
            }
        }

        let Some(key_token) = key_token else {
            return Ok(false);
        };

        let using_clause = if modifiers.is_empty() {
            String::new()
        } else {
            format!(" using {{{}}}", modifiers.join(", "))
        };

        let key_event = if key_token == "enter" {
            format!(
                "tell application \"System Events\" to key code 36{}",
                using_clause
            )
        } else {
            let key_text = if let Some(digit) = key_token.strip_prefix("digit_") {
                if digit.len() == 1 {
                    digit.to_string()
                } else {
                    return Ok(false);
                }
            } else if key_token.chars().count() == 1 {
                key_token
            } else {
                return Ok(false);
            };

            format!(
                "tell application \"System Events\" to keystroke \"{}\"{}",
                escape_applescript_string(&key_text),
                using_clause
            )
        };
        let script = format!(
            "tell application \"Ghostty\" to activate\n\
             delay 0.05\n\
             {}\n\
             delay 0.05",
            key_event
        );

        let output = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| format!("Failed to trigger Ghostty keybinding via osascript: {}", e))?;
        if output.status.success() {
            return Ok(true);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(msg) = maybe_open_permission_settings(&stderr) {
            return Err(msg);
        }

        Ok(false)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = binding;
        Ok(false)
    }
}

fn trigger_ghostty_split_action() -> Result<bool, String> {
    if let Ok(output) = Command::new("ghostty").arg("+list-keybinds").output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for action in ["new_split:right", "new_split_right", "new_split"] {
            if let Some(binding) = parse_ghostty_keybind_for_action(&stdout, action)
                && trigger_ghostty_keybinding(&binding)?
            {
                return Ok(true);
            }
        }
    }

    for action in ["new_split_right", "new_split:right", "new_split"] {
        let status = Command::new("ghostty")
            .args(["+action", action])
            .status()
            .map_err(|e| format!("Failed to run ghostty split action '{}': {}", action, e))?;
        if status.success() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn launch_iterm_split(cwd: &str, shell_command: &str) -> Result<bool, String> {
    let typed_cmd = format!("cd {} && {}", shell_quote(cwd), shell_command);
    let script = format!(
        r#"tell application \"iTerm2\"
    if (count of windows) = 0 then
        create window with default profile
    end if
    tell current window
        tell current session
            set newSession to (split horizontally with default profile)
            tell newSession
                write text \"{}\"
            end tell
        end tell
    end tell
end tell"#,
        escape_applescript_string(&typed_cmd)
    );

    let status = Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map_err(|e| format!("Failed to run osascript for iTerm split: {}", e))?;
    Ok(status.success())
}

pub(super) fn launch_lead_split(
    host: AttachHost,
    cwd: &str,
    shell_command: &str,
) -> Result<String, String> {
    match host {
        AttachHost::Tmux => {
            let status = Command::new("tmux")
                .args(["split-window", "-h", "-c", cwd, "sh", "-lc", shell_command])
                .status()
                .map_err(|e| format!("Failed to run tmux split-window: {}", e))?;
            if !status.success() {
                return Err("tmux split-window failed".to_string());
            }
            Ok("tmux split pane".to_string())
        }
        AttachHost::Zellij => {
            let status = Command::new("zellij")
                .args([
                    "action",
                    "new-pane",
                    "-d",
                    "right",
                    "--cwd",
                    cwd,
                    "--",
                    "sh",
                    "-lc",
                    shell_command,
                ])
                .status()
                .map_err(|e| format!("Failed to run zellij action new-pane: {}", e))?;
            if !status.success() {
                return Err("zellij action new-pane failed".to_string());
            }
            Ok("zellij split pane".to_string())
        }
        AttachHost::Ghostty => {
            if !trigger_ghostty_split_action()? {
                return Err(
                    "ghostty split action failed (tried keybind dispatch and known action names)"
                        .to_string(),
                );
            }

            #[cfg(target_os = "macos")]
            {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let typed_cmd = format!("cd {} && {}", shell_quote(cwd), shell_command);
                let script = format!(
                    "tell application \"Ghostty\" to activate\n\
                     delay 0.05\n\
                     tell application \"System Events\" to keystroke \"{}\"\n\
                     tell application \"System Events\" to key code 36",
                    escape_applescript_string(&typed_cmd)
                );
                let output = Command::new("osascript")
                    .args(["-e", &script])
                    .output()
                    .map_err(|e| format!("Failed to dispatch lead command to Ghostty: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if let Some(msg) = maybe_open_permission_settings(&stderr) {
                        return Err(msg);
                    }
                    return Err(format!(
                        "Ghostty split opened but failed to send lead command: {}",
                        stderr.trim()
                    ));
                }
                Ok("ghostty split pane".to_string())
            }

            #[cfg(not(target_os = "macos"))]
            {
                Err(
                    "Ghostty split command injection is currently only supported on macOS"
                        .to_string(),
                )
            }
        }
        AttachHost::ITerm => {
            if cfg!(target_os = "macos") && launch_iterm_split(cwd, shell_command)? {
                return Ok("iTerm split pane".to_string());
            }
            Err("iTerm split launch failed".to_string())
        }
        AttachHost::Unknown => Err(
            "Unsupported terminal host for automatic split. Use zellij/tmux/ghostty/iTerm."
                .to_string(),
        ),
    }
}

/// Handle `midtown view` command.
///
/// Starts `midtown chat` in the current terminal and auto-creates a split
/// that attaches to the Lead's headless session.
pub fn handle_view(project: Option<&str>, skip_auto_split: bool) -> Result<Response, String> {
    let ctx = resolve_attach_context(project)?;

    // Ensure project-scoped socket resolution uses the target project's repo root.
    let original_cwd = std::env::current_dir().ok();
    std::env::set_current_dir(&ctx.primary_repo).map_err(|e| {
        format!(
            "Failed to switch to project repo '{}': {}",
            ctx.primary_repo.display(),
            e
        )
    })?;

    // Ensure daemon + lead are running for this project.
    if !daemon_is_running() {
        handle_start(Some(ctx.project_name.clone()), ctx.additional_repos.clone())?;
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    // Attach to the headless lead session via the daemon RPC.
    let client =
        DaemonClient::connect().map_err(|e| format!("Failed to connect to daemon: {}", e))?;

    // Wait for lead session to become attachable (it may still be initializing).
    let mut attach_info = None;
    for _ in 0..50 {
        match client.session_attach("name/lead") {
            Ok(info) => {
                attach_info = Some(info);
                break;
            }
            Err(e) if e.contains("No session ID found") || e.contains("matched no persisted") => {
                // Lead session not yet registered — wait and retry
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            Err(e) if e.contains("already attached") => {
                // Previous view session exited without detaching — clean up and retry
                let _ = client.session_detach("lead");
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            Err(e) => {
                if let Some(cwd) = original_cwd {
                    let _ = std::env::set_current_dir(cwd);
                }
                return Err(format!("Failed to attach to lead session: {}", e));
            }
        }
    }

    let info = match attach_info {
        Some(info) => info,
        None => {
            if let Some(cwd) = original_cwd {
                let _ = std::env::set_current_dir(cwd);
            }
            return Err(
                "Lead session not available for attach. Try again in a few seconds.".to_string(),
            );
        }
    };

    let session_id = info
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Daemon did not return session_id")?;
    let cwd = info
        .get("cwd")
        .and_then(|v| v.as_str())
        .ok_or("Daemon did not return cwd")?;
    let provider_str = info
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude");
    let provider = provider_str
        .parse::<midtown::auth::AuthProvider>()
        .unwrap_or(midtown::auth::AuthProvider::Claude);

    if let Err(e) = midtown::platform_launch::run_platform_prelaunch_hook(provider) {
        eprintln!(
            "Warning: Platform pre-launch hook failed (continuing): {}",
            e
        );
    }

    let cwd = super::session::ensure_attach_worktree("lead", cwd)?;
    let lead_shell_command =
        super::session::build_attach_shell_command(&cwd, "lead", provider, session_id)?;

    let host = AttachHost::detect();

    if !skip_auto_split && let Err(e) = launch_lead_split(host, &cwd, &lead_shell_command) {
        // Detach so the lead resumes headless
        let _ = client.session_detach("lead");
        if let Some(cwd) = original_cwd {
            let _ = std::env::set_current_dir(cwd);
        }
        return Err(format!(
            "Failed to create Lead split. Chat was not started.\n{}\n\n\
If you want chat without automatic split creation, run:\n  midtown view --skip-auto-split",
            e
        ));
    }

    let chat_result = super::chat::run();

    // Always detach on exit so the daemon resumes headless mode.
    // Without this, a normal exit leaves the lead in `attached_coworkers`
    // and subsequent `midtown view` calls fail with "already attached".
    let _ = client.session_detach("lead");

    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    chat_result?;
    Ok(Response::message("Exited chat session"))
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
    fn test_parse_ghostty_keybind_for_action_finds_binding() {
        let output = "keybind = super+shift+d=new_split:down\nkeybind = super+d=new_split:right\n";
        assert_eq!(
            parse_ghostty_keybind_for_action(output, "new_split:right"),
            Some("super+d".to_string())
        );
    }

    #[test]
    fn test_parse_ghostty_keybind_for_action_returns_none_when_missing() {
        let output = "keybind = super+shift+d=new_split:down\n";
        assert_eq!(
            parse_ghostty_keybind_for_action(output, "new_split:right"),
            None
        );
    }

    #[test]
    fn test_ghostty_split_action_works_when_running_inside_ghostty() {
        let term_program = std::env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if term_program != "ghostty" {
            eprintln!("Skipping: not running inside Ghostty.");
            return;
        }
        if std::env::var("MIDTOWN_RUN_GHOSTTY_SPLIT_TEST").unwrap_or_default() != "1" {
            eprintln!("Skipping: set MIDTOWN_RUN_GHOSTTY_SPLIT_TEST=1 to enable.");
            return;
        }

        let status = trigger_ghostty_split_action().expect("ghostty split action dispatch failed");
        assert!(
            status,
            "Expected a Ghostty split action/keybind to succeed when running inside Ghostty."
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_accessibility_permission_error_detection() {
        assert!(is_accessibility_permission_error(
            "System Events got an error: osascript is not allowed to send keystrokes. (1002)"
        ));
        assert!(is_automation_permission_error(
            "Not authorized to send Apple events to System Events. (-1743)"
        ));
        assert!(is_accessibility_permission_error(
            "System Events got an error: osascript is not allowed to send keystrokes. (1002)"
        ));
        assert!(!is_automation_permission_error(
            "failed to initialize ghostty error=error.InvalidAction"
        ));
        assert!(!is_accessibility_permission_error(
            "failed to initialize ghostty error=error.InvalidAction"
        ));
    }

    #[test]
    fn test_drain_status_check_recognizes_valid_statuses() {
        // Verify that the drain loop correctly identifies which CoworkerStatus
        // values should be considered "done" vs. "still working".
        //
        // CoworkerStatus enum has: Starting, Running, Stopping, Stopped
        // (no Idle variant - confusion with WorkflowPhase::Idle)
        //
        // "stopped" and "stopping" should be considered done.
        // "starting" and "running" should be considered still working.

        let done_statuses = vec!["stopped", "stopping"];
        let working_statuses = vec!["starting", "running"];

        for status in &done_statuses {
            // These should NOT trigger all_done = false
            let is_working = *status != "stopped" && *status != "stopping";
            assert!(
                !is_working,
                "Status '{}' should be considered done, but was marked as working",
                status
            );
        }

        for status in &working_statuses {
            // These SHOULD trigger all_done = false
            let is_working = *status != "stopped" && *status != "stopping";
            assert!(
                is_working,
                "Status '{}' should be considered working, but was marked as done",
                status
            );
        }

        // Verify that "idle" is not a valid status (it doesn't exist in CoworkerStatus)
        // This test documents the confusion between CoworkerStatus and WorkflowPhase.
        let invalid_status = "idle";
        let is_working = invalid_status != "stopped" && invalid_status != "stopping";
        assert!(
            is_working,
            "Status 'idle' doesn't exist in CoworkerStatus — if it appeared, it would be conservatively treated as 'working' (safe default for unknown statuses)"
        );
    }
}

#[path = "daemon_tests.rs"]
#[cfg(test)]
mod daemon_tests;
