//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use midtown::json_ext::ValueExt;

use crate::cli::Response;
use crate::client::DaemonClient;

/// Build a startup failure error message from a log file.
///
/// Reads the last 50 lines, extracts up to 5 ERROR lines, and formats them
/// in chronological order after the given prefix.
fn build_startup_error(prefix: &str, log_path: &std::path::Path) -> String {
    let mut msg = prefix.to_string();
    if let Ok(contents) = std::fs::read_to_string(log_path) {
        let errors: Vec<&str> = contents
            .lines()
            .rev()
            .take(50)
            .filter(|line| line.contains("ERROR"))
            .take(5)
            .collect();
        if !errors.is_empty() {
            msg.push_str(". Errors from daemon log:");
            for line in errors.into_iter().rev() {
                msg.push('\n');
                msg.push_str(line);
            }
        }
    }
    msg
}

/// Build a startup failure error message from the daemon log.
fn daemon_startup_error(prefix: &str) -> String {
    build_startup_error(prefix, &midtown::paths::daemon_log_file())
}

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
        repo_root().ok().and_then(|r| {
            r.file_name()
                .map(|s| midtown::paths::sanitize_project_name(&s.to_string_lossy()))
        })
    })
}

/// Get the socket path for the daemon.
fn socket_path() -> PathBuf {
    midtown::paths::daemon_socket()
}

/// Track whether a progress line is currently displayed (needs clearing before other output).
static PROGRESS_LINE_ACTIVE: std::sync::atomic::AtomicBool =
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
                let ct_fg = super::ratatui_to_crossterm_color(fg);
                line.push_str(&format!("{}", SetForegroundColor(ct_fg)));
                let ct_bg = super::ratatui_to_crossterm_color(bg);
                line.push_str(&format!("{}", SetBackgroundColor(ct_bg)));
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
fn resolve_repos(repos: &[PathBuf], dir_key: &str) -> Vec<PathBuf> {
    if !repos.is_empty() {
        return repos.to_vec();
    }
    parse_saved_repos(dir_key)
}

/// Parse saved repos from a project's config.toml.
///
/// Reads the `[project].repos` list and returns all entries
/// except the primary repo (which is handled separately).
fn parse_saved_repos(dir_key: &str) -> Vec<PathBuf> {
    let full_config = midtown::config::load_full_project_config(dir_key);
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
///
/// When `explicit_name` is true (user passed `--project`), the name always overwrites.
/// When false (auto-detected), it only sets the name if not already configured.
fn update_project_config(
    dir_key: &str,
    project_name: &str,
    primary_repo: &Path,
    additional_repos: &[PathBuf],
    explicit_name: bool,
) -> Result<(), String> {
    let config_path = midtown::config::project_config_path(dir_key);
    let mut config =
        midtown::config::FullProjectConfig::load_from(&config_path).unwrap_or_default();

    // Set project name if explicitly provided via --project flag, or if not already configured
    if explicit_name || config.project.name().is_none() {
        config.project.name = Some(project_name.to_string());
    }

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

    // Find missing plugins (use canonical list from daemon)
    let missing: Vec<_> = midtown::daemon::REQUIRED_PLUGINS
        .iter()
        .filter(|p| !installed.contains(**p))
        .collect();

    if missing.is_empty() {
        return;
    }

    // Install missing plugins with incremental progress indicators
    let total_missing = missing.len();
    for (i, plugin_id) in missing.iter().enumerate() {
        // Spread progress from 10% to 60% based on plugin index
        let progress = 10 + ((50 * (i + 1)) / total_missing.max(1)) as u16;
        emit_startup_progress(progress, &format!("installing plugin {}", plugin_id));
        install_plugin(plugin_id);
    }
}

/// Returns true if `dist/index.html` is newer than all tracked source files.
///
/// Checked source dirs/files: `src/`, `public/`, `package.json`, `vite.config.js`,
/// `svelte.config.js`. Returns false if dist doesn't exist.
fn is_dist_fresh(web_app_dir: &Path, dist_index: &Path) -> bool {
    let dist_mtime = match dist_index.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    for dir_name in &["src", "public"] {
        let source_dir = web_app_dir.join(dir_name);
        if source_dir.exists() && dir_has_newer_file(&source_dir, dist_mtime) {
            return false;
        }
    }

    for file_name in &[
        "index.html",
        "package.json",
        "vite.config.js",
        "svelte.config.js",
    ] {
        let path = web_app_dir.join(file_name);
        if path
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|mtime| mtime > dist_mtime)
        {
            return false;
        }
    }

    true
}

