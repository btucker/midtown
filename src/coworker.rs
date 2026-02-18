//! Coworker management for the midtown daemon.
//!
//! Tracks active coworkers and their state, coordinating headless coworker
//! sessions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;
use crate::session_key::SessionKey;
use crate::worktree::{WorktreeError, WorktreeManager};

/// Primary Manhattan avenue names used for coworker naming.
///
/// Also used by channel validation to reject these reserved names
/// (a channel named "park" would collide with the "park" channel lead session).
pub const AVENUE_NAMES: &[&str] = &[
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
pub const OVERFLOW_NAMES: &[&str] = &["bleecker", "houston", "canal", "spring", "prince", "mercer"];

/// Check if a name is a known coworker name (avenue or overflow).
///
/// Used to prevent coworker worktree names from being registered as projects.
pub fn is_coworker_name(name: &str) -> bool {
    AVENUE_NAMES.contains(&name) || OVERFLOW_NAMES.contains(&name)
}

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
    /// Get the SessionKey for this coworker, if a session ID is known.
    ///
    /// Returns `None` if `session_id` is `None` (e.g., provisional recovery entries
    /// that haven't been registered yet).
    pub fn session_key(&self) -> Option<SessionKey> {
        self.session_id
            .as_ref()
            .map(|sid| SessionKey::new(&self.name, sid))
    }

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

    /// Get a randomly selected available avenue name.
    ///
    /// Randomly selects from available primary avenue names first.
    /// Falls back to overflow names only when all primary names are in use.
    ///
    /// This can be used to get a name before spawning, e.g., to assign
    /// task ownership atomically before the coworker starts.
    pub fn next_available_name(&self) -> Option<String> {
        self.next_available_name_excluding(&std::collections::HashSet::new())
    }

    /// Pick the next available coworker name, excluding both registered coworkers
    /// and any additional reserved names (e.g., channel lead session names that
    /// could collide with avenue names after the ch- prefix was removed).
    pub fn next_available_name_excluding(
        &self,
        reserved_names: &std::collections::HashSet<String>,
    ) -> Option<String> {
        let coworkers = self.coworkers.read().unwrap();

        // Collect names currently in use (scan values)
        let used_names: std::collections::HashSet<&str> =
            coworkers.values().map(|cw| cw.name.as_str()).collect();

        // Collect available primary avenue names
        let available: Vec<&str> = AVENUE_NAMES
            .iter()
            .filter(|name| !used_names.contains(**name) && !reserved_names.contains(**name))
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
            .filter(|name| !used_names.contains(**name) && !reserved_names.contains(**name))
            .copied()
            .collect();

        if !overflow_available.is_empty() {
            let idx = fastrand::usize(..overflow_available.len());
            return Some(overflow_available[idx].to_string());
        }

        None
    }

    /// Create worktrees for a coworker in all additional repos (multi-repo projects).
    ///
    /// Returns the list of additional worktree paths to pass as --add-dir to Claude.
    /// Failures in additional repos are logged but don't prevent coworker spawn.
    #[allow(deprecated)] // Legacy worktree layout for additional repos
    fn create_additional_worktrees(&self, coworker_name: &str) -> Vec<std::path::PathBuf> {
        let mut additional_dirs = Vec::new();
        for mgr in &self.additional_worktree_managers {
            match mgr.create(coworker_name) {
                Ok(path) => {
                    additional_dirs.push(path);
                }
                Err(WorktreeError::AlreadyExists(_)) => {
                    // Reuse existing worktree path
                    let path = mgr.worktree_path(coworker_name);
                    if is_valid_git_worktree(&path) {
                        additional_dirs.push(path);
                    } else {
                        // Corrupted - try cleanup + recreate
                        tracing::warn!(
                            "Additional worktree for {} in {} is corrupted, recreating",
                            coworker_name,
                            mgr.repo_name()
                        );
                        let _ = mgr.force_cleanup(coworker_name);
                        match mgr.create(coworker_name) {
                            Ok(path) => additional_dirs.push(path),
                            Err(e) => {
                                tracing::error!(
                                    "Failed to recreate additional worktree for {} in {}: {}",
                                    coworker_name,
                                    mgr.repo_name(),
                                    e
                                );
                            }
                        }
                    }
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
            if let Err(e) = mgr.force_cleanup(coworker_name) {
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

    /// Find all orphaned worktree names — worktrees with no active coworker.
    ///
    /// This is useful for clearing state (like reviewer assignments) for coworkers
    /// whose sessions have ended unexpectedly.
    pub fn find_orphaned_worktree_names(&self) -> Vec<String> {
        let active_names: Vec<String> = {
            let coworkers = self.coworkers.read().unwrap();
            coworkers.values().map(|cw| cw.name.clone()).collect()
        };
        self.worktree_manager.find_orphaned_worktrees(&active_names)
    }

    /// Clean up orphaned worktrees that have no active coworker.
    ///
    /// For each orphaned worktree:
    /// - If the branch has no commits beyond the base, delete it safely
    /// - If the branch has commits, flag it (returned in the result)
    ///
    /// Returns a list of coworker names whose worktrees have unmerged commits
    /// and should be flagged to the Lead.
    /// Clean up orphaned worktrees that have no active coworker.
    ///
    /// To avoid saturating the blocking thread pool with expensive git and gh CLI
    /// operations, this processes at most `max_per_tick` worktrees per call.
    /// Pass `None` or a large number to process all orphaned worktrees at once.
    pub fn cleanup_orphaned_worktrees(&self, max_per_tick: Option<usize>) -> Vec<String> {
        let active_names: Vec<String> = {
            let coworkers = self.coworkers.read().unwrap();
            coworkers.values().map(|cw| cw.name.clone()).collect()
        };

        let orphaned = self.worktree_manager.find_orphaned_worktrees(&active_names);
        let mut flagged = Vec::new();

        // Limit how many worktrees we process per tick to avoid blocking the
        // thread pool for too long. Each cleanup involves multiple git/gh calls.
        let limit = max_per_tick.unwrap_or(usize::MAX);
        let to_process = orphaned.into_iter().take(limit);

        for name in to_process {
            match self.worktree_manager.safe_cleanup(&name) {
                Ok(true) => {
                    tracing::info!("Cleaned up empty orphaned worktree for {}", name);
                }
                Ok(false) => {
                    // Log at debug level - the actual rate-limited warning happens
                    // in dispatch::cleanup_orphaned_worktrees() via OrphanTracker.
                    tracing::debug!(
                        "Orphaned worktree for {} has unmerged commits - will check filters",
                        name
                    );
                    flagged.push(name);
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup orphaned worktree for {}: {}", name, e);
                }
            }
        }

        flagged
    }

    /// Force cleanup a worktree by name.
    ///
    /// This removes the worktree and its associated branch, regardless of whether
    /// it has commits. Use only when you know it's safe (e.g., PR was merged).
    pub fn force_cleanup_worktree(
        &self,
        coworker_name: &str,
    ) -> Result<(), crate::worktree::WorktreeError> {
        self.worktree_manager.force_cleanup(coworker_name)
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

    /// Get the SessionKey for a coworker by name.
    ///
    /// Returns a `SessionKey` combining the coworker's name and session ID.
    /// Returns `None` if the coworker doesn't exist or has no session ID yet.
    pub fn session_key(&self, name: &str) -> Option<SessionKey> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers
            .values()
            .find(|cw| cw.name == name)
            .and_then(|cw| {
                cw.session_id
                    .as_ref()
                    .map(|sid| SessionKey::new(&cw.name, sid))
            })
    }

    /// Get all SessionKeys for coworkers with a given name.
    ///
    /// Currently returns at most one (single-session-per-name), but this method
    /// is designed for the multi-session future where a name can have multiple
    /// concurrent sessions.
    pub fn session_keys_for_name(&self, name: &str) -> Vec<SessionKey> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers
            .values()
            .filter(|cw| cw.name == name)
            .filter_map(|cw| {
                cw.session_id
                    .as_ref()
                    .map(|sid| SessionKey::new(&cw.name, sid))
            })
            .collect()
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

    /// Get the branch name checked out in a coworker's worktree.
    ///
    /// Returns None if the worktree doesn't exist or is in detached HEAD state.
    pub fn get_worktree_branch(&self, name: &str) -> Option<String> {
        self.worktree_manager.get_branch(name)
    }

    /// Check if a coworker's branch has a merged PR on GitHub.
    ///
    /// Uses `gh pr list` to check if the branch's PR was merged. This is
    /// an expensive operation (calls gh CLI), so should only be used as a
    /// fallback when cached data doesn't cover the branch.
    pub fn is_branch_pr_merged(&self, name: &str) -> bool {
        self.worktree_manager.is_branch_pr_merged(name)
    }

    /// Check if a worktree's HEAD is reachable from the default branch (main).
    ///
    /// Returns `true` if all commits in the worktree are already on main,
    /// indicating the worktree can be safely cleaned up.
    pub fn is_worktree_head_on_main(&self, name: &str) -> bool {
        self.worktree_manager
            .is_head_reachable_from_default_branch(name)
    }

    /// Clean up stale local branches that match coworker naming patterns
    /// and are already fully merged into the default branch.
    ///
    /// Returns the list of deleted branch names.
    pub fn clean_stale_coworker_branches(&self) -> Vec<String> {
        self.worktree_manager.clean_stale_coworker_branches()
    }

    /// Prepare a coworker's worktree and return the working directory and augmented config.
    ///
    /// This handles all worktree lifecycle management:
    /// - Creates a new worktree if one doesn't exist
    /// - Reuses valid existing worktrees (for orphan recovery, break-resume)
    /// - Detects and recreates corrupted worktrees
    /// - Creates worktrees in additional repos (multi-repo)
    /// - Ensures the worktree is not on the default branch
    ///
    /// Returns `(working_dir, augmented_config)` on success.
    #[allow(deprecated)] // Legacy worktree layout when no working_dir override
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

        // If a working_dir override is provided (task-based worktree), validate and use it
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
            // Legacy path: create or reuse coworker-named worktree
            match self.worktree_manager.create(name) {
                Ok(path) => path,
                Err(WorktreeError::AlreadyExists(_)) => {
                    // Worktree exists but no active session - validate it
                    let worktree_path = self.worktree_manager.worktree_path(name);
                    if !is_valid_git_worktree(&worktree_path) {
                        tracing::warn!(
                            "Worktree for {} is corrupted (git metadata missing), recreating",
                            name
                        );
                        self.worktree_manager.force_cleanup(name).map_err(|e| {
                            crate::Error::Rpc {
                                code: -32603,
                                message: format!(
                                    "Failed to cleanup corrupted worktree for {}: {}",
                                    name, e
                                ),
                            }
                        })?;

                        self.worktree_manager
                            .create(name)
                            .map_err(|e| crate::Error::Rpc {
                                code: -32603,
                                message: format!(
                                    "Failed to recreate worktree for {} after cleanup: {}",
                                    name, e
                                ),
                            })?
                    } else {
                        tracing::info!("Reusing existing valid worktree for {}", name);

                        // Safety check: ensure the worktree is not on the default branch.
                        if self.worktree_manager.is_on_default_branch(name) {
                            tracing::warn!(
                                "Coworker {} worktree is on default branch - creating recovery branch",
                                name
                            );
                            match self.worktree_manager.checkout_new_branch(name, "recovery") {
                                Ok(branch) => {
                                    tracing::info!(
                                        "Created recovery branch {} for coworker {}",
                                        branch,
                                        name
                                    );
                                }
                                Err(e) => {
                                    return Err(crate::Error::Rpc {
                                        code: -32603,
                                        message: format!(
                                            "Coworker {} is on default branch and recovery failed: {}",
                                            name, e
                                        ),
                                    });
                                }
                            }
                        }

                        worktree_path
                    }
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
#[allow(deprecated)]
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
                let slot_id = uuid::Uuid::new_v4().to_string();
                coworkers.insert(
                    slot_id.clone(),
                    Coworker {
                        slot_id,
                        name: name.to_string(),
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
                let slot_id = uuid::Uuid::new_v4().to_string();
                coworkers.insert(
                    slot_id.clone(),
                    Coworker {
                        slot_id,
                        name: name.to_string(),
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
        }

        // Should return None when all names exhausted
        assert_eq!(manager.next_available_name(), None);
    }

    #[test]
    fn test_next_available_name_excludes_channel_lead_names() {
        let (manager, _temp_dir) = test_manager();

        // Reserve "park" as a channel lead name
        let reserved: std::collections::HashSet<String> =
            ["park"].iter().map(|s| s.to_string()).collect();

        // Run multiple times to ensure "park" is never returned
        for _ in 0..50 {
            let name = manager.next_available_name_excluding(&reserved);
            assert!(name.is_some());
            assert_ne!(
                name.as_deref(),
                Some("park"),
                "Channel lead name 'park' must not be allocated to a regular coworker"
            );
        }
    }

    #[test]
    fn test_is_coworker_name() {
        // Avenue names should be recognized
        assert!(is_coworker_name("broadway"));
        assert!(is_coworker_name("amsterdam"));
        assert!(is_coworker_name("columbus"));
        assert!(is_coworker_name("vernon"));
        assert!(is_coworker_name("park"));

        // Overflow names should be recognized
        assert!(is_coworker_name("bleecker"));
        assert!(is_coworker_name("houston"));
        assert!(is_coworker_name("canal"));

        // Real project names should not match
        assert!(!is_coworker_name("midtown"));
        assert!(!is_coworker_name("my-project"));
        assert!(!is_coworker_name("distiller"));
        assert!(!is_coworker_name("default"));
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

    #[test]
    fn test_worktree_recovery_flow() {
        // Test the full recovery flow: detect corrupted → cleanup → recreate
        let (manager, temp_dir) = test_manager();

        // Create a worktree
        let worktree_path = manager.worktree_manager.create("testworker").unwrap();
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

        // Now test recovery: force_cleanup should remove the directory
        manager
            .worktree_manager
            .force_cleanup("testworker")
            .expect("force_cleanup should succeed");

        // Worktree directory should be gone
        assert!(!worktree_path.exists());

        // Recreate should now succeed
        let new_path = manager
            .worktree_manager
            .create("testworker")
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
        let extra_wt_path = extra_wt.worktree_path("testworker");

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
