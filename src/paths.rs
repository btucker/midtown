//! Path utilities for midtown daemon and clients.
//!
//! Provides consistent path handling for daemon sockets, channels, and other
//! resources, with proper support for git worktrees.
//!
//! ## Directory Structure
//!
//! The `~/.midtown/` directory is organized as follows:
//!
//! ```text
//! ~/.midtown/
//! ├── config.toml           # Global configuration
//! ├── agents/               # Custom agent prompts
//! ├── lead/                 # Lead session data by project
//! │   └── <repo>/
//! │       └── session-id    # Lead's Claude session ID
//! └── projects/             # Project-specific runtime data
//!     └── <repo>/
//!         ├── worktrees/    # Task-based worktrees (named by branch slug)
//!         │   ├── lead/     # Lead worktree
//!         │   └── <slug>/   # e.g., task-42-add-auth-endpoint
//!         ├── coworkers/    # Legacy coworker worktrees (named by coworker)
//!         │   └── <name>/   # Individual worktree
//!         ├── workflow.py             # Project-level default workflow script (local, optional)
//!         ├── channels/     # Per-channel directories
//!         │   └── <name>/   # e.g., "midtown", "features"
//!         │       ├── history/current.jsonl  # Active message log
//!         │       ├── history/YYYY-MM-DD.jsonl # Rotated daily archives
//!         │       ├── notes/                 # Channel lead knowledge files
//!         │       ├── cursors/               # Per-agent read cursors
//!         │       ├── workflow.py            # Channel-specific workflow script (local, optional)
//!         │       └── workflow-state.json    # Persistent workflow state between invocations
//!         ├── logs/         # Daemon logs
//!         ├── daemon.pid    # Daemon PID file
//!         └── assets/       # Coworker-generated screenshots and videos
//! ```
//!
//! ## Workflow Scripts
//!
//! Workflow scripts (`workflow.py`) are invoked by the daemon via `uv run` on
//! relevant events. Resolution follows a 4-level priority order:
//!
//! 1. `<project_root>/.midtown/channels/<channel>/workflow.py` — channel-specific, in repo
//! 2. `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` — channel-specific, local
//! 3. `<project_root>/.midtown/workflow.py` — project default, in repo
//! 4. `~/.midtown/projects/<repo>/workflow.py` — project default, local
//!
//! See [`workflow_script_for_channel`] and [`workflow_state_file`].

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    static TEST_MIDTOWN_BASE_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_midtown_base_dir_override() -> Option<PathBuf> {
    TEST_MIDTOWN_BASE_DIR.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
pub struct TestMidtownBaseDirGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestMidtownBaseDirGuard {
    fn drop(&mut self) {
        TEST_MIDTOWN_BASE_DIR.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
pub fn set_test_midtown_base_dir(path: PathBuf) -> TestMidtownBaseDirGuard {
    let previous = TEST_MIDTOWN_BASE_DIR.with(|slot| slot.replace(Some(path)));
    TestMidtownBaseDirGuard { previous }
}

/// Detect the current git repository name.
///
/// Uses `git rev-parse --git-common-dir` to handle worktrees correctly,
/// since worktrees share the main repo's .git directory.
///
/// Returns `None` if not in a git repository.
pub fn detect_repo_name() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    detect_repo_name_from_dir(&cwd)
}

/// Detect the repository name from a specific directory (CWD-independent).
pub fn detect_repo_name_from_dir(dir: &std::path::Path) -> Option<String> {
    // First try git-common-dir which works correctly for worktrees
    let common_dir = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        });

    if let Some(git_dir) = common_dir {
        let git_path = std::path::Path::new(&git_dir);
        // The git-common-dir is the .git folder - get its parent's name
        if let Some(parent) = git_path.parent() {
            // Handle relative ".git" by getting the actual toplevel
            if git_dir == ".git" {
                return std::process::Command::new("git")
                    .args(["rev-parse", "--show-toplevel"])
                    .current_dir(dir)
                    .output()
                    .ok()
                    .and_then(|output| {
                        if output.status.success() {
                            let path = String::from_utf8_lossy(&output.stdout);
                            std::path::Path::new(path.trim())
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                        } else {
                            None
                        }
                    });
            }
            // For worktrees, parent is the main repo directory
            return parent.file_name().map(|s| s.to_string_lossy().to_string());
        }
    }

    // Fallback: try show-toplevel for regular repos
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                std::path::Path::new(path.trim())
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
}

