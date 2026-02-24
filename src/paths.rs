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
//! ├── coworkers/            # Legacy coworker worktrees (named by coworker)
//! │   └── <repo>/
//! │       └── <coworker>/   # Individual worktree
//! ├── worktrees/            # Task-based worktrees (named by branch slug)
//! │   └── <repo>/
//! │       └── <branch-slug>/# Individual worktree
//! ├── lead/                 # Lead session data by project
//! │   └── <repo>/
//! │       └── session-id    # Lead's Claude session ID
//! └── projects/             # Project-specific runtime data
//!     └── <repo>/
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
//!         └── daemon.pid    # Daemon PID file
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

/// Get the coworkers directory for a specific repository.
///
/// Returns `~/.midtown/coworkers/<repo>/`.
///
/// This is the legacy location for coworker worktrees (named by coworker).
/// New worktrees should use `worktrees_dir_for_repo()` instead.
///
/// Automatically migrates from old directory structure on first access.
pub fn coworkers_dir_for_repo(repo: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir().join("coworkers").join(repo)
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
/// Returns `~/.midtown/worktrees/<repo>/`.
///
/// This is where task-based worktrees are created. Each worktree is named
/// by its branch slug (e.g., `task-42-add-auth-endpoint/`), decoupled from
/// coworker identity to enable build cache reuse across reassignment.
pub fn worktrees_dir_for_repo(repo: &str) -> PathBuf {
    midtown_base_dir().join("worktrees").join(repo)
}

/// Get the lead worktree path for a specific repository.
///
/// Returns `~/.midtown/worktrees/<repo>/lead/`.
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

/// Get the active channel file path for the default "midtown" channel.
///
/// Returns `~/.midtown/projects/<repo>/channels/midtown/history/current.jsonl`.
pub fn channel_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join("midtown")
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

#[cfg(test)]
mod tests {
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
}
