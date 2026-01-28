//! Coworker management for the midtown daemon.
//!
//! Tracks active coworkers and their state, coordinating with tmux windows
//! within the project session.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tmux;
use crate::worktree::{WorktreeError, WorktreeManager};

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
    /// Claude Code session ID (UUID) for task symlink management
    pub session_id: Option<String>,
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
        let session = session_name.into();
        let manager = Self {
            coworkers: Arc::new(RwLock::new(HashMap::new())),
            worktree_manager: Arc::new(worktree_manager),
            session_name: session,
        };

        // Discover existing coworkers from tmux on startup
        if let Err(e) = manager.discover_existing() {
            tracing::warn!("Failed to discover existing coworkers: {}", e);
        }

        manager
    }

    /// Returns the tmux session name (e.g., "midtown-projectname").
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Discover existing coworker windows from tmux and add them to tracking.
    ///
    /// This is called on startup to recover coworkers that were running before
    /// the daemon was restarted. The coworkers continue running uninterrupted.
    fn discover_existing(&self) -> crate::Result<()> {
        // Get all known coworker names
        let all_names: std::collections::HashSet<&str> = AVENUE_NAMES
            .iter()
            .chain(OVERFLOW_NAMES.iter())
            .copied()
            .collect();

        // List windows in our session
        let windows = match tmux::list_windows(&self.session_name) {
            Ok(w) => w,
            Err(_) => {
                // Session might not exist yet, that's OK
                return Ok(());
            }
        };

        let mut discovered = 0;
        let mut coworkers = self.coworkers.write().unwrap();

        for window_name in windows {
            // Check if this window name is a known coworker name
            if !all_names.contains(window_name.as_str()) {
                continue;
            }

            // Skip if already tracked
            if coworkers.contains_key(&window_name) {
                continue;
            }

            // Get the working directory from the worktree
            let working_dir = self
                .worktree_manager
                .worktree_path(&window_name)
                .to_string_lossy()
                .to_string();

            // Create a coworker entry
            let coworker = Coworker {
                name: window_name.clone(),
                status: CoworkerStatus::Running,
                working_dir,
                started_at: Utc::now(), // Unknown, use now as approximation
                current_task: None,     // Will be discovered via task tracking
                session_id: None,       // Will be set when coworker registers
            };

            coworkers.insert(window_name.clone(), coworker);
            discovered += 1;
            tracing::info!("Discovered existing coworker: {}", window_name);
        }

        if discovered > 0 {
            tracing::info!(
                "Discovered {} existing coworker(s) from tmux session",
                discovered
            );
        }

        Ok(())
    }

    /// Get a randomly selected available avenue name.
    ///
    /// Randomly selects from available primary avenue names first.
    /// Falls back to overflow names only when all primary names are in use.
    ///
    /// This can be used to get a name before spawning, e.g., to assign
    /// task ownership atomically before the coworker starts.
    pub fn next_available_name(&self) -> Option<String> {
        let coworkers = self.coworkers.read().unwrap();

        // Collect available primary avenue names
        let available: Vec<&str> = AVENUE_NAMES
            .iter()
            .filter(|name| !coworkers.contains_key(**name))
            .copied()
            .collect();

        if !available.is_empty() {
            // Randomly select from available primary names
            let idx = fastrand::usize(..available.len());
            return Some(available[idx].to_string());
        }

        // Fall back to overflow names (also randomized)
        let overflow_available: Vec<&str> = OVERFLOW_NAMES
            .iter()
            .filter(|name| !coworkers.contains_key(**name))
            .copied()
            .collect();

        if !overflow_available.is_empty() {
            let idx = fastrand::usize(..overflow_available.len());
            return Some(overflow_available[idx].to_string());
        }

        None
    }

    /// Spawn a new coworker.
    ///
    /// Creates an isolated git worktree for the coworker and spawns Claude Code in it.
    /// Returns the name of the spawned coworker.
    ///
    /// If a worktree already exists but no tmux window is running (stale worktree),
    /// the worktree is automatically cleaned up and the spawn is retried.
    ///
    /// If `resume` is true, passes `--continue` to claude to resume the previous
    /// session from this worktree (useful for recovering orphaned tasks).
    ///
    /// If `prompt` is provided, waits for the coworker to initialize and sends the
    /// prompt as the initial nudge. This is the preferred way to send initial
    /// instructions as it avoids the race condition of spawning then nudging separately.
    pub fn spawn(&self, resume: bool, prompt: Option<&str>) -> crate::Result<String> {
        let name = self
            .next_available_name()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "No available coworker slots (all avenue names in use)".to_string(),
            })?;

        // Try to create an isolated worktree for this coworker
        let worktree_path = match self.worktree_manager.create(&name) {
            Ok(path) => path,
            Err(WorktreeError::AlreadyExists(_)) => {
                // Worktree exists - check if there's a corresponding tmux window
                let window_exists = tmux::window_exists(&self.session_name, &name).unwrap_or(false);

                if window_exists {
                    // There's an active window, so the coworker is actually running
                    return Err(crate::Error::Rpc {
                        code: -32603,
                        message: format!(
                            "Coworker {} is already running (worktree and window exist)",
                            name
                        ),
                    });
                }

                // Stale worktree - clean it up and retry
                tracing::info!("Cleaning up stale worktree for {}", name);
                self.worktree_manager
                    .force_cleanup(&name)
                    .map_err(|e| crate::Error::Rpc {
                        code: -32603,
                        message: format!("Failed to cleanup stale worktree for {}: {}", name, e),
                    })?;

                // Retry creating the worktree
                self.worktree_manager
                    .create(&name)
                    .map_err(|e| crate::Error::Rpc {
                        code: -32603,
                        message: format!(
                            "Failed to create worktree for {} after cleanup: {}",
                            name, e
                        ),
                    })?
            }
            Err(e) => {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Failed to create worktree for {}: {}", name, e),
                });
            }
        };

        let working_dir = worktree_path
            .to_str()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "Worktree path is not valid UTF-8".to_string(),
            })?
            .to_string();

        // Create the tmux window and spawn claude in the worktree
        // Pass repo_name so the coworker's tasks can be symlinked to the Lead's tasks
        let repo_name = self.worktree_manager.repo_name();
        let session_id = tmux::spawn_claude(
            &self.session_name,
            &name,
            &working_dir,
            Some(repo_name),
            resume,
        )?;

        // Record the coworker with their session ID for symlink management
        let coworker = Coworker {
            name: name.clone(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
            session_id: Some(session_id),
        };

        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.insert(name.clone(), coworker);
        }

        // If a prompt was provided, wait for initialization and send it
        if let Some(prompt_text) = prompt {
            self.send_initial_prompt(&name, prompt_text);
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

    /// Update a coworker's status display in their tmux tab.
    ///
    /// This is called when a coworker posts a /me action to the channel,
    /// updating their tmux window name to show their current activity.
    ///
    /// # Arguments
    /// * `name` - The coworker name
    /// * `status` - The status text (or None to clear/show idle)
    pub fn update_status_display(&self, name: &str, status: Option<&str>) -> crate::Result<()> {
        // Only update if this is a known coworker
        {
            let coworkers = self.coworkers.read().unwrap();
            if !coworkers.contains_key(name) {
                // Not a coworker we're managing - might be Lead or external
                return Ok(());
            }
        }

        tmux::rename_window(&self.session_name, name, status)
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

    /// Wait for a coworker to initialize and send an initial prompt.
    ///
    /// This is a helper method used by `spawn()` and `spawn_with_name()` to avoid
    /// code duplication. It waits 2 seconds for the coworker to initialize, then
    /// sends the prompt as a nudge.
    fn send_initial_prompt(&self, name: &str, prompt: &str) {
        // Wait for coworker to initialize before sending prompt
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Send the initial prompt as a nudge
        if let Err(e) = self.nudge(name, prompt) {
            tracing::warn!("Failed to send initial prompt to {}: {}", name, e);
        } else {
            tracing::info!("Sent initial prompt to {}", name);
        }
    }

    /// Send a nudge (input) to the Lead session.
    ///
    /// This is used to notify the Lead about coworker feedback requests.
    pub fn nudge_lead(&self, message: &str) -> crate::Result<()> {
        // Check if lead window exists
        if !tmux::window_exists(&self.session_name, "lead")? {
            return Err(crate::Error::Rpc {
                code: -32602,
                message: "Lead session not found".to_string(),
            });
        }

        // Send keys to the lead window
        tmux::send_keys(&self.session_name, "lead", message)
    }

    /// Spawn a coworker with a specific name.
    ///
    /// Unlike `spawn()` which picks a random available name, this takes an explicit
    /// name parameter. This is useful for:
    /// - @mention routing where the mentioned coworker name is known
    /// - Orphan task recovery where the task owner's name is known
    ///
    /// Creates a new worktree if one doesn't exist, or reuses an existing one.
    /// If `resume` is true, passes `--continue` to claude to resume the previous
    /// session from this worktree (preserving context).
    ///
    /// If `prompt` is provided, waits for the coworker to initialize and sends the
    /// prompt as the initial nudge. This is the preferred way to send initial
    /// instructions as it avoids the race condition of spawning then nudging separately.
    ///
    /// Returns the coworker name on success.
    pub fn spawn_with_name(
        &self,
        name: &str,
        resume: bool,
        prompt: Option<&str>,
    ) -> crate::Result<String> {
        // Check if already running
        {
            let coworkers = self.coworkers.read().unwrap();
            if coworkers.contains_key(name) {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Coworker {} is already running", name),
                });
            }
        }

        // Try to get or create the worktree
        let worktree_path = match self.worktree_manager.create(name) {
            Ok(path) => path,
            Err(WorktreeError::AlreadyExists(_)) => {
                // Worktree exists - check if there's a corresponding tmux window
                let window_exists = tmux::window_exists(&self.session_name, name).unwrap_or(false);

                if window_exists {
                    // There's an active window, so the coworker is actually running
                    return Err(crate::Error::Rpc {
                        code: -32603,
                        message: format!(
                            "Coworker {} is already running (worktree and window exist)",
                            name
                        ),
                    });
                }

                // Worktree exists but no window - validate it's a valid git worktree
                let worktree_path = self.worktree_manager.worktree_path(name);
                if !is_valid_git_worktree(&worktree_path) {
                    // Worktree is corrupted - clean up and recreate
                    tracing::warn!(
                        "Worktree for {} is corrupted (git metadata missing), recreating",
                        name
                    );
                    if let Err(e) = self.worktree_manager.force_cleanup(name) {
                        tracing::warn!("Failed to clean up corrupted worktree: {}", e);
                    }
                    // Try to create fresh worktree
                    match self.worktree_manager.create(name) {
                        Ok(path) => path,
                        Err(e) => {
                            return Err(crate::Error::Rpc {
                                code: -32603,
                                message: format!(
                                    "Failed to recreate worktree for {} after cleanup: {}",
                                    name, e
                                ),
                            });
                        }
                    }
                } else {
                    worktree_path
                }
            }
            Err(e) => {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Failed to create worktree for {}: {}", name, e),
                });
            }
        };

        let working_dir = worktree_path
            .to_str()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "Worktree path is not valid UTF-8".to_string(),
            })?
            .to_string();

        // Create the tmux window and spawn claude in the worktree
        let repo_name = self.worktree_manager.repo_name();
        let session_id = tmux::spawn_claude(
            &self.session_name,
            name,
            &working_dir,
            Some(repo_name),
            resume,
        )?;

        // Record the coworker with their session ID for symlink management
        let coworker = Coworker {
            name: name.to_string(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
            session_id: Some(session_id),
        };

        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.insert(name.to_string(), coworker);
        }

        tracing::info!("Spawned coworker {} with resume={}", name, resume);

        // If a prompt was provided, wait for initialization and send it
        if let Some(prompt_text) = prompt {
            self.send_initial_prompt(name, prompt_text);
        }

        Ok(name.to_string())
    }

    /// Respawn a coworker with a specific name.
    ///
    /// This is used to recover orphaned coworkers whose tmux windows died but
    /// whose worktrees still exist. Unlike `spawn()`, this uses a specific name
    /// rather than selecting a random available name.
    ///
    /// The worktree is reused if it exists, allowing the coworker to resume
    /// where they left off.
    #[deprecated(note = "Use spawn_with_name(name, true, None) instead")]
    pub fn respawn(&self, name: &str) -> crate::Result<()> {
        // Check if already running
        {
            let coworkers = self.coworkers.read().unwrap();
            if coworkers.contains_key(name) {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Coworker {} is already running", name),
                });
            }
        }

        // Check if there's a worktree - respawn requires an existing worktree
        let worktree_path = self.worktree_manager.worktree_path(name);
        if !worktree_path.exists() {
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Cannot respawn {}: no existing worktree at {}",
                    name,
                    worktree_path.display()
                ),
            });
        }

        let working_dir = worktree_path
            .to_str()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "Worktree path is not valid UTF-8".to_string(),
            })?
            .to_string();

        // Spawn Claude in the existing worktree, resuming the previous session
        // so the coworker picks up where they left off
        let repo_name = self.worktree_manager.repo_name();
        let session_id = tmux::spawn_claude(
            &self.session_name,
            name,
            &working_dir,
            Some(repo_name),
            true,
        )?;

        // Record the coworker with their session ID for symlink management
        let coworker = Coworker {
            name: name.to_string(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
            session_id: Some(session_id),
        };

        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.insert(name.to_string(), coworker);
        }

        tracing::info!("Respawned coworker {} in existing worktree", name);
        Ok(())
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