/// Detect the project name for the current repository.
///
/// Priority:
/// 1. `[project].name` from the per-project config.toml
/// 2. Git repository directory name (from `detect_repo_name()`)
///
/// This is the canonical way to get the project name for use in
/// session names, display labels, etc.
pub fn detect_project_name() -> Option<String> {
    let repo_name = detect_repo_name()?;

    // Check if the project config has an explicit name
    if let Some(name) = crate::config::load_full_project_config(&repo_name)
        .and_then(|c| c.project.name().map(|s| s.to_string()))
    {
        return Some(name);
    }

    Some(repo_name)
}

/// Get the state directory for midtown.
///
/// Uses `XDG_STATE_HOME` if set, otherwise `~/.local/state`.
fn state_dir() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        })
}

/// Get the base midtown directory.
///
/// Returns `~/.midtown/`.
pub fn midtown_base_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(override_path) = test_midtown_base_dir_override() {
        return override_path;
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
}

/// Get the projects directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/`.
///
/// This is where project-specific runtime data is stored:
/// - channels/ (per-channel directories with history, notes, cursors)
/// - logs/ (daemon logs)
/// - daemon.pid (daemon PID file)
///
/// Automatically migrates from old directory structure on first access.
pub fn projects_dir_for_repo(repo: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir().join("projects").join(repo)
}

/// Get the assets directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/assets/`.
///
/// This is where coworker-generated screenshots and videos are stored.
/// It lives outside `web-app/dist/` so it persists across rebuilds and
/// worktree recreations. Files here are served by the webserver at
/// `/api/projects/<repo>/assets/<path>`.
pub fn assets_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("assets")
}

/// Get the coworkers directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/coworkers/`.
///
/// This is the legacy location for coworker worktrees (named by coworker).
/// New worktrees should use `worktrees_dir_for_repo()` instead.
///
/// Automatically migrates from old directory structure on first access.
pub fn coworkers_dir_for_repo(repo: &str) -> PathBuf {
    migrate_worktree_paths(repo);
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join("coworkers")
}

/// Get the headless session output log file for a coworker.
///
/// Returns `~/.midtown/projects/<repo>/headless-<name>.jsonl`.
///
/// This file stores all StreamEvents from a headless coworker session,
/// enabling `midtown coworker view` to read recent output and providing
/// persistent debug logs.
pub fn headless_output_file(repo: &str, coworker_name: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join(format!("headless-{}.jsonl", coworker_name))
}

/// Get the task-based worktrees directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/worktrees/`.
///
/// This is where task-based worktrees are created. Each worktree is named
/// by its branch slug (e.g., `task-42-add-auth-endpoint/`), decoupled from
/// coworker identity to enable build cache reuse across reassignment.
///
/// Automatically migrates from the old `~/.midtown/worktrees/<repo>/` layout on first access.
pub fn worktrees_dir_for_repo(repo: &str) -> PathBuf {
    migrate_worktree_paths(repo);
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join("worktrees")
}

/// Get the lead worktree path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/worktrees/lead/`.
pub fn lead_worktree_path(repo: &str) -> PathBuf {
    worktrees_dir_for_repo(repo).join("lead")
}

/// Get the lead directory for a specific repository.
///
/// Returns `~/.midtown/lead/<repo>/`.
///
/// This is where Lead session data is stored.
///
/// Automatically migrates from old directory structure on first access.
pub fn lead_dir_for_repo(repo: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir().join("lead").join(repo)
}

/// Get the lead system prompt file path for a specific repository.
///
/// Returns `~/.midtown/lead/<repo>/system-prompt.txt`.
///
/// This file stores the lead's system prompt (lead.md + common.md)
/// so it can be re-applied when attaching to a headless lead session.
pub fn lead_system_prompt_file(repo: &str) -> PathBuf {
    lead_dir_for_repo(repo).join("system-prompt.txt")
}

/// Get the Lead session ID file path for a specific repository.
///
/// Returns `~/.midtown/lead/<repo>/session-id`.
pub fn lead_session_file_for_repo(repo: &str) -> PathBuf {
    lead_dir_for_repo(repo).join("session-id")
}

/// Get the Lead session ID file path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn lead_session_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    lead_session_file_for_repo(&repo)
}

/// Get the task list ID for a specific repository.
///
/// Returns `midtown-<repo>` which should be set as `CLAUDE_CODE_TASK_LIST_ID`
/// for all Claude sessions (Lead and coworkers) to share the same task storage.
pub fn task_list_id_for_repo(repo: &str) -> String {
    format!("midtown-{}", repo)
}

