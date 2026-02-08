//! Coworker management for the midtown daemon.
//!
//! Tracks active coworkers and their state, coordinating with tmux windows
//! within the project session.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tmux;
use crate::worktree::{WorktreeError, WorktreeManager};

/// Timeout for waiting for the lead's input to be empty before nudging.
const LEAD_INPUT_WAIT_TIMEOUT: Duration = Duration::from_secs(90);

/// Timeout for waiting for coworker input to be stable before nudging.
/// We wait longer than lead (120s vs 90s) because coworkers are typically
/// controlled by Claude Code and may take longer to process.
const COWORKER_INPUT_MAX_WAIT: Duration = Duration::from_secs(120);

/// Duration that input must be stable (unchanged) before we consider it safe to nudge.
/// Per task requirements: "only append if content hasn't changed for 20 seconds"
const COWORKER_INPUT_STABLE_DURATION: Duration = Duration::from_secs(20);

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
    /// Whether this coworker has an isolated task list (e.g., review coworkers)
    /// Isolated coworkers are sent on a break immediately when they go idle.
    #[serde(default)]
    pub isolated_tasks: bool,
}

impl Coworker {
    /// Create a Coworker entry for recovery (tmux or headless session discovery).
    ///
    /// Used by `sync_with_tmux()` when a running session is found that isn't
    /// tracked in CoworkerManager. Placeholder fields (started_at, current_task,
    /// session_id) will be populated when the coworker registers via RPC.
    pub fn recovered(name: String, working_dir: String) -> Self {
        Self {
            name,
            status: CoworkerStatus::Running,
            working_dir,
            started_at: chrono::Utc::now(), // Unknown, use now as approximation
            current_task: None,             // Will be discovered via task tracking
            session_id: None,               // Will be set when coworker registers
            isolated_tasks: false,          // Assume shared task list (conservative default)
        }
    }
}

/// A queued nudge message for a coworker.
#[derive(Debug, Clone)]
struct CoworkerNudge {
    /// Target coworker name
    name: String,
    /// The nudge message text
    message: String,
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
    /// The tmux session name for the project (e.g., "midtown-projectname")
    session_name: String,
    /// Names of coworkers discovered from tmux on startup (before daemon was managing them).
    /// Used to nudge them to continue their tasks after daemon restart.
    discovered_on_startup: Arc<RwLock<Vec<String>>>,
    /// Queue for lead nudges. A background thread waits for the lead's input to
    /// be empty before delivering each nudge, so the daemon loop is never blocked.
    lead_nudge_tx: mpsc::Sender<String>,
    /// Queue for coworker nudges. A background thread waits for each coworker's input
    /// to be stable (no typing) before delivering nudges, preventing interruption.
    coworker_nudge_tx: mpsc::Sender<CoworkerNudge>,
}

impl CoworkerManager {
    /// Create a new coworker manager.
    ///
    /// # Arguments
    /// * `session_name` - The tmux session name (e.g., "midtown-projectname")
    /// * `worktree_manager` - Manager for creating isolated git worktrees (primary repo)
    pub fn new(session_name: impl Into<String>, worktree_manager: WorktreeManager) -> Self {
        Self::with_additional_repos(session_name, worktree_manager, vec![])
    }