/// Check if a worktree directory is a valid git worktree.
///
/// A worktree can become corrupted if its metadata in `.git/worktrees/<name>/`
/// is removed (e.g., by `git worktree prune`) while the worktree directory still
/// exists. This function validates that git commands work in the directory.
fn is_valid_git_worktree(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }

    // Run `git rev-parse --git-dir` in the worktree directory.
    // This will fail if the worktree metadata is missing or corrupted.
    std::process::Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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

        // Configure git user (required in CI where no global config exists)
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to set git user.email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to set git user.name");

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
    fn test_next_available_name_empty() {
        let (manager, _temp_dir) = test_manager();
        let name = manager.next_available_name();
        assert!(name.is_some());
        // Should be one of the primary avenue names
        assert!(AVENUE_NAMES.contains(&name.as_ref().unwrap().as_str()));
    }

    #[test]
    fn test_next_available_name_with_used_names() {
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
                    session_id: None,
                },
            );
        }

        // Should return a name that is NOT "lexington"
        let name = manager.next_available_name();
        assert!(name.is_some());
        let name = name.unwrap();
        assert_ne!(name, "lexington");
        // Should still be from primary avenue names
        assert!(AVENUE_NAMES.contains(&name.as_str()));
    }

    #[test]
    fn test_next_available_name_overflow() {
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
                        session_id: None,
                    },
                );
            }
        }

        // Should return an overflow name (randomized)
        let name = manager.next_available_name();
        assert!(name.is_some());
        assert!(OVERFLOW_NAMES.contains(&name.as_ref().unwrap().as_str()));
    }

    #[test]
    fn test_next_available_name_exhausted() {
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
                        session_id: None,
                    },
                );
            }
        }

        // Should return None when all names exhausted
        assert_eq!(manager.next_available_name(), None);
    }

    #[test]
    #[allow(deprecated)]
    fn test_respawn_fails_without_worktree() {
        let (manager, _temp_dir) = test_manager();

        // Respawn should fail if no worktree exists
        let result = manager.respawn("nonexistent");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no existing worktree")
        );
    }

    #[test]
    #[allow(deprecated)]
    fn test_respawn_fails_if_already_running() {
        let (manager, _temp_dir) = test_manager();

        // Manually insert a running coworker
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
                    session_id: None,
                },
            );
        }

        // Respawn should fail if coworker is already running
        let result = manager.respawn("lexington");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[test]
    fn test_spawn_with_name_fails_if_already_running() {
        let (manager, _temp_dir) = test_manager();

        // Manually insert a running coworker
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
                    session_id: None,
                },
            );
        }

        // spawn_with_name should fail if coworker is already running
        let result = manager.spawn_with_name("lexington", false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));

        // Also test with resume=true
        let result = manager.spawn_with_name("lexington", true, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[test]
    fn test_is_valid_git_worktree() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to init git repo");

        // Configure git user (required in CI)
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to set git user.email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to set git user.name");

        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to create initial commit");

        // Valid git repo should return true
        assert!(is_valid_git_worktree(temp_dir.path()));

        // Non-existent path should return false
        assert!(!is_valid_git_worktree(std::path::Path::new(
            "/nonexistent/path"
        )));

        // Directory without .git should return false
        let non_git_dir = TempDir::new().expect("Failed to create temp dir");
        assert!(!is_valid_git_worktree(non_git_dir.path()));
    }

    #[test]
    fn test_corrupted_worktree_detection() {
        let (manager, temp_dir) = test_manager();

        // Create a worktree
        let worktree_path = manager.worktree_manager.create("testworker").unwrap();
        assert!(worktree_path.exists());
        assert!(is_valid_git_worktree(&worktree_path));

        // Simulate corruption by removing the git worktree metadata
        // The metadata lives in .git/worktrees/<name>/
        let git_worktrees_dir = temp_dir.path().join(".git").join("worktrees");
        if git_worktrees_dir.exists() {
            std::fs::remove_dir_all(&git_worktrees_dir)
                .expect("Failed to remove worktrees metadata");
        }

        // The worktree directory still exists but is now invalid
        assert!(worktree_path.exists());
        assert!(!is_valid_git_worktree(&worktree_path));
    }
}