/// Get the task list ID for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn task_list_id() -> String {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    task_list_id_for_repo(&repo)
}

/// Get the active channel file path for the default channel of a repository.
///
/// Returns `~/.midtown/projects/<repo>/channels/<repo>/history/current.jsonl`.
/// The channel name matches the repo name so each project has its own default channel.
pub fn channel_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join(repo)
        .join("history")
        .join("current.jsonl")
}

/// Get the cursors directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/cursors/`.
pub fn cursors_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("cursors")
}

/// Get the GitHub state file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/github-state.json`.
///
/// This file stores persistent GitHub-related state:
/// - PR reviewer assignments (which coworker is reviewing which PR)
pub fn github_state_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("github-state.json")
}

/// Get the reminders file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/reminders.json`.
pub fn reminders_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("reminders.json")
}

/// Get the unified daemon state file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/daemon-state.json`.
///
/// This file stores all persistent daemon state:
/// - GitHub PR reviewer assignments, review cache, pending spawns
/// - One-shot reminders
pub fn daemon_state_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("daemon-state.json")
}

/// Get the daemon socket path for a specific repository.
///
/// Returns `~/.local/state/midtown/<repo>/daemon.sock`.
///
/// This ensures each project has its own daemon, preventing coworkers
/// from different projects from being mixed.
pub fn daemon_socket_for_repo(repo: &str) -> PathBuf {
    state_dir().join("midtown").join(repo).join("daemon.sock")
}

/// Enumerate all daemon sockets across all projects.
///
/// Scans `~/.local/state/midtown/*/daemon.sock` and returns `(repo_name, socket_path)`
/// pairs for each socket that exists on disk. The socket existing doesn't guarantee
/// the daemon is running — callers should handle connection failures gracefully.
pub fn enumerate_daemon_sockets() -> Vec<(String, PathBuf)> {
    enumerate_daemon_sockets_in(&state_dir().join("midtown"))
}

fn enumerate_daemon_sockets_in(midtown_state: &Path) -> Vec<(String, PathBuf)> {
    let entries = match fs::read_dir(midtown_state) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let repo_name = entry.file_name().to_string_lossy().to_string();
            let sock = path.join("daemon.sock");
            if sock.exists() {
                Some((repo_name, sock))
            } else {
                None
            }
        })
        .collect()
}

/// Get the daemon socket path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn daemon_socket() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_socket_for_repo(&repo)
}

/// Get the daemon PID file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/daemon.pid`.
///
/// The PID file is used to enforce singleton behavior - only one daemon
/// can run per repository. The file is locked exclusively while the
/// daemon is running.
pub fn daemon_pid_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("daemon.pid")
}

/// Get the daemon PID file path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn daemon_pid_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_pid_file_for_repo(&repo)
}

/// Get the daemon log directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/`.
/// This is where daemon stdout/stderr are redirected when daemonized.
pub fn daemon_log_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("logs")
}

/// Get the daemon log directory for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/`.
/// This is where daemon stdout/stderr are redirected when daemonized.
pub fn daemon_log_dir() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_log_dir_for_repo(&repo)
}

/// Get the daemon log file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/daemon.log`.
/// This is where tracing output is written (via `tracing_subscriber`).
pub fn daemon_log_file_for_repo(repo: &str) -> PathBuf {
    daemon_log_dir_for_repo(repo).join("daemon.log")
}

/// Get the daemon log file path for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/daemon.log`.
pub fn daemon_log_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_log_file_for_repo(&repo)
}

/// Get the hooks log file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/hooks.log`.
/// Hook handlers append timestamped lines here for debugging.
pub fn hooks_log_file_for_repo(repo: &str) -> PathBuf {
    daemon_log_dir_for_repo(repo).join("hooks.log")
}

/// Get the hooks log file path for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/hooks.log`.
pub fn hooks_log_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    hooks_log_file_for_repo(&repo)
}

