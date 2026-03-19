//! Coworker management for the midtown daemon.
//!
//! Tracks active coworkers and their state, coordinating headless coworker
//! sessions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;
use crate::worktree::WorktreeManager;

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
    /// Daemon-generated UUID, used as the internal HashMap key.
    /// Generated at spawn time, stable for the lifetime of the session.
    #[serde(default = "default_slot_id")]
    pub slot_id: String,
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
    /// The Claude model this coworker is using (e.g., "sonnet", "opus", "haiku")
    #[serde(default = "default_model")]
    pub model: String,
    /// Auth provider backing this coworker session.
    #[serde(default = "default_provider")]
    pub provider: AuthProvider,
    /// Auth profile name for this coworker session.
    /// Used for multi-account usage tracking.
    #[serde(default = "default_profile")]
    pub profile: String,
}

fn default_slot_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn default_model() -> String {
    "sonnet".to_string()
}

fn default_provider() -> AuthProvider {
    AuthProvider::Claude
}

fn default_profile() -> String {
    crate::auth::DEFAULT_PROFILE.to_string()
}

impl Coworker {
    /// Create a Coworker entry for recovery (headless session discovery).
    ///
    /// Used when a running session is found that isn't tracked in
    /// CoworkerManager. Placeholder fields (started_at, current_task,
    /// session_id) will be populated when the coworker registers via RPC.
    pub fn recovered(name: String, working_dir: String) -> Self {
        Self {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name,
            status: CoworkerStatus::Running,
            working_dir,
            started_at: chrono::Utc::now(), // Unknown, use now as approximation
            current_task: None,             // Will be discovered via task tracking
            session_id: None,               // Will be set when coworker registers
            model: default_model(),         // Default to sonnet for recovered sessions
            provider: default_provider(),
            profile: default_profile(),
        }
    }
}

/// Manager for coworker lifecycle.
#[derive(Debug, Clone)]
pub struct CoworkerManager {
    /// Map of coworker name -> coworker info
    coworkers: Arc<RwLock<HashMap<String, Coworker>>>,
    /// Worktree manager for the primary repo
    worktree_manager: Arc<WorktreeManager>,
    /// Worktree managers for additional repos in multi-repo projects
    additional_worktree_managers: Vec<Arc<WorktreeManager>>,
}

impl CoworkerManager {
    /// Create a new coworker manager.
    ///
    /// # Arguments
    /// * `worktree_manager` - Manager for creating isolated git worktrees (primary repo)
    pub fn new(worktree_manager: WorktreeManager) -> Self {
        Self::with_additional_repos(worktree_manager, vec![])
    }

    /// Create a new coworker manager with additional repos for multi-repo projects.
    ///
    /// # Arguments
    /// * `worktree_manager` - Manager for the primary repo
    /// * `additional_worktree_managers` - Managers for additional repos
    pub fn with_additional_repos(
        worktree_manager: WorktreeManager,
        additional_worktree_managers: Vec<WorktreeManager>,
    ) -> Self {
        Self {
            coworkers: Arc::new(RwLock::new(HashMap::new())),
            worktree_manager: Arc::new(worktree_manager),
            additional_worktree_managers: additional_worktree_managers
                .into_iter()
                .map(Arc::new)
                .collect(),
        }
    }

    /// Get a reference to the primary worktree manager.
    pub fn worktree_manager(&self) -> &WorktreeManager {
        &self.worktree_manager
    }