    /// Create a new coworker manager with additional repos for multi-repo projects.
    ///
    /// # Arguments
    /// * `session_name` - The tmux session name (e.g., "midtown-projectname")
    /// * `worktree_manager` - Manager for the primary repo
    /// * `additional_worktree_managers` - Managers for additional repos
    pub fn with_additional_repos(
        session_name: impl Into<String>,
        worktree_manager: WorktreeManager,
        additional_worktree_managers: Vec<WorktreeManager>,
    ) -> Self {
        let session = session_name.into();

        // Spawn a background thread to process lead nudges with input-empty waiting.
        // The thread waits for the lead's input to be clear before delivering each nudge,
        // ensuring the daemon loop is never blocked and nudges arrive in FIFO order.
        let (lead_nudge_tx, lead_nudge_rx) = mpsc::channel::<String>();
        let nudge_session = session.clone();
        std::thread::Builder::new()
            .name("lead-nudge-queue".into())
            .spawn(move || {
                Self::lead_nudge_worker(&nudge_session, lead_nudge_rx);
            })
            .expect("Failed to spawn lead nudge worker thread");

        // Spawn a background thread to process coworker nudges with input-stability waiting.
        // The thread waits for each coworker's input to be stable (not actively typing)
        // before delivering nudges, preventing interruption of user typing.
        let (coworker_nudge_tx, coworker_nudge_rx) = mpsc::channel::<CoworkerNudge>();
        let coworker_nudge_session = session.clone();
        let last_nudge_map: Arc<RwLock<HashMap<String, String>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let last_nudge_map_clone = Arc::clone(&last_nudge_map);
        std::thread::Builder::new()
            .name("coworker-nudge-queue".into())
            .spawn(move || {
                Self::coworker_nudge_worker(
                    &coworker_nudge_session,
                    coworker_nudge_rx,
                    last_nudge_map_clone,
                );
            })
            .expect("Failed to spawn coworker nudge worker thread");

        let manager = Self {
            coworkers: Arc::new(RwLock::new(HashMap::new())),
            worktree_manager: Arc::new(worktree_manager),
            additional_worktree_managers: additional_worktree_managers
                .into_iter()
                .map(Arc::new)
                .collect(),
            session_name: session,
            discovered_on_startup: Arc::new(RwLock::new(Vec::new())),
            lead_nudge_tx,
            coworker_nudge_tx,
        };

        // Discover existing coworkers from tmux on startup
        match manager.discover_existing() {
            Ok(names) => {
                *manager.discovered_on_startup.write().unwrap() = names;
            }
            Err(e) => {
                tracing::warn!("Failed to discover existing coworkers: {}", e);
            }
        }

        manager
    }

    /// Returns the tmux session name (e.g., "midtown-projectname").
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Get a reference to the primary worktree manager.
    pub fn worktree_manager(&self) -> &WorktreeManager {
        &self.worktree_manager
    }

    /// Take the list of coworker names discovered from tmux on startup.
    ///
    /// This drains the list so it can only be consumed once. The daemon uses
    /// this to nudge discovered coworkers to continue their assigned tasks
    /// after a daemon restart.
    pub fn take_discovered_on_startup(&self) -> Vec<String> {
        std::mem::take(&mut *self.discovered_on_startup.write().unwrap())
    }