/// Find a workflow script for a specific channel.
///
/// Implements a 4-level resolution order so scripts can be committed to the repo
/// (shared with the team) or kept local (machine-specific overrides):
///
/// 1. `<project_root>/.midtown/channels/<channel>/workflow.py` — channel-specific, in repo
/// 2. `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` — channel-specific, local
/// 3. `<project_root>/.midtown/workflow.py` — project default, in repo
/// 4. `~/.midtown/projects/<repo>/workflow.py` — project default, local
///
/// Returns `None` if no workflow script is found at any level, meaning the daemon
/// falls back to its compiled-in default behavior.
pub fn workflow_script_for_channel(
    channel: &str,
    project_root: &Path,
    repo: &str,
) -> Option<PathBuf> {
    // 1. Channel-specific, in repo
    let path = project_root
        .join(".midtown")
        .join("channels")
        .join(channel)
        .join("workflow.py");
    if path.exists() {
        return Some(path);
    }

    // 2. Channel-specific, local
    let path = projects_dir_for_repo(repo)
        .join("channels")
        .join(channel)
        .join("workflow.py");
    if path.exists() {
        return Some(path);
    }

    // 3. Project default, in repo
    let path = project_root.join(".midtown").join("workflow.py");
    if path.exists() {
        return Some(path);
    }

    // 4. Project default, local
    let path = projects_dir_for_repo(repo).join("workflow.py");
    if path.exists() {
        return Some(path);
    }

    None
}

/// Get the workflow state file path for a channel.
///
/// Returns `~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json`.
///
/// This file stores serialized workflow state between invocations. The
/// subprocess-per-event model requires external persistence: the daemon passes
/// this path to each `uv run` invocation so the script can load and save its
/// state machine state transparently.
pub fn workflow_state_file(channel: &str, repo: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join(channel)
        .join("workflow-state.json")
}

/// Rename `tmp` to `target`, cleaning up `tmp` if the rename fails.
///
/// This wraps `fs::rename` to ensure temp files are not leaked on disk when
/// the rename step of an atomic write fails (e.g., permission denied).
pub fn atomic_rename(tmp: &Path, target: &Path) -> std::io::Result<()> {
    if let Err(e) = fs::rename(tmp, target) {
        if let Err(cleanup_err) = fs::remove_file(tmp) {
            tracing::warn!(
                "Failed to clean up temp file {} after failed rename: {}",
                tmp.display(),
                cleanup_err
            );
        }
        return Err(e);
    }
    Ok(())
}

/// Migrate data from the old directory structure to the new one.
///
/// Old structure: `~/.midtown/<repo>/...`
/// New structure: `~/.midtown/{projects,coworkers,lead}/<repo>/...`
///
/// Migrates:
/// - `channel.jsonl` -> `projects/<repo>/channel.jsonl`
/// - `cursors/` -> `projects/<repo>/cursors/`
/// - `logs/` -> `projects/<repo>/logs/`
/// - `daemon.pid` -> `projects/<repo>/daemon.pid`
/// - `worktrees/` -> `coworkers/<repo>/`
/// - `lead-session-id` -> `lead/<repo>/session-id`
/// - `lead-initialized` -> `lead/<repo>/lead-initialized`
///
/// Returns Ok(true) if migration was performed, Ok(false) if already migrated or nothing to migrate.
pub fn migrate_directory_structure(repo: &str) -> std::io::Result<bool> {
    let base = midtown_base_dir();
    let old_repo_dir = base.join(repo);

    // Check if old structure exists
    if !old_repo_dir.exists() {
        return Ok(false);
    }

    // Check if already migrated (new structure exists)
    let new_projects_dir = projects_dir_for_repo(repo);
    if new_projects_dir.exists() && new_projects_dir.join("channel.jsonl").exists() {
        return Ok(false);
    }

    // Create new directories
    std::fs::create_dir_all(&new_projects_dir)?;
    std::fs::create_dir_all(coworkers_dir_for_repo(repo))?;
    std::fs::create_dir_all(lead_dir_for_repo(repo))?;

    // Migrate project files
    let project_files = [
        "channel.jsonl",
        "daemon.pid",
        "cursors",
        "logs",
        "flagged_prs",
        "insights",
    ];
    for file in &project_files {
        let old_path = old_repo_dir.join(file);
        let new_path = new_projects_dir.join(file);
        if old_path.exists() && !new_path.exists() {
            std::fs::rename(&old_path, &new_path)?;
        }
    }

    // Migrate worktrees directory
    let old_worktrees = old_repo_dir.join("worktrees");
    let new_coworkers = coworkers_dir_for_repo(repo);
    if old_worktrees.exists() {
        // Move each worktree directory
        if let Ok(entries) = std::fs::read_dir(&old_worktrees) {
            for entry in entries.flatten() {
                let old_path = entry.path();
                if old_path.is_dir()
                    && let Some(name) = old_path.file_name()
                {
                    let new_path = new_coworkers.join(name);
                    if !new_path.exists() {
                        std::fs::rename(&old_path, &new_path)?;
                    }
                }
            }
        }
        // Remove empty old worktrees directory
        let _ = std::fs::remove_dir(&old_worktrees);
    }

    // Migrate lead session files
    let lead_dir = lead_dir_for_repo(repo);
    let old_session_file = old_repo_dir.join("lead-session-id");
    let new_session_file = lead_dir.join("session-id");
    if old_session_file.exists() && !new_session_file.exists() {
        std::fs::rename(&old_session_file, &new_session_file)?;
    }

    let old_initialized = old_repo_dir.join("lead-initialized");
    let new_initialized = lead_dir.join("lead-initialized");
    if old_initialized.exists() && !new_initialized.exists() {
        std::fs::rename(&old_initialized, &new_initialized)?;
    }

    // Try to remove old repo directory if empty
    let _ = std::fs::remove_dir(&old_repo_dir);

    Ok(true)
}