    /// Create worktrees for a coworker in all additional repos (multi-repo projects).
    ///
    /// Returns the list of additional worktree paths to pass as --add-dir to Claude.
    /// Failures in additional repos are logged but don't prevent coworker spawn.
    ///
    /// Uses detached HEAD (not branch-based) to avoid collisions between coworker
    /// names (e.g., "park", "madison") and real branches in additional repos.
    fn create_additional_worktrees(&self, coworker_name: &str) -> Vec<std::path::PathBuf> {
        let mut additional_dirs = Vec::new();
        for mgr in &self.additional_worktree_managers {
            match mgr.create_detached_worktree(coworker_name) {
                Ok(path) => {
                    additional_dirs.push(path);
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to create additional worktree for {} in {}: {}",
                        coworker_name,
                        mgr.repo_name(),
                        e
                    );
                }
            }
        }
        additional_dirs
    }

    /// Clean up worktrees for a coworker in all additional repos.
    fn cleanup_additional_worktrees(&self, coworker_name: &str) {
        for mgr in &self.additional_worktree_managers {
            if let Err(e) = mgr.force_cleanup_task_worktree(coworker_name) {
                tracing::warn!(
                    "Failed to cleanup additional worktree for {} in {}: {}",
                    coworker_name,
                    mgr.repo_name(),
                    e
                );
            }
        }
    }

    /// Remove a coworker from tracking.
    ///
    /// The `SessionManager` handles killing the headless process, and this
    /// method handles the tracking cleanup (removing from HashMap + cleaning
    /// up additional worktrees).
    pub fn deregister(&self, name: &str) {
        // Clean up additional repo worktrees (multi-repo projects)
        self.cleanup_additional_worktrees(name);

        // Remove from tracking (find slot_id by name, then remove)
        {
            let mut coworkers = self.coworkers.write().unwrap();
            let slot_id = coworkers
                .values()
                .find(|cw| cw.name == name)
                .map(|cw| cw.slot_id.clone());
            if let Some(slot_id) = slot_id {
                coworkers.remove(&slot_id);
            }
        }

        tracing::info!("Deregistered coworker {}", name);
    }

    /// Send all coworkers on a break.
    pub fn shutdown_all(&self) -> crate::Result<()> {
        let mut coworkers = self.coworkers.write().unwrap();
        coworkers.clear();
        Ok(())
    }

    /// List all coworkers.
    pub fn list(&self) -> Vec<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.values().cloned().collect()
    }

    /// List only coworkers with `Running` status.
    ///
    /// Use this instead of `list()` when building active-name sets for task
    /// assignment, so that coworkers stuck in `Stopping` or other non-running
    /// states are excluded.
    pub fn list_running(&self) -> Vec<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers
            .values()
            .filter(|cw| cw.status == CoworkerStatus::Running)
            .cloned()
            .collect()
    }

    /// Get a coworker by name (scans values, returns first match).
    pub fn get(&self, name: &str) -> Option<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.values().find(|cw| cw.name == name).cloned()
    }

    /// Get a coworker by session ID.
    ///
    /// Searches all tracked coworkers for one with a matching `session_id`.
    /// This enables session-first lookups alongside existing name-based lookups,
    /// bridging the transition to session-keyed architecture.
    pub fn get_by_session_id(&self, session_id: &str) -> Option<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers
            .values()
            .find(|cw| cw.session_id.as_deref() == Some(session_id))
            .cloned()
    }

    /// Get the session ID for a coworker.
    ///
    /// Returns the Claude Code session ID (UUID) if the coworker is tracked
    /// and has a known session ID. This is used for PR handoff — when a different
    /// coworker needs to resume work on a PR, they can resume the original
    /// author's session to preserve context.
    pub fn get_session_id(&self, name: &str) -> Option<String> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers
            .values()
            .find(|cw| cw.name == name)
            .and_then(|cw| cw.session_id.clone())
    }

    /// Prepare a coworker's worktree and return the working directory and augmented config.
    ///
    /// This handles all worktree lifecycle management:
    /// - Validates the task-based worktree provided via config.working_dir
    /// - Creates worktrees in additional repos (multi-repo)
    ///
    /// The daemon must provide a `working_dir` (task-based worktree) via
    /// `Effect::EnsureWorktree` before spawning. This method validates that
    /// path and returns `(working_dir, augmented_config)` on success.
    pub fn prepare_spawn(
        &self,
        config: &crate::launch::LaunchConfig,
    ) -> crate::Result<(String, crate::launch::LaunchConfig)> {
        let name = &config.name;

        // Check if already running (scan values for name match)
        {
            let coworkers = self.coworkers.read().unwrap();
            if coworkers.values().any(|cw| cw.name == *name) {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!("Coworker {} is already running", name),
                });
            }
        }

        // Resolve the working directory for this session.
        // Task-based coworkers get their worktree via Effect::EnsureWorktree (config.working_dir).
        // Non-task sessions (channel leads, forks) get an on-demand detached worktree.
        let worktree_path = if let Some(ref working_dir) = config.working_dir {
            // Validate the path exists and is a valid git worktree
            if !working_dir.exists() {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!(
                        "Specified working_dir does not exist: {}",
                        working_dir.display()
                    ),
                });
            }
            if !is_valid_git_worktree(working_dir) {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!(
                        "Specified working_dir is not a valid git worktree: {}",
                        working_dir.display()
                    ),
                });
            }
            tracing::info!(
                "Using task-based worktree for {}: {}",
                name,
                working_dir.display()
            );
            working_dir.clone()
        } else {
            // No explicit working_dir — create a detached worktree keyed by session name.
            // This is the path for channel leads, forks, and other non-task sessions.
            match self.worktree_manager.create_detached_worktree(name) {
                Ok(path) => {
                    tracing::info!("Created detached worktree for {}: {}", name, path.display());
                    path
                }
                Err(e) => {
                    return Err(crate::Error::Rpc {
                        code: -32603,
                        message: format!("Failed to create worktree for {}: {}", name, e),
                    });
                }
            }
        };

        let working_dir = worktree_path
            .to_str()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "Worktree path is not valid UTF-8".to_string(),
            })?
            .to_string();

        // Create worktrees in additional repos (multi-repo projects)
        let additional_dirs = self.create_additional_worktrees(name);

        // Augment the config with the additional dirs from worktree creation
        let mut launch_config = config.clone();
        launch_config.additional_dirs.extend(additional_dirs);

        Ok((working_dir, launch_config))
    }

    /// Register a coworker as running after its session has been spawned.
    ///
    /// This adds the coworker to the internal tracking map. Call this after
    /// the headless session has been successfully started.
    ///
    /// If an entry already exists with `session_id: None`, it's treated as a
    /// provisional recovery entry and is updated with the authoritative values
    /// (session_id, working_dir). This allows session recovery to create
    /// provisional entries without blocking legitimate registrations.
    ///
    /// Returns an error if the name was taken by a concurrent spawn (an entry
    /// exists with a non-None session_id).
    #[allow(clippy::too_many_arguments)] // All params are necessary for coworker registration
    pub fn register(
        &self,
        slot_id: &str,
        name: &str,
        working_dir: String,
        session_id: Option<String>,
        model: String,
        provider: AuthProvider,
        profile: String,
    ) -> crate::Result<()> {
        let mut coworkers = self.coworkers.write().unwrap();

        // Check if an entry already exists with this name
        if let Some(existing) = coworkers.values().find(|cw| cw.name == name) {
            // If the existing entry has a session_id, it's a real concurrent spawn race.
            // Fail to prevent overwriting a legitimate registration.
            if existing.session_id.is_some() {
                return Err(crate::Error::Rpc {
                    code: -32603,
                    message: format!(
                        "Coworker {} was registered by another concurrent request",
                        name
                    ),
                });
            }

            // If session_id is None, this is a provisional recovery entry.
            // Remove it and replace with the authoritative entry below.
            let old_slot = existing.slot_id.clone();
            tracing::info!(
                "Updating provisional recovery entry for {} with authoritative values",
                name
            );
            coworkers.remove(&old_slot);
        }

        let coworker = Coworker {
            slot_id: slot_id.to_string(),
            name: name.to_string(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
            session_id,
            model,
            provider,
            profile,
        };
        coworkers.insert(slot_id.to_string(), coworker);

        tracing::info!("Registered coworker {} (slot_id={})", name, slot_id);
        Ok(())
    }

    /// Retain only coworkers whose names are in the given alive set.
    ///
    /// Removes any coworker from the tracking map whose name is not present
    /// in `alive_names`. Used by the session monitor tick to prune entries
    /// for sessions that are no longer alive in the `SessionManager`.
    pub fn retain_alive(&self, alive_names: &std::collections::HashSet<String>) {
        let mut coworkers = self.coworkers.write().unwrap();
        coworkers.retain(|_, cw| alive_names.contains(&cw.name));
    }

    /// Get count of active coworkers.
    pub fn count(&self) -> usize {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.len()
    }

    // ─── Test-only methods ───────────────────────────────────────────────────

    /// Insert a coworker directly into the map (for testing).
    ///
    /// This bypasses the normal spawn flow and is only intended for tests.
    /// Returns true if inserted, false if name was already taken.
    #[doc(hidden)]
    pub fn insert_for_testing(&self, coworker: Coworker) -> bool {
        let mut coworkers = self.coworkers.write().unwrap();
        if coworkers.values().any(|cw| cw.name == coworker.name) {
            return false;
        }
        let slot_id = coworker.slot_id.clone();
        coworkers.insert(slot_id, coworker);
        true
    }

    /// Clear all coworkers (for testing).
    ///
    /// This bypasses the normal shutdown flow and is only intended for tests.
    #[doc(hidden)]
    pub fn clear_for_testing(&self) {
        let mut coworkers = self.coworkers.write().unwrap();
        coworkers.clear();
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
        let manager = CoworkerManager::new(worktree_manager);

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
    fn test_coworker_manager_new() {
        let (manager, _temp_dir) = test_manager();
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_prepare_spawn_fails_if_already_running() {
        let (manager, _temp_dir) = test_manager();

        // Manually insert a running coworker
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            let slot_id = uuid::Uuid::new_v4().to_string();
            coworkers.insert(
                slot_id.clone(),
                Coworker {
                    slot_id,
                    name: "lexington".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    model: "sonnet".to_string(),
                    provider: default_provider(),
                    profile: default_profile(),
                },
            );
        }

        // prepare_spawn should fail if coworker is already running
        let config = crate::launch::LaunchConfig::coworker(
            "lexington",
            "test-repo",
            crate::launch::SessionMode::Fresh,
            None,
            None,
        );
        let result = manager.prepare_spawn(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));

        // Also test with resume=true
        let resume_config = crate::launch::LaunchConfig::coworker(
            "lexington",
            "test-repo",
            crate::launch::SessionMode::Resume,
            None,
            None,
        );
        let result = manager.prepare_spawn(&resume_config);
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

        // Create a task-based worktree
        let worktree_path = manager
            .worktree_manager
            .create_task_worktree("task-42-test")
            .unwrap();
        assert!(worktree_path.exists());
        assert!(is_valid_git_worktree(&worktree_path));

        // Simulate corruption by removing the git worktree metadata
        let git_worktrees_dir = temp_dir.path().join(".git").join("worktrees");
        if git_worktrees_dir.exists() {
            std::fs::remove_dir_all(&git_worktrees_dir)
                .expect("Failed to remove worktrees metadata");
        }

        // The worktree directory still exists but is now invalid
        assert!(worktree_path.exists());
        assert!(!is_valid_git_worktree(&worktree_path));
    }

    #[test]
    fn test_worktree_recovery_flow() {
        // Test the full recovery flow: detect corrupted → cleanup → recreate
        let (manager, temp_dir) = test_manager();

        // Create a task-based worktree
        let worktree_path = manager
            .worktree_manager
            .create_task_worktree("task-42-test")
            .unwrap();
        assert!(worktree_path.exists());
        assert!(is_valid_git_worktree(&worktree_path));

        // Simulate corruption by removing the git worktree metadata
        let git_worktrees_dir = temp_dir.path().join(".git").join("worktrees");
        if git_worktrees_dir.exists() {
            std::fs::remove_dir_all(&git_worktrees_dir)
                .expect("Failed to remove worktrees metadata");
        }

        // Verify it's now corrupted
        assert!(worktree_path.exists());
        assert!(!is_valid_git_worktree(&worktree_path));

        // Now test recovery: force_cleanup_task_worktree should remove the directory
        manager
            .worktree_manager
            .force_cleanup_task_worktree("task-42-test")
            .expect("force_cleanup_task_worktree should succeed");

        // Worktree directory should be gone
        assert!(!worktree_path.exists());

        // Recreate should now succeed
        let new_path = manager
            .worktree_manager
            .create_task_worktree("task-42-test")
            .expect("create should succeed after cleanup");

        // New worktree should be valid
        assert!(new_path.exists());
        assert!(is_valid_git_worktree(&new_path));
    }

    /// Create a git repo in a temp dir for use as an additional repo
    fn create_git_repo(dir: &std::path::Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("Failed to init git repo");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git user.email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git user.name");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(dir)
            .output()
            .expect("Failed to create initial commit");
    }

    #[test]
    fn test_with_additional_repos() {
        let primary_dir = TempDir::new().expect("Failed to create primary temp dir");
        create_git_repo(primary_dir.path());

        let extra_dir = TempDir::new().expect("Failed to create extra temp dir");
        create_git_repo(extra_dir.path());

        let primary_wt =
            WorktreeManager::new(primary_dir.path().to_path_buf()).expect("primary wt manager");
        let extra_wt =
            WorktreeManager::new(extra_dir.path().to_path_buf()).expect("extra wt manager");

        let manager = CoworkerManager::with_additional_repos(primary_wt, vec![extra_wt]);

        // Verify additional managers are tracked
        assert_eq!(manager.additional_worktree_managers.len(), 1);
    }

    #[test]
    fn test_create_additional_worktrees() {
        let primary_dir = TempDir::new().expect("Failed to create primary temp dir");
        create_git_repo(primary_dir.path());

        let extra_dir = TempDir::new().expect("Failed to create extra temp dir");
        create_git_repo(extra_dir.path());

        let primary_wt =
            WorktreeManager::new(primary_dir.path().to_path_buf()).expect("primary wt manager");
        let extra_wt =
            WorktreeManager::new(extra_dir.path().to_path_buf()).expect("extra wt manager");

        let manager = CoworkerManager::with_additional_repos(primary_wt, vec![extra_wt]);

        let additional_dirs = manager.create_additional_worktrees("testworker");
        assert_eq!(additional_dirs.len(), 1);
        assert!(additional_dirs[0].exists());
        assert!(is_valid_git_worktree(&additional_dirs[0]));
    }

    #[test]
    fn test_create_additional_worktrees_empty_when_no_additional() {
        let (manager, _temp_dir) = test_manager();
        let additional_dirs = manager.create_additional_worktrees("testworker");
        assert!(additional_dirs.is_empty());
    }

    #[test]
    fn test_create_additional_worktrees_reuses_existing() {
        let primary_dir = TempDir::new().expect("Failed to create primary temp dir");
        create_git_repo(primary_dir.path());

        let extra_dir = TempDir::new().expect("Failed to create extra temp dir");
        create_git_repo(extra_dir.path());

        let primary_wt =
            WorktreeManager::new(primary_dir.path().to_path_buf()).expect("primary wt manager");
        let extra_wt =
            WorktreeManager::new(extra_dir.path().to_path_buf()).expect("extra wt manager");

        let manager = CoworkerManager::with_additional_repos(primary_wt, vec![extra_wt]);

        // Create first time
        let dirs1 = manager.create_additional_worktrees("testworker");
        assert_eq!(dirs1.len(), 1);

        // Create again - should reuse existing
        let dirs2 = manager.create_additional_worktrees("testworker");
        assert_eq!(dirs2.len(), 1);
        assert_eq!(dirs1[0], dirs2[0]);
    }

    #[test]
    fn test_cleanup_additional_worktrees() {
        let primary_dir = TempDir::new().expect("Failed to create primary temp dir");
        create_git_repo(primary_dir.path());

        let extra_dir = TempDir::new().expect("Failed to create extra temp dir");
        create_git_repo(extra_dir.path());

        let primary_wt =
            WorktreeManager::new(primary_dir.path().to_path_buf()).expect("primary wt manager");
        let extra_wt =
            WorktreeManager::new(extra_dir.path().to_path_buf()).expect("extra wt manager");
        let extra_wt_path = extra_wt.task_worktree_path("testworker");

        let manager = CoworkerManager::with_additional_repos(primary_wt, vec![extra_wt]);

        // Create worktrees
        let dirs = manager.create_additional_worktrees("testworker");
        assert_eq!(dirs.len(), 1);
        assert!(extra_wt_path.exists());

        // Cleanup
        manager.cleanup_additional_worktrees("testworker");
        assert!(!extra_wt_path.exists());
    }

    #[test]
    fn test_list_running_excludes_stopping_coworkers() {
        let (manager, _temp_dir) = test_manager();

        {
            let mut coworkers = manager.coworkers.write().unwrap();
            let slot_id = uuid::Uuid::new_v4().to_string();
            coworkers.insert(
                slot_id.clone(),
                Coworker {
                    slot_id,
                    name: "lexington".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    model: "sonnet".to_string(),
                    provider: default_provider(),
                    profile: default_profile(),
                },
            );
            let slot_id = uuid::Uuid::new_v4().to_string();
            coworkers.insert(
                slot_id.clone(),
                Coworker {
                    slot_id,
                    name: "park".to_string(),
                    status: CoworkerStatus::Stopping,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    model: "sonnet".to_string(),
                    provider: default_provider(),
                    profile: default_profile(),
                },
            );
            let slot_id = uuid::Uuid::new_v4().to_string();
            coworkers.insert(
                slot_id.clone(),
                Coworker {
                    slot_id,
                    name: "madison".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    model: "sonnet".to_string(),
                    provider: default_provider(),
                    profile: default_profile(),
                },
            );
        }

        // list() returns all 3 coworkers
        assert_eq!(manager.list().len(), 3);

        // list_running() should only return the 2 Running coworkers
        let running = manager.list_running();
        assert_eq!(running.len(), 2);
        let names: Vec<&str> = running.iter().map(|cw| cw.name.as_str()).collect();
        assert!(names.contains(&"lexington"));
        assert!(names.contains(&"madison"));
        assert!(!names.contains(&"park"));
    }
}
