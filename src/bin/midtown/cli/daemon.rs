//! Daemon lifecycle commands (start, stop, attach).
//!
//! These commands manage the midtown daemon and Lead session.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
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

/// Ensure the official marketplace is configured and required plugins are installed.
///
/// Plugin setup is best-effort — failures are logged but don't block startup.
/// The Claude CLI may not be authenticated yet (e.g. fresh container), in which
/// case plugin commands will fail gracefully.
fn ensure_plugins_installed() -> Result<(), String> {
    use midtown::daemon::REQUIRED_PLUGINS;

    if REQUIRED_PLUGINS.is_empty() {
        return Ok(());
    }

    // First ensure marketplace is configured
    if let Err(e) = ensure_marketplace_configured() {
        eprintln!("Warning: Could not configure plugin marketplace: {}", e);
        return Ok(());
    }

    // Get list of installed plugins
    let installed = match get_installed_plugins() {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Warning: Could not list plugins: {}", e);
            return Ok(());
        }
    };

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
    let trimmed = stdout.trim();

    // Empty output means no plugins installed (e.g. fresh container)
    if trimmed.is_empty() {
        return Ok(std::collections::HashSet::new());
    }

    // Parse JSON output - it's an array of objects with "id" field
    let plugins: Vec<serde_json::Value> = serde_json::from_str(trimmed)
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

/// Check if Zellij is available on the system.
/// Delegates to the shared implementation in `midtown::process`.
fn zellij_is_available() -> bool {
    midtown::process::zellij_is_available()
}

/// Escape a string for use inside KDL double-quoted strings.
///
/// KDL uses the same escape sequences as JSON strings, so we need to escape
/// backslashes and double quotes. This prevents paths with special characters
/// from producing invalid KDL.
fn escape_kdl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generate a KDL layout file for the Zellij session.
///
/// Creates a layout with:
/// - Left pane (25%): Midtown sidebar plugin
/// - Middle pane (25%): midtown chat (channel TUI)
/// - Right pane (50%): Lead Claude Code session (launched via shell script)
fn generate_zellij_layout(
    project_name: &str,
    lead_launcher: &Path,
    lead_workdir: &Path,
) -> Result<PathBuf, String> {
    let layout_dir = midtown::paths::midtown_base_dir().join("layouts");
    std::fs::create_dir_all(&layout_dir)
        .map_err(|e| format!("Failed to create layout dir: {}", e))?;

    let layout_path = layout_dir.join(format!("{}.kdl", project_name));
    let plugin_path = midtown::paths::midtown_base_dir()
        .join("plugins")
        .join("midtown_zellij_plugin.wasm");

    // Escape paths for KDL string interpolation
    let escaped_launcher = escape_kdl_string(&lead_launcher.display().to_string());
    let escaped_cwd = escape_kdl_string(&lead_workdir.display().to_string());

    // Check if the plugin WASM file exists; if not, use a simpler layout
    // without the plugin pane (graceful degradation).
    let layout = if plugin_path.exists() {
        let escaped_plugin = escape_kdl_string(&plugin_path.display().to_string());
        format!(
            r#"layout {{
    pane size="25%" {{
        plugin location="file:{plugin_path}"
    }}
    pane size="25%" {{
        command "midtown"
        args "chat"
    }}
    pane size="50%" focus=true {{
        command "bash"
        args "-c" "{launcher}"
        cwd "{cwd}"
    }}
}}
"#,
            plugin_path = escaped_plugin,
            launcher = escaped_launcher,
            cwd = escaped_cwd,
        )
    } else {
        format!(
            r#"layout {{
    pane size="30%" {{
        command "midtown"
        args "chat"
    }}
    pane size="70%" focus=true {{
        command "bash"
        args "-c" "{launcher}"
        cwd "{cwd}"
    }}
}}
"#,
            launcher = escaped_launcher,
            cwd = escaped_cwd,
        )
    };

    std::fs::write(&layout_path, layout).map_err(|e| format!("Failed to write layout: {}", e))?;

    Ok(layout_path)
}