    /// Discover existing coworker windows from tmux and add them to tracking.
    ///
    /// This is called on startup to recover coworkers that were running before
    /// the daemon was restarted. The coworkers continue running uninterrupted.
    ///
    /// Returns the names of discovered coworkers so the daemon can nudge them
    /// to continue their assigned tasks.
    fn discover_existing(&self) -> crate::Result<Vec<String>> {
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
                return Ok(Vec::new());
            }
        };

        let mut discovered_names = Vec::new();
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
            // Assume not isolated (shared task list) for discovered coworkers - they were
            // likely regular coworkers. Isolated review coworkers go on a break when idle anyway.
            let coworker = Coworker {
                name: window_name.clone(),
                status: CoworkerStatus::Running,
                working_dir,
                started_at: Utc::now(), // Unknown, use now as approximation
                current_task: None,     // Will be discovered via task tracking
                session_id: None,       // Will be set when coworker registers
                isolated_tasks: false,
            };

            coworkers.insert(window_name.clone(), coworker);
            discovered_names.push(window_name.clone());
            tracing::info!("Discovered existing coworker: {}", window_name);
        }

        if !discovered_names.is_empty() {
            tracing::info!(
                "Discovered {} existing coworker(s) from tmux session",
                discovered_names.len()
            );
        }

        Ok(discovered_names)
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
    /// Spawn a new coworker, automatically selecting the next available name.
    ///
    /// The `config.name` field is ignored; a name is chosen via `next_available_name()`
    /// and injected into the config before delegating to `spawn_with_name()`.
    pub fn spawn(&self, config: &tmux::ClaudeLaunchConfig) -> crate::Result<String> {
        let name = self
            .next_available_name()
            .ok_or_else(|| crate::Error::Rpc {
                code: -32603,
                message: "No available coworker slots (all avenue names in use)".to_string(),
            })?;

        let mut config = config.clone();
        config.name = name;
        self.spawn_with_name(&config)
    }

    /// Create worktrees for a coworker in all additional repos (multi-repo projects).
    ///
    /// Returns the list of additional worktree paths to pass as --add-dir to Claude.
    /// Failures in additional repos are logged but don't prevent coworker spawn.
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

    /// Send a coworker on a break (shut down their tmux session).
    ///
    /// This is the **tmux legacy path**. For headless coworkers, use
    /// `deregister()` after calling `SessionManager::shutdown()`.
    pub fn shutdown(&self, name: &str) -> crate::Result<()> {
        // Update status to stopping
        {
            let mut coworkers = self.coworkers.write().unwrap();
            if let Some(cw) = coworkers.get_mut(name) {
                cw.status = CoworkerStatus::Stopping;
                tracing::debug!("Coworker {} status: Running -> Stopping", name);
            } else {
                return Err(crate::Error::Rpc {
                    code: -32602,
                    message: format!("Coworker not found: {}", name),
                });
            }
        }

        // Kill ALL tmux windows with this name — uses window IDs to handle
        // duplicates that accumulate when kill_window (name-based) fails with
        // ambiguous targets. This prevents orphaned windows across break/respawn cycles.
        let kill_result = tmux::kill_all_windows_by_name(&self.session_name, name).map(|n| {
            if n > 1 {
                tracing::warn!("Killed {} duplicate '{}' windows during shutdown", n, name);
            }
        });

        // Clean up additional repo worktrees (multi-repo projects)
        self.cleanup_additional_worktrees(name);

        // Remove from tracking
        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.remove(name);
        }

        if kill_result.is_ok() {
            tracing::info!("Shut down coworker {}", name);
        }

        kill_result
    }

    /// Remove a coworker from tracking without killing any tmux windows.
    ///
    /// Used by the headless shutdown path: the `SessionManager` handles
    /// killing the headless process, and this method handles the tracking
    /// cleanup (removing from HashMap + cleaning up additional worktrees).
    pub fn deregister(&self, name: &str) {
        // Clean up additional repo worktrees (multi-repo projects)
        self.cleanup_additional_worktrees(name);

        // Remove from tracking
        {
            let mut coworkers = self.coworkers.write().unwrap();
            coworkers.remove(name);
        }

        tracing::info!("Deregistered coworker {}", name);
    }

    /// Send all coworkers on a break.
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

    /// Find all orphaned worktree names — worktrees with no active coworker.
    ///
    /// This is useful for clearing state (like reviewer assignments) for coworkers
    /// whose sessions have ended unexpectedly.
    pub fn find_orphaned_worktree_names(&self) -> Vec<String> {
        let active_names: Vec<String> = {
            let coworkers = self.coworkers.read().unwrap();
            coworkers.keys().cloned().collect()
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
            coworkers.keys().cloned().collect()
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

    /// Check if a coworker has an active tmux window.
    ///
    /// This directly queries tmux, bypassing the in-memory coworker map.
    /// Used to prevent race conditions where a coworker exists in tmux but
    /// hasn't been synced to the daemon's internal state yet.
    pub fn has_tmux_window(&self, coworker_name: &str) -> bool {
        crate::tmux::window_exists(&self.session_name, coworker_name).unwrap_or(false)
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

    /// Get a coworker by name.
    pub fn get(&self, name: &str) -> Option<Coworker> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.get(name).cloned()
    }

    /// Get the session ID for a coworker.
    ///
    /// Returns the Claude Code session ID (UUID) if the coworker is tracked
    /// and has a known session ID. This is used for PR handoff — when a different
    /// coworker needs to resume work on a PR, they can resume the original
    /// author's session to preserve context.
    pub fn get_session_id(&self, name: &str) -> Option<String> {
        let coworkers = self.coworkers.read().unwrap();
        coworkers.get(name).and_then(|cw| cw.session_id.clone())
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

    /// Clean up stale local branches that match coworker naming patterns
    /// and are already fully merged into the default branch.
    ///
    /// Returns the list of deleted branch names.
    pub fn clean_stale_coworker_branches(&self) -> Vec<String> {
        self.worktree_manager.clean_stale_coworker_branches()
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

    /// Update a coworker's tmux tab with a pre-formatted status string.
    ///
    /// Unlike `update_status_display` which passes text through `parse_status()`,
    /// this method uses the formatted string directly (e.g., "dev#42" from a
    /// structured state file).
    pub fn update_status_formatted(&self, name: &str, formatted: &str) -> crate::Result<()> {
        {
            let coworkers = self.coworkers.read().unwrap();
            if !coworkers.contains_key(name) {
                return Ok(());
            }
        }

        tmux::rename_window_formatted(&self.session_name, name, formatted)
    }

    /// Send a nudge (input) to a coworker.
    ///
    /// The nudge is queued and delivered by a background thread that waits for
    /// the coworker's input prompt to be stable (no active typing) before sending.
    /// This prevents nudges from corrupting text the user is currently typing.
    ///
    /// Returns immediately — the actual delivery happens asynchronously.
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

        // Queue the nudge for async delivery with input-stability waiting
        self.coworker_nudge_tx
            .send(CoworkerNudge {
                name: name.to_string(),
                message: message.to_string(),
            })
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!("Coworker nudge queue closed: {}", e),
            })
    }

    /// Send a nudge directly to a coworker without waiting for input stability.
    ///
    /// This bypasses the normal input-waiting queue and sends immediately.
    /// Use sparingly - only for urgent interrupts that shouldn't be delayed.
    pub fn nudge_immediate(&self, name: &str, message: &str) -> crate::Result<()> {
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

        // Send keys directly to the tmux window
        tmux::send_keys(&self.session_name, name, message)
    }

    /// Send a nudge (input) to the Lead session.
    ///
    /// The nudge is queued and delivered by a background thread that waits for
    /// the lead's input prompt to be empty before sending. This prevents nudges
    /// from corrupting text the human is currently typing. If the input isn't
    /// empty after 90 seconds, the nudge is sent anyway.
    ///
    /// Returns immediately — the actual delivery happens asynchronously.
    pub fn nudge_lead(&self, message: &str) -> crate::Result<()> {
        // Check if lead window exists before queuing
        if !tmux::window_exists(&self.session_name, "lead")? {
            return Err(crate::Error::Rpc {
                code: -32602,
                message: "Lead session not found".to_string(),
            });
        }

        self.lead_nudge_tx
            .send(message.to_string())
            .map_err(|e| crate::Error::Rpc {
                code: -32603,
                message: format!("Lead nudge queue closed: {}", e),
            })
    }

    /// Send a special key (like Escape) to a coworker or the lead.
    ///
    /// Unlike nudge, this sends a raw key without text, useful for canceling
    /// operations or escaping from vim mode.
    ///
    /// Note: This bypasses the lead nudge queue intentionally. The Escape key is
    /// meant for immediate effect (canceling operations), so waiting for an empty
    /// input prompt would defeat the purpose.
    pub fn send_key(&self, name: &str, key: &str) -> crate::Result<()> {
        // Validate the target exists and determine the actual tmux target
        let target = if name == "lead" {
            if !tmux::window_exists(&self.session_name, "lead")? {
                return Err(crate::Error::Rpc {
                    code: -32602,
                    message: "Lead session not found".to_string(),
                });
            }
            // Target pane .0 (Claude Code) specifically, not the chat TUI pane (.1)
            "lead.0".to_string()
        } else {
            let coworkers = self.coworkers.read().unwrap();
            if !coworkers.contains_key(name) {
                return Err(crate::Error::Rpc {
                    code: -32602,
                    message: format!("Coworker not found: {}", name),
                });
            }
            name.to_string()
        };

        // Send the key to the tmux window/pane
        tmux::send_keys_raw(&self.session_name, &target, key)
    }

    /// Background worker that processes queued lead nudges.
    ///
    /// For each nudge, waits until the lead's input prompt is empty (or 90s
    /// timeout expires), then delivers the nudge via tmux send-keys.
    fn lead_nudge_worker(session: &str, rx: mpsc::Receiver<String>) {
        let target = format!("{}:lead.0", session);

        for message in rx {
            // Wait for the lead's input to be empty before sending
            let cleared = tmux::wait_for_empty_input(&target, LEAD_INPUT_WAIT_TIMEOUT);
            if !cleared {
                tracing::info!(
                    "Lead input still has text after {}s timeout, nudging anyway",
                    LEAD_INPUT_WAIT_TIMEOUT.as_secs()
                );
            }

            // Send keys to the lead's Claude Code pane (pane .0), NOT the chat pane (.1).
            if let Err(e) = tmux::send_keys(session, "lead.0", &message) {
                tracing::error!("Failed to deliver lead nudge: {}", e);
            }
        }

        tracing::debug!("Lead nudge worker shutting down (channel closed)");
    }

    /// Background worker that processes queued coworker nudges.
    ///
    /// For each nudge, waits until the coworker's input is stable (not actively
    /// being typed into) before delivering. This respects user input and prevents
    /// corrupting text being typed.
    ///
    /// The waiting logic:
    /// 1. If input is empty → send immediately
    /// 2. If input contains last nudge text → safe to overwrite
    /// 3. If user content detected → wait until unchanged for 20s (COWORKER_INPUT_STABLE_DURATION)
    /// 4. After 120s max wait → send anyway
    fn coworker_nudge_worker(
        session: &str,
        rx: mpsc::Receiver<CoworkerNudge>,
        last_nudge_map: Arc<RwLock<HashMap<String, String>>>,
    ) {
        for nudge in rx {
            let target = format!("{}:{}", session, nudge.name);

            // Get the last nudge text for this coworker (if any)
            let last_nudge_text = {
                let map = last_nudge_map.read().unwrap();
                map.get(&nudge.name).cloned()
            };

            // Wait for a safe opportunity to nudge
            let safe = tmux::wait_for_nudge_safe(
                &target,
                last_nudge_text.as_deref(),
                COWORKER_INPUT_STABLE_DURATION,
                COWORKER_INPUT_MAX_WAIT,
            );

            if !safe {
                tracing::info!(
                    "Coworker {} input still active after {}s, nudging anyway",
                    nudge.name,
                    COWORKER_INPUT_MAX_WAIT.as_secs()
                );
            }

            // Deliver the nudge
            if let Err(e) = tmux::send_keys(session, &nudge.name, &nudge.message) {
                tracing::error!("Failed to deliver coworker nudge to {}: {}", nudge.name, e);
            } else {
                // Record this as the last nudge for this coworker
                let mut map = last_nudge_map.write().unwrap();
                map.insert(nudge.name.clone(), nudge.message);
            }
        }

        tracing::debug!("Coworker nudge worker shutting down (channel closed)");
    }

    /// Send a bell notification to the human's terminal.
    ///
    /// This is triggered when someone posts an @user mention in the channel.
    /// The bell is sent to the lead's chat pane (.1) since that's the pane
    /// the human interacts with directly.
    ///
    /// NOTE: We only send to the chat pane (.1), NOT the Claude Code pane (.0).
    /// The bell character (\x07 / ASCII 7) is also Ctrl+G, which Claude Code
    /// interprets as "open editor" shortcut. Sending it to .0 would trigger
    /// unwanted editor popups instead of a notification.
    pub fn notify_user(&self) -> crate::Result<()> {
        // Check if lead window exists (the human operates through the lead session)
        if !tmux::window_exists(&self.session_name, "lead")? {
            return Err(crate::Error::Rpc {
                code: -32602,
                message: "Lead session not found".to_string(),
            });
        }

        // Send bell only to the chat pane (.1) - NOT to Claude Code pane (.0)
        // because \x07 triggers Ctrl+G (open editor) in Claude Code.
        tmux::send_bell(&self.session_name, "lead.1")
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
    pub fn prepare_spawn(
        &self,
        config: &crate::launch::LaunchConfig,
    ) -> crate::Result<(String, crate::launch::LaunchConfig)> {
        let name = &config.name;

        // Check if already running
        {
            let coworkers = self.coworkers.read().unwrap();
            if coworkers.contains_key(name.as_str()) {
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
    /// Returns an error if the name was taken by a concurrent spawn.
    pub fn register(
        &self,
        name: &str,
        working_dir: String,
        session_id: Option<String>,
        isolated_tasks: bool,
    ) -> crate::Result<()> {
        let mut coworkers = self.coworkers.write().unwrap();

        if coworkers.contains_key(name) {
            return Err(crate::Error::Rpc {
                code: -32603,
                message: format!(
                    "Coworker {} was registered by another concurrent request",
                    name
                ),
            });
        }

        let coworker = Coworker {
            name: name.to_string(),
            status: CoworkerStatus::Running,
            working_dir,
            started_at: Utc::now(),
            current_task: None,
            session_id,
            isolated_tasks,
        };
        coworkers.insert(name.to_string(), coworker);

        tracing::info!("Registered coworker {}", name);
        Ok(())
    }

    /// Spawn a coworker with a specific name using a `ClaudeLaunchConfig`.
    ///
    /// This is the **tmux-based legacy path** — used only for the Lead session
    /// and during the migration period. For headless coworkers, use
    /// `prepare_spawn()` + `SessionManager::spawn()` + `register()` instead.
    ///
    /// Creates a new worktree if one doesn't exist, or reuses an existing one.
    /// The `additional_dirs` field in the config is augmented with worktree paths
    /// created for multi-repo projects.
    ///
    /// Returns the coworker name on success.
    pub fn spawn_with_name(&self, config: &tmux::ClaudeLaunchConfig) -> crate::Result<String> {
        let name = &config.name;

        let (working_dir, launch_config) = self.prepare_spawn(config)?;

        let session_id = tmux::spawn_claude(&self.session_name, &working_dir, &launch_config)?;

        let isolated_tasks = matches!(config.task_mode, crate::launch::TaskMode::Isolated);

        // Register with TOCTTOU race check
        if let Err(e) = self.register(name, working_dir, Some(session_id), isolated_tasks) {
            // Race condition: another spawn beat us to it. Clean up the tmux
            // window we just created and return an error.
            tracing::warn!(
                "Spawn race detected for {}: name was taken during slow work, killing orphaned window",
                name
            );
            if let Err(kill_err) = tmux::kill_all_windows_by_name(&self.session_name, name) {
                tracing::error!(
                    "Failed to kill orphaned window(s) for {}: {}",
                    name,
                    kill_err
                );
            }
            return Err(e);
        }

        tracing::info!(
            "Spawned coworker {} (isolated={}, session_mode={:?})",
            name,
            isolated_tasks,
            config.session_mode,
        );

        Ok(name.to_string())
    }

    /// Sync state with actual tmux windows (bidirectional).
    ///
    /// 1. Removes coworkers whose tmux windows no longer exist.
    /// 2. Adds coworkers whose tmux windows exist but aren't tracked.
    ///
    /// The second case handles coworkers that were missed during startup discovery
    /// (e.g., due to timing issues or transient tmux failures). Without this,
    /// orphan cleanup would incorrectly delete worktrees for running coworkers.
    pub fn sync_with_tmux(
        &self,
        headless_names: &std::collections::HashSet<String>,
    ) -> crate::Result<()> {
        let active_windows = tmux::list_windows(&self.session_name)?;

        // Get all known coworker names for validation
        let all_names: std::collections::HashSet<&str> = AVENUE_NAMES
            .iter()
            .chain(OVERFLOW_NAMES.iter())
            .copied()
            .collect();

        let mut coworkers = self.coworkers.write().unwrap();

        // Remove coworkers whose windows are gone, but preserve headless sessions
        // (they don't have tmux windows but are still alive)
        coworkers.retain(|name, _| active_windows.contains(name) || headless_names.contains(name));

        // Add coworkers whose windows exist but aren't tracked.
        // This prevents orphan cleanup from deleting worktrees for coworkers
        // that were missed during startup discovery.
        for window_name in &active_windows {
            // Only track windows that are valid coworker names
            if !all_names.contains(window_name.as_str()) {
                continue;
            }

            // Skip if already tracked
            if coworkers.contains_key(window_name) {
                continue;
            }

            // Verify the worktree actually exists and is valid before tracking.
            // If a tmux window exists but its worktree was deleted, we shouldn't
            // create an entry with an invalid working_dir path.
            let worktree_path = self.worktree_manager.worktree_path(window_name);
            if !is_valid_git_worktree(&worktree_path) {
                tracing::warn!(
                    "Tmux window {} exists but worktree is missing or invalid - not tracking",
                    window_name
                );
                continue;
            }

            let working_dir = worktree_path.to_string_lossy().to_string();

            let coworker = Coworker::recovered(window_name.clone(), working_dir);
            coworkers.insert(window_name.clone(), coworker);
            tracing::info!(
                "Recovered undiscovered coworker from tmux: {} (preventing worktree deletion)",
                window_name
            );
        }

        // Recover headless coworkers that are alive in SessionManager but missing
        // from the CoworkerManager tracking map. This handles the race condition
        // where a session is spawned (added to SessionManager) but registration
        // in CoworkerManager hasn't completed yet when sync_with_tmux runs.
        // Without this, the daemon loses track of running headless sessions.
        for headless_name in headless_names {
            // Only recover valid coworker names
            if !all_names.contains(headless_name.as_str()) {
                continue;
            }

            // Skip if already tracked
            if coworkers.contains_key(headless_name) {
                continue;
            }

            // Verify the worktree actually exists and is valid before tracking.
            // Same validation as the tmux recovery path — if a headless session
            // survives with a corrupted/missing worktree, we shouldn't create
            // an entry with an invalid working_dir path.
            let worktree_path = self.worktree_manager.worktree_path(headless_name);
            if !is_valid_git_worktree(&worktree_path) {
                tracing::warn!(
                    "Headless session {} exists but worktree is missing or invalid - not tracking",
                    headless_name
                );
                continue;
            }
            let working_dir = worktree_path.to_string_lossy().to_string();

            let coworker = Coworker::recovered(headless_name.clone(), working_dir);
            coworkers.insert(headless_name.clone(), coworker);
            tracing::info!(
                "Recovered undiscovered headless coworker from SessionManager: {}",
                headless_name
            );
        }

        Ok(())
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
    /// Returns true if inserted, false if name was already taken (matching
    /// the fixed spawn_with_name behavior that checks before insert).
    #[doc(hidden)]
    pub fn insert_for_testing(&self, coworker: Coworker) -> bool {
        let mut coworkers = self.coworkers.write().unwrap();
        if coworkers.contains_key(&coworker.name) {
            return false;
        }
        coworkers.insert(coworker.name.clone(), coworker);
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
                    isolated_tasks: false,
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
                        isolated_tasks: false,
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
                        isolated_tasks: false,
                    },
                );
            }
        }

        // Should return None when all names exhausted
        assert_eq!(manager.next_available_name(), None);
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
                    isolated_tasks: false,
                },
            );
        }

        // spawn_with_name should fail if coworker is already running
        let config = crate::launch::LaunchConfig::coworker(
            "lexington",
            "test-repo",
            crate::launch::SessionMode::Fresh,
            None,
        );
        let result = manager.spawn_with_name(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));

        // Also test with resume=true
        let resume_config = crate::launch::LaunchConfig::coworker(
            "lexington",
            "test-repo",
            crate::launch::SessionMode::Resume,
            None,
        );
        let result = manager.spawn_with_name(&resume_config);
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

        let manager =
            CoworkerManager::with_additional_repos("midtown-test", primary_wt, vec![extra_wt]);

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

        let manager =
            CoworkerManager::with_additional_repos("midtown-test", primary_wt, vec![extra_wt]);

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

        let manager =
            CoworkerManager::with_additional_repos("midtown-test", primary_wt, vec![extra_wt]);

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

        let manager =
            CoworkerManager::with_additional_repos("midtown-test", primary_wt, vec![extra_wt]);

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
            coworkers.insert(
                "lexington".to_string(),
                Coworker {
                    name: "lexington".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    isolated_tasks: false,
                },
            );
            coworkers.insert(
                "park".to_string(),
                Coworker {
                    name: "park".to_string(),
                    status: CoworkerStatus::Stopping,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    isolated_tasks: false,
                },
            );
            coworkers.insert(
                "madison".to_string(),
                Coworker {
                    name: "madison".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    isolated_tasks: false,
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

    #[test]
    fn test_shutdown_succeeds_when_window_missing() {
        let (manager, _temp_dir) = test_manager();

        // Insert a coworker into the HashMap
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
                    isolated_tasks: false,
                },
            );
        }

        assert_eq!(manager.count(), 1);

        // shutdown() should succeed even when tmux window doesn't exist
        // (idempotent behavior - the window is already "gone")
        let result = manager.shutdown("lexington");
        assert!(
            result.is_ok(),
            "shutdown should succeed when window is already gone"
        );

        // The coworker should be removed from tracking
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_shutdown_coworker_not_tracked() {
        let (manager, _temp_dir) = test_manager();

        // Try to shutdown a coworker that was never tracked
        let result = manager.shutdown("nonexistent");

        // Should return an error since the coworker isn't tracked
        assert!(
            result.is_err(),
            "shutdown should error for untracked coworker"
        );
    }

    #[test]
    fn test_sync_with_tmux_preserves_headless_coworkers() {
        let (manager, _temp_dir) = test_manager();

        // Register a headless coworker (no tmux window)
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            coworkers.insert(
                "madison".to_string(),
                Coworker {
                    name: "madison".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    isolated_tasks: false,
                },
            );
        }

        assert_eq!(manager.count(), 1);

        // sync_with_tmux should preserve madison because it's in headless_names,
        // even though it has no tmux window
        let headless_names: std::collections::HashSet<String> =
            ["madison".to_string()].into_iter().collect();
        let result = manager.sync_with_tmux(&headless_names);
        assert!(result.is_ok());

        // madison should still be tracked
        assert_eq!(manager.count(), 1);
        let coworkers = manager.coworkers.read().unwrap();
        assert!(coworkers.contains_key("madison"));
    }

    #[test]
    fn test_sync_with_tmux_recovers_missing_headless_coworkers() {
        let (manager, temp_dir) = test_manager();

        // Create a valid worktree for madison so recovery can proceed.
        // Without a valid worktree, the validation check will skip it.
        let worktree_path = manager.worktree_manager.worktree_path("madison");
        Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &worktree_path.to_string_lossy(),
            ])
            .current_dir(temp_dir.path())
            .output()
            .expect("Failed to create worktree for madison");

        // Do NOT register any coworker in the map.
        // This simulates the race condition: SessionManager has the session
        // (so it appears in headless_names), but CoworkerManager doesn't
        // have an entry yet (registration hasn't completed).
        assert_eq!(manager.count(), 0);

        // sync_with_tmux should recover madison by adding it to the map
        // because it's in headless_names (alive in SessionManager)
        let headless_names: std::collections::HashSet<String> =
            ["madison".to_string()].into_iter().collect();
        let result = manager.sync_with_tmux(&headless_names);
        assert!(result.is_ok());

        // madison should now be tracked (recovered from headless_names)
        assert_eq!(
            manager.count(),
            1,
            "sync_with_tmux should recover headless coworkers missing from the tracking map"
        );
        let coworkers = manager.coworkers.read().unwrap();
        assert!(
            coworkers.contains_key("madison"),
            "madison should be in the coworkers map after recovery"
        );
        let madison = coworkers.get("madison").unwrap();
        assert_eq!(madison.status, CoworkerStatus::Running);
    }

    #[test]
    fn test_sync_with_tmux_skips_headless_recovery_with_invalid_worktree() {
        let (manager, _temp_dir) = test_manager();

        // Do NOT create a worktree for "madison".
        // The worktree_path will point to a non-existent directory.
        assert_eq!(manager.count(), 0);

        // sync_with_tmux should NOT recover madison because the worktree
        // is missing/invalid — same validation the tmux path performs.
        let headless_names: std::collections::HashSet<String> =
            ["madison".to_string()].into_iter().collect();
        let result = manager.sync_with_tmux(&headless_names);
        assert!(result.is_ok());

        // madison should NOT be tracked (invalid worktree)
        assert_eq!(
            manager.count(),
            0,
            "sync_with_tmux should not recover headless coworkers with invalid worktrees"
        );
    }

    #[test]
    fn test_sync_with_tmux_removes_coworker_not_in_headless_or_tmux() {
        let (manager, _temp_dir) = test_manager();

        // Register a coworker that has no tmux window AND is not in headless_names
        {
            let mut coworkers = manager.coworkers.write().unwrap();
            coworkers.insert(
                "park".to_string(),
                Coworker {
                    name: "park".to_string(),
                    status: CoworkerStatus::Running,
                    working_dir: "/tmp".to_string(),
                    started_at: Utc::now(),
                    current_task: None,
                    session_id: None,
                    isolated_tasks: false,
                },
            );
        }

        assert_eq!(manager.count(), 1);

        // sync_with_tmux with empty headless_names should remove park
        let headless_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let result = manager.sync_with_tmux(&headless_names);
        assert!(result.is_ok());

        // park should be removed (not in tmux windows, not in headless_names)
        assert_eq!(manager.count(), 0);
    }
}