/// Migrate worktree paths from old layout to new project-grouped layout.
///
/// Old layout:
/// - `~/.midtown/worktrees/<repo>/` → `~/.midtown/projects/<repo>/worktrees/`
/// - `~/.midtown/coworkers/<repo>/` → `~/.midtown/projects/<repo>/coworkers/`
///
/// This is called internally by `worktrees_dir_for_repo()` and
/// `coworkers_dir_for_repo()` to ensure seamless migration on first access.
/// It's idempotent and only runs once per session per repo.
pub fn migrate_worktree_paths(repo: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static MIGRATED_WT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED_WT.get_or_init(|| Mutex::new(HashSet::new()));

    let mut guard = migrated.lock().unwrap();
    if guard.contains(repo) {
        return;
    }
    guard.insert(repo.to_string());
    drop(guard);

    let _ = do_migrate_worktree_paths(repo);
}

/// Perform the actual migration of worktree paths.
///
/// Returns `Ok(true)` if any migration was performed, `Ok(false)` if nothing to migrate.
pub fn do_migrate_worktree_paths(repo: &str) -> std::io::Result<bool> {
    let base = midtown_base_dir();
    let projects_dir = base.join("projects").join(repo);
    let mut migrated_any = false;

    // Migrate ~/.midtown/worktrees/<repo>/ → ~/.midtown/projects/<repo>/worktrees/
    let old_worktrees = base.join("worktrees").join(repo);
    let new_worktrees = projects_dir.join("worktrees");
    if old_worktrees.exists() && !new_worktrees.exists() {
        fs::create_dir_all(&projects_dir)?;
        fs::rename(&old_worktrees, &new_worktrees)?;
        tracing::info!(
            "Migrated worktrees: {} -> {}",
            old_worktrees.display(),
            new_worktrees.display()
        );
        migrated_any = true;

        // Clean up empty parent ~/.midtown/worktrees/ if it's now empty
        let old_worktrees_parent = base.join("worktrees");
        let _ = fs::remove_dir(&old_worktrees_parent);
    }

    // Migrate ~/.midtown/coworkers/<repo>/ → ~/.midtown/projects/<repo>/coworkers/
    let old_coworkers = base.join("coworkers").join(repo);
    let new_coworkers = projects_dir.join("coworkers");
    if old_coworkers.exists() && !new_coworkers.exists() {
        fs::create_dir_all(&projects_dir)?;
        fs::rename(&old_coworkers, &new_coworkers)?;
        tracing::info!(
            "Migrated coworkers: {} -> {}",
            old_coworkers.display(),
            new_coworkers.display()
        );
        migrated_any = true;

        // Clean up empty parent ~/.midtown/coworkers/ if it's now empty
        let old_coworkers_parent = base.join("coworkers");
        let _ = fs::remove_dir(&old_coworkers_parent);
    }

    Ok(migrated_any)
}

/// Auto-migrate on first access.
///
/// This is called internally when accessing paths to ensure seamless migration.
/// It's idempotent and only runs once per session per repo.
pub fn auto_migrate(repo: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static MIGRATED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED.get_or_init(|| Mutex::new(HashSet::new()));

    let mut guard = migrated.lock().unwrap();
    if guard.contains(repo) {
        return;
    }
    guard.insert(repo.to_string());
    drop(guard);

    // Attempt migration silently
    let _ = migrate_directory_structure(repo);
}

#[path = "paths_tests.rs"]
#[cfg(test)]
mod tests;
