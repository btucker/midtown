//! Coworker management for the midtown daemon.
//!
//! Tracks active coworkers and their state, coordinating with tmux windows
//! within the project session.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tmux;
use crate::worktree::WorktreeManager;

/// Primary Manhattan avenue names used for coworker naming.
const AVENUE_NAMES: &[&str] = &[
    "lexington",
    "park",
    "madison",
    "broadway",
    "amsterdam",
    "columbus",
    "riverside",
    "york",
    "pleasant",
    "vernon",
];

/// Overflow street names for when primary avenues are exhausted.
const OVERFLOW_NAMES: &[&str] = &["bleecker", "houston", "canal", "spring", "prince", "mercer"];

/// Status of a coworker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoworkerStatus {
    /// Starting up
    Starting,
    /// Running and ready
    Running,
    /// Shutting down
    Stopping,
    /// Stopped/terminated
    Stopped,
}

impl std::fmt::Display for CoworkerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoworkerStatus::Starting => write!(f, "starting"),
            CoworkerStatus::Running => write!(f, "running"),
            CoworkerStatus::Stopping => write!(f, "stopping"),
            CoworkerStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// Information about a coworker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coworker {
    /// Unique name (avenue name)
    pub name: String,
    /// Current status
    pub status: CoworkerStatus,
    /// Working directory
    pub working_dir: String,
    /// When the coworker was started
    pub started_at: DateTime<Utc>,
    /// Current task being worked on (if any)
    pub current_task: Option<String>,
}

/// Manager for coworker lifecycle.
#[derive(Debug, Clone)]
pub struct CoworkerManager {
    /// Map of coworker name -> coworker info
    coworkers: Arc<RwLock<HashMap<String, Coworker>>>,
    /// Worktree manager for creating isolated workspaces
    worktree_manager: Arc<WorktreeManager>,
    /// The tmux session name for the project (e.g., "midtown-projectname")
    session_name: String,
}

impl CoworkerManager {
    /// Create a new coworker manager.
    ///
    /// # Arguments
    /// * `session_name` - The tmux session name (e.g., "midtown-projectname")
    /// * `worktree_manager` - Manager for creating isolated git worktrees
    pub fn new(session_name: impl Into<String>, worktree_manager: WorktreeManager) -> Self {
        Self {
            coworkers: Arc::new(RwLock::new(HashMap::new())),
            worktree_manager: Arc::new(worktree_manager),
            session_name: session_name.into(),
        }
    }

    /// Get the next available avenue name.
    ///
    /// Tries primary avenue names first, then falls back to overflow names.
    fn next_name(&self) -> Option<String> {
        let coworkers = self.coworkers.read().unwrap();

        // Try primary avenue names first
        for name in AVENUE_NAMES {
            if !coworkers.contains_key(*name) {
                return Some(name.to_string());
            }
        }

        // Fall back to overflow names
        for name in OVERFLOW_NAMES {
            if !coworkers.contains_key(*name) {
                return Some(name.to_string());
            }
        }

        None
    }