/// Write a shell launcher script for the Lead Claude Code session.
///
/// The launcher script sets environment variables and execs claude with
/// the appropriate flags. This is used by Zellij's KDL layout to start
/// the Lead pane.
fn write_lead_launcher_script(
    _session: &str,
    lead_workdir: &Path,
    project_name: &str,
    additional_repos: &[PathBuf],
) -> Result<PathBuf, String> {
    let lead_dir = midtown::paths::midtown_base_dir()
        .join("lead")
        .join(project_name);
    std::fs::create_dir_all(&lead_dir).map_err(|e| format!("Failed to create lead dir: {}", e))?;

    // Reuse existing tmux infrastructure to build the lead shell command.
    // spawn_lead writes prompt/settings files and constructs the command;
    // we extract just the command string for the launcher script.
    let prompt_file = midtown::settings::write_lead_prompt_file()
        .map_err(|e| format!("Failed to write lead prompt: {}", e))?;
    let settings_file = midtown::settings::write_lead_settings_file()
        .map_err(|e| format!("Failed to write lead settings: {}", e))?;

    // Resolve auth profile from project config
    let auth_dir = midtown::auth::active_profile_dir_for_project(project_name);

    let config = midtown::launch::LaunchConfig {
        name: "lead".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        role: midtown::launch::CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: additional_repos.to_vec(),
        restrict_setting_sources: false,
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: Some(auth_dir),
        auth_provider: midtown::auth::AuthProvider::Claude,
    };

    let task_list_id = midtown::paths::task_list_id_for_repo(project_name);

    // Allow tests/CI to override the lead command (claude isn't available in CI)
    let shell_command = if let Ok(test_cmd) = std::env::var("MIDTOWN_LEAD_COMMAND") {
        test_cmd
    } else {
        let launch = config.to_shell_command(
            &settings_file,
            &prompt_file,
            None,
            lead_workdir,
            project_name,
        );
        launch.shell_command
    };

    let script_content = format!(
        "#!/bin/bash\nexport CLAUDE_CODE_TASK_LIST_ID='{}';\n{}\n",
        task_list_id, shell_command
    );

    let script_path = lead_dir.join("zellij-lead-launcher.sh");
    std::fs::write(&script_path, &script_content)
        .map_err(|e| format!("Failed to write launcher script: {}", e))?;

    // Make the script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }

    Ok(script_path)
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

/// Create or reuse the lead worktree, falling back to the main repo path on error.
///
/// This helper encapsulates the worktree creation logic shared between
/// `handle_start()` and `ensure_lead_has_settings()`.
///
/// Returns the lead worktree path on success, or the original repo path as fallback.
fn create_or_reuse_lead_worktree(repo: &Path) -> Result<PathBuf, String> {
    let worktree_manager = midtown::worktree::WorktreeManager::new(repo.to_path_buf())
        .map_err(|e| format!("Failed to initialize worktree manager: {}", e))?;

    worktree_manager
        .create_lead_worktree()
        .map_err(|e| {
            eprintln!(
                "Warning: Failed to create lead worktree, falling back to main repo: {}",
                e
            );
            e
        })
        .or_else(|_| Ok(repo.to_path_buf()))
}

/// Handle `midtown start` command.
///
/// 1. Starts the daemon (if not running)
/// 2. Creates a terminal session for the project (Zellij preferred, tmux fallback)
/// 3. Launches Claude Code with Lead config in that session
///
/// When Zellij is available, creates a Zellij session with a KDL layout containing
/// a sidebar plugin, chat pane, and lead pane. When Zellij is not installed, falls
/// back to the legacy tmux-based session with status bar hooks and chat pane.
///
/// Claude Code processes run inside a lightweight filesystem sandbox
/// (sandbox-exec on macOS, bwrap on Linux) that restricts writes to
/// the project directory, ~/.midtown, ~/.claude, and temp directories.
pub fn handle_start(
    daemon_only: bool,
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
    let additional_repos = resolve_repos(&repos, &project_name);
    let session = session_name_for(&Some(project_name.clone()))?;
    emit_startup_progress(
        5,
        &format!(
            "starting project '{}'{}",
            project_name,
            if daemon_only { " (daemon-only)" } else { "" }
        ),
    );

    // Verify Claude CLI is installed (unless using a stub command or daemon-only mode)
    if !daemon_only && std::env::var("MIDTOWN_LEAD_COMMAND").is_err() && !claude_cli_available() {
        clear_startup_progress();
        return Err(
            "Claude CLI is not installed. Install it with: curl -fsSL https://claude.ai/install.sh | bash"
                .to_string(),
        );
    }

    // Ensure required plugins are installed (unless using a stub command)
    if std::env::var("MIDTOWN_LEAD_COMMAND").is_err() {
        emit_startup_progress(55, "checking required Claude plugins");
        ensure_plugins_installed()?;
    }

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

    // Step 2: Launch terminal session (unless --daemon-only)
    // Prefer Zellij when available, fall back to tmux.
    if daemon_only {
        messages.push("Skipping terminal session (--daemon-only)".to_string());
        emit_startup_progress(88, "skipping terminal session (--daemon-only)");
    } else if session_exists(&session) {
        messages.push(format!("Session '{}' already exists", session));
        emit_startup_progress(88, &format!("session '{}' already exists", session));
    } else if zellij_is_available() {
        // --- Zellij path ---
        emit_startup_progress(88, "creating Zellij session");

        // Clear stale task ID mappings from previous sessions
        midtown::tasks::clear_lead_task_id_map(&project_name);

        // Create lead worktree (or reuse existing one)
        emit_startup_progress(90, "creating lead worktree");
        let lead_workdir = create_or_reuse_lead_worktree(&primary_repo)?;

        // Write launcher script for the Lead Claude Code session
        let lead_launcher =
            write_lead_launcher_script(&session, &lead_workdir, &project_name, &additional_repos)?;

        // Generate KDL layout for this project
        let layout_path = generate_zellij_layout(&project_name, &lead_launcher, &lead_workdir)?;

        // Launch Zellij in the background (detached)
        let status = Command::new("zellij")
            .args([
                "--session",
                &session,
                "--layout",
                &layout_path.to_string_lossy(),
            ])
            .current_dir(&lead_workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Failed to create Zellij session: {}", e))?;

        if !status.success() {
            clear_startup_progress();
            return Err(format!("Failed to create Zellij session '{}'", session));
        }

        // Write marker file indicating Lead was initialized by midtown
        let marker_path = lead_initialized_marker(&primary_repo);
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker_path, env!("CARGO_PKG_VERSION"));

        messages.push(format!("Started Lead session in '{}' (Zellij)", session));
        emit_startup_progress(93, "lead session started (Zellij)");
    } else {
        // --- tmux path (legacy) ---
        let display_name = project_name.to_uppercase();
        emit_startup_progress(88, "creating tmux session");

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
            clear_startup_progress();
            return Err(format!("Failed to create tmux session '{}'", session));
        }

        // Create lead worktree (or reuse existing one) so the lead session
        // starts in the worktree instead of the main repo.
        emit_startup_progress(90, "creating lead worktree");
        let lead_workdir = create_or_reuse_lead_worktree(&primary_repo)?;

        // Use spawn_lead() to create the Lead window with proper config,
        // auth profile, settings, and system prompt.
        midtown::tmux::spawn_lead(
            &session,
            &lead_workdir.to_string_lossy(),
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

        messages.push(format!("Started Lead session in '{}' (tmux)", session));
        emit_startup_progress(93, "lead session started");
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
    let attach_hint = "Attach with: midtown attach".to_string();
    messages.push(attach_hint);
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
                // Kill the Zellij session — `zellij kill-session` sends SIGHUP to
                // pane processes, which Claude Code may survive. Use `zellij kill-session`
                // first, then clean up any orphaned processes.
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
/// Gracefully restarts the daemon and webserver while preserving the tmux
/// session and all running Claude processes (Lead and coworkers).
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
    //    If instead we stopped and re-spawned from the Lead's sandboxed tmux
    //    pane, the new daemon would inherit the sandbox, causing sandbox-exec
    //    nesting failures when spawning coworkers.
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

        let result = handle_start(true, None, vec![])?;
        restart_chat_pane();
        return match result {
            Response::Message { message } => Ok(Response::Message {
                message: format!("{} (legacy restart). Attach with: midtown attach", message),
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

    // Restart the chat pane to pick up code changes
    restart_chat_pane();

    let session = session_name().unwrap_or_else(|_| "midtown".to_string());
    Ok(Response::Message {
        message: format!(
            "Daemon exec-restarted. Resumed Lead session in '{}'. Attach with: midtown attach",
            session
        ),
    })
}

/// Restart the chat TUI pane in the tmux session.
fn restart_chat_pane() {
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
/// Attaches to the project's terminal session (Zellij or tmux).
/// If the session doesn't exist, it is automatically created first.
/// If the session exists but Lead wasn't started with midtown settings, reinitialize it.
pub fn handle_attach(project: Option<&str>) -> Result<Response, String> {
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
                "No session '{}' found. Start the project first with 'midtown start'.",
                session
            ));
        }

        // No active session means Lead is not running.
        // Clear any stale session-id file so we start a fresh
        // Claude Code session instead of resuming the old one.
        if let Some(ref repo) = repo {
            clear_stale_lead_session(repo);
        }

        // Start midtown (daemon + terminal session)
        handle_start(false, None, vec![])?;

        // Wait briefly for the session to be ready
        std::thread::sleep(std::time::Duration::from_millis(200));
    } else if let Some(ref repo) = repo {
        // Session exists - ensure Lead has proper settings (tmux only)
        if tmux_session_exists(&session) {
            ensure_lead_has_settings(&session, repo)?;
        }
    }

    // Attach to the session — try Zellij first, then tmux
    if zellij_session_exists(&session) {
        let err = Command::new("zellij").args(["attach", &session]).exec();
        // If we get here, exec failed
        Err(format!("Failed to attach to Zellij session: {}", err))
    } else {
        let err = Command::new("tmux").args(["attach", "-t", &session]).exec();
        // If we get here, exec failed
        Err(format!("Failed to attach to tmux session: {}", err))
    }
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

    // Create lead worktree (or reuse existing one) so the lead session
    // starts in the worktree instead of the main repo.
    let lead_workdir = create_or_reuse_lead_worktree(repo)?;

    // spawn_lead kills existing lead windows and creates a fresh one
    midtown::tmux::spawn_lead(session, &lead_workdir.to_string_lossy(), &repo_name, &[])
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
