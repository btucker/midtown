//! Unified persistent state for the midtown daemon.
//!
//! Consolidates what was previously spread across multiple JSON files
//! (github-state.json, reminders.json) into a single daemon-state.json.
//! Loaded once at startup, saved after any mutation.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ci_stats::CiCheckStats;
use crate::github_state::GitHubState;
use crate::reminders::ReminderState;
use crate::worktree_registry::WorktreeRegistry;

/// Persisted info about a headless Claude Code session.
///
/// Stored in `DaemonPersistentState` to survive daemon restarts. The daemon
/// uses these session IDs to resume coworker sessions after restart, and to
/// support `midtown attach` (which pauses headless execution and resumes it
/// in an interactive terminal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessSessionInfo {
    /// Claude Code session ID (used with `--resume <id>`).
    pub session_id: String,
    /// Last time this session was active (event received or message sent).
    pub last_active: DateTime<Utc>,
    /// Human-readable purpose (e.g., "task !5: Add auth endpoint", "reviewer for PR #42").
    pub purpose: String,
    /// OS process ID for zombie detection and cleanup.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Coworker type: "dev" or "reviewer".
    #[serde(default)]
    pub coworker_type: Option<String>,
    /// Task ID if this is a dev coworker working on a task.
    #[serde(default)]
    pub task_id: Option<u64>,
    /// PR number if this is a reviewer coworker.
    #[serde(default)]
    pub pr_number: Option<u64>,
    /// Working directory (worktree path) for this session.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Provider (Claude or Codex) for this session.
    #[serde(default)]
    pub provider: Option<crate::auth::AuthProvider>,
}

/// All persistent daemon state in one struct.
///
/// Serialized to `~/.midtown/projects/<repo>/daemon-state.json`.
/// Contains GitHub PR state and one-shot reminders. Loaded at startup
/// and saved after every mutation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonPersistentState {
    /// GitHub PR reviewer assignments, review cache, pending spawns.
    #[serde(default)]
    pub github: GitHubState,

    /// One-shot condition-based reminders.
    #[serde(default)]
    pub reminders: ReminderState,

    /// CI check duration statistics for auto-retry of stale checks.
    #[serde(default)]
    pub ci_stats: CiCheckStats,

    /// Task-based worktree registry mapping tasks to worktrees by branch slug.
    /// Enables build cache reuse across coworker reassignment and automatic
    /// cleanup on PR merge.
    #[serde(default)]
    pub worktree_registry: WorktreeRegistry,

    /// Headless session IDs for coworkers, keyed by coworker name.
    /// Persisted so the daemon can resume sessions after restart.
    #[serde(default)]
    pub headless_sessions: HashMap<String, HeadlessSessionInfo>,

    /// Task-to-channel assignment mapping for message routing.
    /// Maps task ID → channel name. Used by the daemon to route coworker messages
    /// to the appropriate topic channel based on the task they're working on.
    /// Persists across daemon restarts so channel routing survives.
    #[serde(default)]
    pub task_channel: HashMap<String, String>,

    /// Task-to-model assignment mapping for coworker spawn.
    /// Maps task ID → model specification (e.g., "claude/opus", "claude/sonnet").
    /// Used by the daemon to launch coworkers with the requested model when spawning
    /// for a task. Stored separately from Claude Code's native task storage for
    /// compatibility. Persists across daemon restarts.
    #[serde(default)]
    pub task_model: HashMap<String, String>,

    /// Clusterer session ID for resume-on-demand.
    /// The clusterer accumulates context about channel assignments across
    /// invocations, so we persist the session ID to resume it on next task creation.
    #[serde(default)]
    pub clusterer_session_id: Option<String>,
}