    /// Spawn a new coworker.
    ///
    /// Creates an isolated git worktree for the coworker and spawns Claude Code in it.
    /// Returns the name of the spawned coworker.
    pub fn spawn(&self) -> crate::Result<String> {
        let name = self.next_name().ok_or_else(|| crate::Error::Rpc {
            code: -32603,
            message: "No available coworker slots (all avenue names in use)".to_string(),
        })?;

        // Create an isolated worktree for this coworker
        let worktree_path = self
            .worktree_manager
            .create(&name)
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!("Failed to create worktree for {}: {}", name, e),
            })?;

        let working_dir = worktree_path
            .to_str()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "Worktree path is not valid UTF-8".to_string(),
            })?
            .to_string();

        // Create the tmux window and spawn claude in the worktree
        tmux::spawn_claude(&self.session_name, &name, &working_dir)?;

        // Record the coworker
        let coworker = Coworker {
            name: name.clone(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
        };

        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.insert(name.clone(), coworker);
        }

        Ok(name)
    }

    /// Shutdown a coworker by name.
    pub fn shutdown(&self, name: &str) -> crate::Result<()> {
        // Update status to stopping
        {
            let mut coworkers = self.coworkers.write().unwrap();
            if let Some(cw) = coworkers.get_mut(name) {
                cw.status = CoworkerStatus::Stopping;
            } else {
                return Err(crate::Error::Rpc {
                    code: -32602,
                    message: format!("Coworker not found: {}", name),
                });
            }
        }

        // Kill the tmux window
        tmux::kill_window(&self.session_name, name)?;

        // Remove from tracking
        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.remove(name);
        }

        Ok(())
    }

    /// Shutdown all coworkers.
    pub fn shutdown_all(&self) -> crate::Result<()> {
        let names: Vec<String> = {
            let coworkers = self.coworkers.read().unwrap();
            coworkers.keys().cloned().collect()
        };

        for name in names {
            // Ignore errors during shutdown_all - best effort
            let _ = self.shutdown(&name);
        }

        Ok(())
    }

    /// List all coworkers.
    pub fn list(&self) -> Vec<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.values().cloned().collect()
    }

    /// Get a coworker by name.
    pub fn get(&self, name: &str) -> Option<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.get(name).cloned()
    }

    /// Send a nudge (input) to a coworker.
    pub fn nudge(&self, name: &str, message: &str) -> crate::Result<()> {
        // Verify coworker exists
        {
            let coworkers = self.coworkers.read().unwrap();
            if !coworkers.contains_key(name) {
                return Err(crate::Error::Rpc {
                    code: -32602,
                    message: format!("Coworker not found: {}", name),
                });
            }
        }

        // Send keys to the tmux window
        tmux::send_keys(&self.session_name, name, message)
    }

    /// Sync state with actual tmux windows.
    ///
    /// Removes coworkers whose tmux windows no longer exist.
    pub fn sync_with_tmux(&self) -> crate::Result<()> {
        let active_windows = tmux::list_windows(&self.session_name)?;

        let mut coworkers = self.coworkers.write().unwrap();

        // Remove coworkers whose windows are gone
        coworkers.retain(|name, _| active_windows.contains(name));

        Ok(())
    }

    /// Get count of active coworkers.
    pub fn count(&self) -> usize {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Create a test CoworkerManager with a temporary git repo
    fn test_manager() -> (CoworkerManager, TempDir) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Initialize a git repo in the temp dir
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to init git repo");

        // Create an initial commit (required for worktrees)
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to create initial commit");

        let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("Failed to create worktree manager");
        let manager = CoworkerManager::new("midtown-test", worktree_manager);

        (manager, temp_dir)
    }

    #[test]
    fn test_coworker_status_display() {
        assert_eq!(CoworkerStatus::Running.to_string(), "running");
        assert_eq!(CoworkerStatus::Starting.to_string(), "starting");
        assert_eq!(CoworkerStatus::Stopping.to_string(), "stopping");
        assert_eq!(CoworkerStatus::Stopped.to_string(), "stopped");
    }

    #[test]
    fn test_avenue_names_exist() {
        assert!(!AVENUE_NAMES.is_empty());
        assert_eq!(AVENUE_NAMES.len(), 10);
        assert!(!OVERFLOW_NAMES.is_empty());
        assert_eq!(OVERFLOW_NAMES.len(), 6);
    }

    #[test]
    fn test_coworker_manager_new() {
        let (manager, _temp_dir) = test_manager();
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_next_name_empty() {
        let (manager, _temp_dir) = test_manager();
        assert_eq!(manager.next_name(), Some("lexington".to_string()));
    }

    #[test]
    fn test_next_name_with_used_names() {
        let (manager, _temp_dir) = test_manager();

        // Manually insert a coworker to simulate "lexington" being in use
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            coworkers.insert(
                "lexington".to_string(),
                Coworker {
                    name: "lexington".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                },
            );
        }

        // Should return "park" (second in list)
        assert_eq!(manager.next_name(), Some("park".to_string()));
    }

    #[test]
    fn test_next_name_overflow() {
        let (manager, _temp_dir) = test_manager();

        // Fill all primary avenue names
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            for name in AVENUE_NAMES {
                coworkers.insert(
                    name.to_string(),
                    Coworker {
                        name: name.to_string(),
                        status: CoworkerStatus::Running,
                        working_dir: "/tmp".to_string(),
                        started_at: Utc::now(),
                        current_task: None,
                    },
                );
            }
        }

        // Should return first overflow name
        assert_eq!(manager.next_name(), Some("bleecker".to_string()));
    }

    #[test]
    fn test_next_name_exhausted() {
        let (manager, _temp_dir) = test_manager();

        // Fill all names (primary and overflow)
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            for name in AVENUE_NAMES.iter().chain(OVERFLOW_NAMES.iter()) {
                coworkers.insert(
                    name.to_string(),
                    Coworker {
                        name: name.to_string(),
                        status: CoworkerStatus::Running,
                        working_dir: "/tmp".to_string(),
                        started_at: Utc::now(),
                        current_task: None,
                    },
                );
            }
        }

        // Should return None when all names exhausted
        assert_eq!(manager.next_name(), None);
    }
}
