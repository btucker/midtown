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
//! ├── coworkers/            # Coworker worktrees by project
//! │   └── <repo>/
//! │       └── <coworker>/   # Individual worktree
//! ├── lead/                 # Lead session data by project
//! │   └── <repo>/
//! │       └── session-id    # Lead's Claude session ID
//! └── projects/             # Project-specific runtime data
//!     └── <repo>/
//!         ├── channel.jsonl # IRC-style message log
//!         ├── cursors/      # Per-agent read cursors
//!         ├── logs/         # Daemon logs
//!         └── daemon.pid    # Daemon PID file
//! ```

use std::path::PathBuf;

/// Detect the current git repository name.
///
/// Uses `git rev-parse --git-common-dir` to handle worktrees correctly,
/// since worktrees share the main repo's .git directory.
///
/// Returns `None` if not in a git repository.
pub fn detect_repo_name() -> Option<String> {
    // First try git-common-dir which works correctly for worktrees
    let common_dir = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
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
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
}

/// Get the projects directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/`.
///
/// This is where project-specific runtime data is stored:
/// - channel.jsonl (message log)
/// - cursors/ (per-agent read positions)
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
/// This is where coworker worktrees are created.
///
/// Automatically migrates from old directory structure on first access.
pub fn coworkers_dir_for_repo(repo: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir().join("coworkers").join(repo)
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

/// Get the channel file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/channel.jsonl`.
pub fn channel_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("channel.jsonl")
}

/// Get the cursors directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/cursors/`.
pub fn cursors_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("cursors")
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
    fn test_channel_file_for_repo() {
        let path = channel_file_for_repo("myproject");
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().contains("projects"));
        assert!(path.to_string_lossy().contains("myproject"));
        assert!(path.to_string_lossy().ends_with("channel.jsonl"));
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
    fn test_migrate_returns_false_when_nothing_to_migrate() {
        // Non-existent repo should return false
        let result = migrate_directory_structure("nonexistent-test-repo-xyz123");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