impl DaemonPersistentState {
    /// Load from the unified state file for a repository.
    ///
    /// If `daemon-state.json` doesn't exist, attempts migration from the
    /// legacy separate files (github-state.json, reminders.json). If those
    /// don't exist either, returns default state.
    pub fn load_for_repo(repo: &str) -> io::Result<Self> {
        let path = crate::paths::daemon_state_file_for_repo(repo);
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let mut state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse daemon-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                // Rebuild reverse indexes that aren't serialized
                state.worktree_registry.rebuild_indexes();
                debug!(
                    "Loaded daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings",
                    state.github.pr_reviewers.len(),
                    state.reminders.reminders.len(),
                    state.ci_stats.summary(),
                    state.worktree_registry.len(),
                    state.task_channel.len(),
                    state.task_model.len()
                );
                Ok(state)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!("daemon-state.json not found, attempting migration from legacy files");
                Self::migrate_from_legacy(repo)
            }
            Err(e) => Err(e),
        }
    }

    /// Save to the unified state file atomically (temp file + rename).
    pub fn save_for_repo(&self, repo: &str) -> io::Result<()> {
        let path = crate::paths::daemon_state_file_for_repo(repo);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &contents)?;
        crate::paths::atomic_rename(&tmp_path, &path)?;
        debug!(
            "Saved daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings",
            self.github.pr_reviewers.len(),
            self.reminders.reminders.len(),
            self.ci_stats.summary(),
            self.worktree_registry.len(),
            self.task_channel.len(),
            self.task_model.len()
        );
        Ok(())
    }

    /// Migrate from legacy separate files into the unified format.
    ///
    /// Loads github-state.json and reminders.json if they exist,
    /// combines them into a single DaemonPersistentState, saves as
    /// daemon-state.json, then removes the old files.
    fn migrate_from_legacy(repo: &str) -> io::Result<Self> {
        let github = crate::github_state::load_state_for_repo(repo).unwrap_or_else(|e| {
            if e.kind() != ErrorKind::NotFound {
                warn!(
                    "Failed to load legacy github-state.json during migration: {}",
                    e
                );
            }
            GitHubState::default()
        });

        let reminder_path = crate::paths::reminders_file_for_repo(repo);
        let reminders = ReminderState::load(&reminder_path).unwrap_or_else(|e| {
            if e.kind() != ErrorKind::NotFound {
                warn!(
                    "Failed to load legacy reminders.json during migration: {}",
                    e
                );
            }
            ReminderState::default()
        });

        let state = Self {
            github,
            reminders,
            ci_stats: CiCheckStats::default(),
            worktree_registry: WorktreeRegistry::default(),
            headless_sessions: HashMap::new(),
            task_channel: HashMap::new(),
            task_model: HashMap::new(),
            clusterer_session_id: None,
        };

        // Save the unified file
        if let Err(e) = state.save_for_repo(repo) {
            warn!("Failed to save migrated daemon-state.json: {}", e);
            return Err(e);
        }

        // Clean up legacy files (best-effort, don't fail if removal fails)
        let github_path = crate::paths::github_state_file_for_repo(repo);
        if github_path.exists() {
            let _ = fs::remove_file(&github_path);
            debug!("Removed legacy github-state.json after migration");
        }
        if reminder_path.exists() {
            let _ = fs::remove_file(&reminder_path);
            debug!("Removed legacy reminders.json after migration");
        }

        Ok(state)
    }

    /// Clear reviewer assignment for a coworker and save state.
    ///
    /// Returns true if an assignment was cleared, false if the coworker had no assignment.
    /// This helper is used by both RPC handlers (coworker.break) and Effect handlers
    /// (ClearOrphanedReviewerAssignments) to avoid duplicating the cleanup logic.
    pub fn clear_reviewer_assignment(&mut self, reviewer_name: &str, repo: &str) -> bool {
        if let Some(assignment) = self.github.remove_assignment_by_reviewer(reviewer_name) {
            tracing::info!(
                "Cleared reviewer assignment for {} (was reviewing PR #{})",
                reviewer_name,
                assignment.pr_number
            );
            if let Err(e) = self.save_for_repo(repo) {
                tracing::warn!(
                    "Failed to save persistent state after clearing reviewer assignment: {}",
                    e
                );
            }
            true
        } else {
            false
        }
    }
}

#[path = "state_tests.rs"]
#[cfg(test)]
mod tests;