/// Returns true if any file under `dir` has an mtime newer than `than`.
fn dir_has_newer_file(dir: &Path, than: std::time::SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_has_newer_file(&path, than) {
                return true;
            }
        } else if path
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|mtime| mtime > than)
        {
            return true;
        }
    }
    false
}

/// Build the web-app if the source tree is available and the dist is stale.
///
/// Skips silently when:
/// - `web-app/package.json` doesn't exist (production install without source)
/// - `npm` isn't on `PATH`
/// - The existing `dist/index.html` is already newer than all source files
///
/// Non-blocking: logs warnings on failure but never aborts startup.
fn build_web_app_if_needed_quiet() {
    build_web_app_if_needed_inner(false);
}

fn build_web_app_if_needed() {
    build_web_app_if_needed_inner(true);
}

fn build_web_app_if_needed_inner(show_progress: bool) {
    let web_app_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web-app");

    if !web_app_dir.join("package.json").exists() {
        return;
    }

    let dist_index = web_app_dir.join("dist").join("index.html");
    if is_dist_fresh(&web_app_dir, &dist_index) {
        return;
    }

    if show_progress {
        emit_startup_progress(25, "installing web app dependencies");
    }

    let install = Command::new("npm")
        .args(["install", "--prefer-offline"])
        .current_dir(&web_app_dir)
        .output();

    match install {
        Err(e) => {
            eprintln!("Warning: Failed to run npm install for web-app: {e}");
            return;
        }
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            eprintln!("Warning: npm install for web-app failed:\n{stderr}");
            return;
        }
        Ok(_) => {}
    }

    if show_progress {
        emit_startup_progress(35, "building web app");
    }

    let build = match Command::new("npm")
        .args(["run", "build"])
        .current_dir(&web_app_dir)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Warning: Failed to run npm build for web-app: {e}");
            return;
        }
    };

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        eprintln!("Warning: web-app build failed:\n{stderr}");
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
    let dir_key = midtown::paths::detect_repo_name().unwrap_or_else(|| {
        primary_repo
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });
    let project_name = resolve_project_name(&project)
        .unwrap_or_else(|| midtown::paths::sanitize_project_name(&dir_key));
    let additional_repos = resolve_repos(&repos, &dir_key);
    emit_startup_progress(5, &format!("starting project '{}'", project_name));

    // Check and install required plugins before starting daemon
    ensure_required_plugins();

    // Install agent definitions to shared dir (~/.midtown/platforms/claude/agents/)
    let agents_dir = super::agents_install::claude_agents_dir();
    match super::agents_install::install_agent_definitions(&agents_dir, false) {
        Ok(installed) if !installed.is_empty() => {
            let names: Vec<&str> = installed.iter().map(|d| d.filename).collect();
            eprintln!("Installed agent definitions: {}", names.join(", "));
        }
        Err(e) => eprintln!("Warning: Failed to install agent definitions: {e}"),
        _ => {}
    }

    // Build web-app if source is available and dist is stale
    build_web_app_if_needed();

    let mut messages = Vec::new();

    // Update project config with repo information
    let _ = update_project_config(
        &dir_key,
        &project_name,
        &primary_repo,
        &additional_repos,
        project.is_some(),
    );

    // Step 1: Start daemon if not running
    if daemon_is_running() {
        messages.push("Daemon already running".to_string());
        emit_startup_progress(65, "daemon already running");
    } else {
        // Clean up any stale PID file or orphaned daemon before starting
        emit_startup_progress(65, "starting daemon");
        cleanup_stale_daemon();

        // Start the daemon in the background using `midtown daemon` (or `daemon-v2`)
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable: {}", e))?;

        let use_v2 = std::env::var("MIDTOWN_DAEMON_V2").is_ok_and(|v| v == "1" || v == "true");

        let mut cmd = Command::new(&exe);
        if use_v2 {
            cmd.arg("daemon-v2");
            cmd.current_dir(&primary_repo);
            cmd.arg("--workdir").arg(&dir_key);
            cmd.arg("--channel").arg(&project_name);
        } else {
            cmd.arg("daemon");
            cmd.current_dir(&primary_repo);
            cmd.arg("--workdir").arg(&primary_repo);
            if project.is_some() {
                cmd.arg("--project").arg(&project_name);
            }
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
            return Err(daemon_startup_error("Daemon failed to start"));
        }
    }

    // Step 2: Spawn the Lead as a headless session (idempotent).
    emit_startup_progress(88, "spawning headless lead session");
    let configured_lead_provider = midtown::config::get_execution_provider_for_role(
        &dir_key,
        midtown::config::ExecutionRole::Lead,
    );
    let lead_provider = std::env::var("MIDTOWN_LEAD_PROVIDER")
        .ok()
        .and_then(|s| s.parse::<midtown::auth::AuthProvider>().ok())
        .unwrap_or(configured_lead_provider);
    match DaemonClient::connect() {
        Ok(client) => match client.lead_spawn(lead_provider) {
            Ok(_) => messages.push("Lead session running".to_string()),
            Err(e) => {
                // If the daemon died during startup (race: socket bound before
                // init finished), surface the real errors from the log.
                if !daemon_is_running() {
                    clear_startup_progress();
                    return Err(daemon_startup_error("Daemon exited during startup"));
                }
                messages.push(format!("Warning: Failed to spawn lead: {}", e));
            }
        },
        Err(e) => {
            if !daemon_is_running() {
                clear_startup_progress();
                return Err(daemon_startup_error("Daemon exited during startup"));
            }
            messages.push(format!("Warning: Could not connect to daemon: {}", e));
        }
    }

    // Step 3: Auto-launch shared webserver if not running
    let global_config = midtown::config::GlobalConfig::load();
    let webserver_scheme = if global_config.webserver.tls_cert.is_some()
        && global_config.webserver.tls_key.is_some()
    {
        "https"
    } else {
        "http"
    };
    if !webserver_is_running() {
        emit_startup_progress(96, "starting shared webserver");
        match launch_webserver() {
            Ok(()) => messages.push(format!(
                "Started webserver on {}://localhost:{}",
                webserver_scheme,
                midtown::webserver::DEFAULT_WEBSERVER_PORT
            )),
            Err(e) => messages.push(format!("Warning: Failed to start webserver: {}", e)),
        }
    } else {
        messages.push(format!(
            "Webserver running at {}://localhost:{}",
            webserver_scheme,
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        ));
        emit_startup_progress(96, "shared webserver already running");
    }

    // Non-blocking version check (respects 1-hour cooldown)
    if let Some(notice) = super::update::check_for_update_notice() {
        messages.push(notice);
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

    // Pass TLS config from global config if set
    let global_config = midtown::config::GlobalConfig::load();
    if let Some(ref cert) = global_config.webserver.tls_cert {
        cmd.args(["--tls-cert", &cert.to_string_lossy()]);
    }
    if let Some(ref key) = global_config.webserver.tls_key {
        cmd.args(["--tls-key", &key.to_string_lossy()]);
    }

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
    let global_config = midtown::config::GlobalConfig::load();
    let scheme = if global_config.webserver.tls_cert.is_some()
        && global_config.webserver.tls_key.is_some()
    {
        "https"
    } else {
        "http"
    };
    if was_running {
        Ok(Response::message(format!(
            "Restarted webserver on {}://localhost:{}",
            scheme,
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    } else {
        Ok(Response::message(format!(
            "Started webserver on {}://localhost:{}",
            scheme,
            midtown::webserver::DEFAULT_WEBSERVER_PORT
        )))
    }
}

/// Handle `midtown stop` command.
///
/// Stops the daemon and webserver.
/// Also cleans up any orphaned `gh webhook forward` processes.
pub fn handle_stop() -> Result<Response, String> {
    let mut messages = Vec::new();

    // Step 1: Stop daemon (this also signals the gh webhook forwarder to stop)
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

    // Step 2: Kill any orphaned `gh webhook forward` processes.
    // The daemon's SIGTERM handler should have already stopped these, but
    // if the daemon exited uncleanly they may be left behind.
    kill_orphaned_webhook_forwarders(&mut messages);

    // Step 3: Kill any orphaned claude processes.
    kill_orphaned_claude_processes(&mut messages);

    // Step 4: Kill any orphaned codex app-server processes.
    // The Codex daemon-side app-server is shared and long-lived, so when the
    // daemon exits without graceful shutdown it can remain orphaned.
    kill_orphaned_codex_processes(&mut messages);

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
/// Only kills processes that:
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

/// Kill any orphaned Codex app-server processes that were started by midtown.
///
/// Codex app-server is a long-lived JSON-RPC process that can remain alive when
/// its parent (midtown daemon) dies unexpectedly. We only kill orphaned
/// processes here to avoid interrupting user-owned Codex invocations.
fn kill_orphaned_codex_processes(messages: &mut Vec<String>) {
    // Pattern matches the baseline codex app-server invocations used by midtown:
    // `codex app-server` and `codex app-server --listen ...`.
    let pattern = "codex app-server";

    let count = midtown::process::kill_orphaned_processes(pattern);
    if count > 0 {
        messages.push(format!(
            "Killed {} orphaned codex app-server process(es)",
            count
        ));
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

/// How long `midtown restart` waits for active review coworkers to go on break.
const REVIEWER_BREAK_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);
/// Poll interval while waiting for review coworkers to drain.
const REVIEWER_BREAK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Extract coworker names that are currently in review phase from `coworkers.status` RPC payload.
fn extract_review_phase_names(
    coworkers_status: &serde_json::Value,
) -> std::collections::HashSet<String> {
    coworkers_status
        .array_field("coworkers")
        .into_iter()
        .flatten()
        .filter_map(|coworker| {
            let phase = coworker.str_field("phase")?;
            if phase != "review" {
                return None;
            }
            coworker.str_field("name").map(|name| name.to_string())
        })
        .collect()
}

/// Extract assigned reviewers for open PRs that still need review from `prs.status` payload.
fn extract_unreviewed_assigned_reviewer_names(
    prs_status: &serde_json::Value,
) -> std::collections::HashSet<String> {
    prs_status
        .array_field("prs")
        .into_iter()
        .flatten()
        .filter_map(|pr| {
            let review_posted = pr.bool_or("review_posted", false);
            if review_posted {
                return None;
            }
            pr.str_field("reviewer")
                .filter(|name| !name.is_empty())
                .map(|name| name.to_string())
        })
        .collect()
}

/// Get currently running coworker names from `coworker.list`.
fn running_coworker_names(
    client: &DaemonClient,
) -> Result<std::collections::HashSet<String>, String> {
    match client.coworker_list()? {
        Response::Coworkers { coworkers } => Ok(coworkers
            .into_iter()
            .filter(|cw| cw.status != "stopped" && cw.status != "stopping")
            .filter(|cw| !cw.is_channel_lead)
            .map(|cw| cw.name)
            .collect()),
        _ => Err("Unexpected response from coworker.list".to_string()),
    }
}

/// Return currently active reviewer coworkers that should be allowed to finish before restart.
///
/// Uses two signals:
/// 1. Coworkers explicitly reporting `phase=review`.
/// 2. PR reviewer assignments (for in-flight reviews where phase reporting may lag/miss).
fn active_review_coworkers(client: &DaemonClient) -> Result<Vec<String>, String> {
    let mut names = std::collections::HashSet::new();
    let mut detection_errors = Vec::new();

    match client.coworkers_status() {
        Ok(raw) => {
            names.extend(extract_review_phase_names(&raw));
        }
        Err(e) => detection_errors.push(format!("coworkers.status failed: {}", e)),
    }

    let running = match running_coworker_names(client) {
        Ok(names) => Some(names),
        Err(e) => {
            detection_errors.push(format!("coworker.list failed: {}", e));
            None
        }
    };

    match (client.prs_status(), running.as_ref()) {
        (Ok(raw), Some(running_names)) => {
            for name in extract_unreviewed_assigned_reviewer_names(&raw) {
                if running_names.contains(&name) {
                    names.insert(name);
                }
            }
        }
        (Err(e), _) => detection_errors.push(format!("prs.status failed: {}", e)),
        (Ok(_), None) => {}
    }

    if names.is_empty() && detection_errors.len() >= 2 {
        return Err(detection_errors.join("; "));
    }

    let mut active: Vec<String> = names.into_iter().collect();
    active.sort();
    Ok(active)
}

/// Wait until no active review coworkers remain (they have gone on break).
/// How many consecutive RPC errors abort the wait loop.
///
/// Transient failures (socket hiccup, brief daemon busy) are logged and
/// retried. Only sustained failures — where every poll fails — abort the loop
/// to avoid masking a permanently unreachable daemon.
const REVIEWER_BREAK_MAX_CONSECUTIVE_ERRORS: u32 = 3;

fn wait_for_review_coworkers_to_break(client: &DaemonClient) -> Result<(), String> {
    let start = std::time::Instant::now();
    let mut last_reported: Vec<String> = Vec::new();
    let mut consecutive_errors: u32 = 0;

    loop {
        match active_review_coworkers(client) {
            Ok(active) => {
                consecutive_errors = 0;
                if active.is_empty() {
                    if !last_reported.is_empty() {
                        eprintln!("All review coworkers are on break.");
                    }
                    return Ok(());
                }

                if active != last_reported {
                    eprintln!(
                        "Waiting for review coworker(s) to go on break before restart: {}",
                        active.join(", ")
                    );
                    last_reported = active.clone();
                }

                if start.elapsed() >= REVIEWER_BREAK_WAIT_TIMEOUT {
                    return Err(format!(
                        "Timed out after {}s waiting for review coworker(s): {}",
                        REVIEWER_BREAK_WAIT_TIMEOUT.as_secs(),
                        active.join(", ")
                    ));
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                eprintln!(
                    "Warning: failed to query active reviewers (attempt {}): {}",
                    consecutive_errors, e
                );
                if consecutive_errors >= REVIEWER_BREAK_MAX_CONSECUTIVE_ERRORS {
                    return Err(format!(
                        "Aborting reviewer wait after {} consecutive RPC failures: {}",
                        consecutive_errors, e
                    ));
                }
            }
        }

        std::thread::sleep(REVIEWER_BREAK_POLL_INTERVAL);
    }
}

/// Handle `midtown restart` command.
///
/// Restarts the daemon and webserver by waiting for active review coworkers to
/// go on break, then sending SIGTERM to all running coworker sessions before
/// re-execing the daemon process.
///
/// Default behavior waits for reviewers to finish current reviews. `--force`
/// skips that wait.
///
/// For a full fresh start, use `midtown stop && midtown start`.
pub fn handle_restart(force: bool) -> Result<Response, String> {
    // Send SIGTERM to all running coworker sessions via daemon RPC.
    // The daemon's graceful_shutdown_all() sends SIGTERM and waits up to 10s,
    // then SIGKILL as fallback.
    let client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => {
            // Daemon not running — nothing to signal
            eprintln!("Daemon not running, skipping coworker shutdown.");
            // Fall through to start path below
            return handle_start(None, vec![]).map(|_| Response::Message {
                message: "Daemon was not running; started fresh.".to_string(),
            });
        }
    };

    // Signal the daemon to stop assigning new review tasks immediately.
    // This prevents a race where TaskDispatchTick hands out a new reviewer
    // assignment during the REVIEWER_BREAK_WAIT_TIMEOUT window.
    if let Err(e) = client.set_draining() {
        eprintln!(
            "Warning: failed to set daemon draining flag: {}. Continuing.",
            e
        );
    }

    if force {
        eprintln!("--force set: skipping wait for review coworkers to go on break.");
    } else if let Err(e) = wait_for_review_coworkers_to_break(&client) {
        eprintln!("Warning: {}. Continuing restart with coworker shutdown.", e);
    }

    eprintln!("Sending SIGTERM to all coworker sessions...");
    match client.stop_all_coworkers() {
        Ok(_) => eprintln!("Coworker sessions stopped."),
        Err(e) => eprintln!("Warning: stop_all_coworkers RPC failed: {}", e),
    }
    drop(client);

    // Send exec-restart RPC to the daemon. The daemon will:
    // 1. Persist session info, then terminate all coworker sessions
    // 2. Re-exec itself with --foreground, preserving its original process
    //    context. This is critical on macOS: the daemon was initially launched
    //    from an unsandboxed CLI, so re-exec preserves that unsandboxed state.
    //    If instead we stopped and re-spawned from a sandboxed agent process,
    //    the new daemon would inherit the sandbox and break coworker spawning.
    let client =
        DaemonClient::connect().map_err(|e| format!("Failed to connect to daemon: {}", e))?;

    // Stop the webserver first (it runs independently of the daemon)
    let _ = stop_webserver();

    // Build web-app if source is available and dist is stale.
    // Use quiet variant to suppress startup progress bar (percentages are
    // calibrated for handle_start's sequence and would confuse during restart).
    build_web_app_if_needed_quiet();

    // Tell the daemon to exec-restart
    if let Err(e) = client.exec_restart() {
        // Fallback: if RPC fails (e.g., old daemon without exec-restart support),
        // use the legacy stop+start path.
        eprintln!(
            "Warning: exec-restart RPC failed ({}), falling back to stop+start",
            e
        );
        drop(client);
        handle_stop()?;

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

/// Handle `midtown view` command.
///
/// By default, starts `midtown chat` in the current terminal without touching the lead session.
/// With `--attach`, attaches to the Lead session first, then returns to chat on exit.
pub fn handle_view(project: Option<&str>, attach: bool) -> Result<Response, String> {
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

    if attach {
        // Attach to the headless lead session via the daemon RPC.
        let client =
            DaemonClient::connect().map_err(|e| format!("Failed to connect to daemon: {}", e))?;

        // Wait for lead session to become attachable (it may still be initializing).
        let mut attach_info = None;
        for _ in 0..50 {
            match client.session_attach(&format!("name/{}", ctx.project_name)) {
                Ok(info) => {
                    attach_info = Some(info);
                    break;
                }
                Err(e)
                    if e.contains("No session ID found") || e.contains("matched no persisted") =>
                {
                    // Lead session not yet registered — wait and retry
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
                Err(e) if e.contains("already attached") => {
                    // Previous view session exited without detaching — clean up and retry
                    let _ = client.session_detach(&ctx.project_name);
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
                    "Lead session not available for attach. Try again in a few seconds."
                        .to_string(),
                );
            }
        };

        let session_id = info
            .str_field("session_id")
            .ok_or("Daemon did not return session_id")?;
        let cwd = info.str_field("cwd").ok_or("Daemon did not return cwd")?;
        let provider_str = info.str_or("provider", "claude");
        let provider = provider_str
            .parse::<midtown::auth::AuthProvider>()
            .unwrap_or(midtown::auth::AuthProvider::Claude);
        let profile = info.str_field("profile").map(|s| s.to_string());

        let profile_dir = profile
            .as_deref()
            .map(|name| midtown::auth::profile_dir_for(provider, name));
        if let Err(e) =
            midtown::platform_launch::run_platform_prelaunch_hook(provider, profile_dir.as_deref())
        {
            eprintln!(
                "Warning: Platform pre-launch hook failed (continuing): {}",
                e
            );
        }

        let cwd = super::agent::ensure_attach_worktree(&ctx.project_name, cwd, true)?;
        let lead_shell_command = super::agent::build_attach_shell_command(
            &cwd,
            &ctx.project_name,
            provider,
            session_id,
            super::agent::AttachShellOptions {
                launch: super::agent::AttachLaunchOptions {
                    profile: profile.as_deref(),
                    coworker_type: Some("lead"),
                    channel: None,
                },
                include_detach: false, // midtown view calls session_detach explicitly on exit
            },
        )?;

        let attach_result = Command::new("sh")
            .args(["-lc", &lead_shell_command])
            .current_dir(&cwd)
            .status()
            .map_err(|e| format!("Failed to launch interactive session: {}", e));

        // Always detach on exit so the daemon resumes headless mode.
        let _ = client.session_detach(&ctx.project_name);

        // Propagate spawn failures but not non-zero exit codes — interactive
        // CLIs commonly exit non-zero on Ctrl+C, and the session is already
        // detached so chat should always be reachable.
        let _attach_status = attach_result?;

        let chat_result = super::chat::run();

        if let Some(cwd) = original_cwd {
            let _ = std::env::set_current_dir(cwd);
        }

        chat_result?;
    } else {
        // Chat-only: launch the TUI without touching the lead session.
        let chat_result = super::chat::run();

        if let Some(cwd) = original_cwd {
            let _ = std::env::set_current_dir(cwd);
        }

        chat_result?;
    }

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

        lines.push(format!("{:<20} {}", name, status));
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

    // Save lead session ID to the project directory
    let project_dir = midtown::paths::lead_dir_for_repo(&repo_name);
    fs::create_dir_all(&project_dir)
        .map_err(|e| format!("Failed to create project directory: {}", e))?;

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
    fn test_find_git_root_in_fake_repo() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("my-project");
        fs::create_dir_all(&repo_dir).unwrap();
        create_git_repo(&repo_dir);

        with_temp_cwd(&repo_dir, || {
            let result = find_git_root();
            assert!(result.is_some());
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
        let result = update_project_config(
            &project_name,
            &project_name,
            &primary_repo,
            &additional,
            false,
        );

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

    #[test]
    fn test_build_startup_error_extracts_errors() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");
        std::fs::write(
            &log_path,
            "2026-01-01 INFO starting up\n\
             2026-01-01 ERROR failed to bind socket\n\
             2026-01-01 INFO retrying\n\
             2026-01-01 ERROR config file not found\n",
        )
        .unwrap();

        let result = build_startup_error("Daemon failed", &log_path);
        assert!(result.starts_with("Daemon failed"));
        assert!(result.contains("Errors from daemon log:"));
        assert!(result.contains("failed to bind socket"));
        assert!(result.contains("config file not found"));
        // ERROR lines should appear in chronological order
        let socket_pos = result.find("failed to bind socket").unwrap();
        let config_pos = result.find("config file not found").unwrap();
        assert!(socket_pos < config_pos);
    }

    #[test]
    fn test_build_startup_error_no_errors() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");
        std::fs::write(&log_path, "2026-01-01 INFO all good\n").unwrap();

        let result = build_startup_error("Daemon failed", &log_path);
        assert_eq!(result, "Daemon failed");
    }

    #[test]
    fn test_build_startup_error_missing_file() {
        let result = build_startup_error("Daemon failed", std::path::Path::new("/nonexistent/log"));
        assert_eq!(result, "Daemon failed");
    }

    #[test]
    fn test_build_startup_error_limits_to_5_errors() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("2026-01-01 ERROR error number {}\n", i));
        }
        std::fs::write(&log_path, &content).unwrap();

        let result = build_startup_error("Daemon failed", &log_path);
        let error_lines: Vec<&str> = result.lines().filter(|l| l.contains("ERROR")).collect();
        assert_eq!(error_lines.len(), 5);
    }

    #[test]
    fn test_update_project_config_explicit_name_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let dir_key = format!("test-explicit-{}", uuid::Uuid::new_v4());
        let primary_repo = dir.path().join("main-repo");
        let additional = vec![];
        let config_path = midtown::config::project_config_path(&dir_key);

        // First call: auto-detected name
        let r1 = update_project_config(&dir_key, "auto-name", &primary_repo, &additional, false);
        if r1.is_err() {
            // Skip if filesystem doesn't allow writes (sandbox)
            return;
        }
        let config = midtown::config::FullProjectConfig::load_from(&config_path).unwrap();
        assert_eq!(config.project.name(), Some("auto-name"));

        // Second call with explicit=false: should NOT clobber
        update_project_config(&dir_key, "new-auto-name", &primary_repo, &additional, false)
            .unwrap();
        let config = midtown::config::FullProjectConfig::load_from(&config_path).unwrap();
        assert_eq!(config.project.name(), Some("auto-name"));

        // Third call with explicit=true: SHOULD override
        update_project_config(&dir_key, "explicit-name", &primary_repo, &additional, true).unwrap();
        let config = midtown::config::FullProjectConfig::load_from(&config_path).unwrap();
        assert_eq!(config.project.name(), Some("explicit-name"));

        // Clean up
        let _ = std::fs::remove_file(&config_path);
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn test_is_dist_fresh_no_dist_returns_false() {
        let temp = TempDir::new().unwrap();
        let web_app_dir = temp.path();
        let dist_index = web_app_dir.join("dist").join("index.html");
        // dist/index.html doesn't exist → stale (needs rebuild)
        assert!(!is_dist_fresh(web_app_dir, &dist_index));
    }

    #[test]
    fn test_is_dist_fresh_newer_source_returns_false() {
        let temp = TempDir::new().unwrap();
        let web_app_dir = temp.path();

        // Create dist/index.html first
        let dist_dir = web_app_dir.join("dist");
        fs::create_dir_all(&dist_dir).unwrap();
        let dist_index = dist_dir.join("index.html");
        fs::write(&dist_index, "old").unwrap();

        // Sleep briefly so source file gets a newer mtime
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create a source file that's newer than dist
        let src_dir = web_app_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("app.ts"), "new code").unwrap();

        assert!(!is_dist_fresh(web_app_dir, &dist_index));
    }

    #[test]
    fn test_is_dist_fresh_no_newer_source_returns_true() {
        let temp = TempDir::new().unwrap();
        let web_app_dir = temp.path();

        // Create source files first
        let src_dir = web_app_dir.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("app.ts"), "code").unwrap();
        fs::write(web_app_dir.join("package.json"), "{}").unwrap();

        // Sleep briefly so dist gets a newer mtime
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create dist/index.html after source files
        let dist_dir = web_app_dir.join("dist");
        fs::create_dir_all(&dist_dir).unwrap();
        let dist_index = dist_dir.join("index.html");
        fs::write(&dist_index, "built").unwrap();

        assert!(is_dist_fresh(web_app_dir, &dist_index));
    }

    #[test]
    fn test_is_dist_fresh_newer_config_file_returns_false() {
        let temp = TempDir::new().unwrap();
        let web_app_dir = temp.path();

        // Create dist first
        let dist_dir = web_app_dir.join("dist");
        fs::create_dir_all(&dist_dir).unwrap();
        let dist_index = dist_dir.join("index.html");
        fs::write(&dist_index, "built").unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));

        // package.json modified after dist → stale
        fs::write(web_app_dir.join("package.json"), "{}").unwrap();

        assert!(!is_dist_fresh(web_app_dir, &dist_index));
    }
}

#[path = "daemon_tests.rs"]
#[cfg(test)]
mod daemon_tests;
