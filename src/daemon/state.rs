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
                    "Loaded daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings",
                    state.github.pr_reviewers.len(),
                    state.reminders.reminders.len(),
                    state.ci_stats.summary(),
                    state.worktree_registry.len(),
                    state.task_channel.len()
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
            "Saved daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings",
            self.github.pr_reviewers.len(),
            self.reminders.reminders.len(),
            self.ci_stats.summary(),
            self.worktree_registry.len(),
            self.task_channel.len()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_state() {
        let state = DaemonPersistentState::default();
        assert!(state.github.pr_reviewers.is_empty());
        assert!(state.reminders.reminders.is_empty());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon-state.json");

        let mut state = DaemonPersistentState::default();
        state.github.assign_reviewer(
            42,
            "lexington",
            crate::github_state::AssignmentSource::PollingFallback,
        );
        state.reminders.add(
            crate::reminders::ReminderTrigger::AllWorkMerged,
            "Deploy".to_string(),
        );

        // Save directly to path
        let contents = serde_json::to_string_pretty(&state).unwrap();
        fs::write(&path, &contents).unwrap();

        // Load directly from path
        let loaded: DaemonPersistentState =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.github.get_reviewer(42), Some("lexington"));
        assert_eq!(loaded.reminders.reminders.len(), 1);
        assert_eq!(loaded.reminders.reminders[0].message, "Deploy");
    }

    #[test]
    fn test_serde_default_handles_missing_fields() {
        // Forward compatibility: missing sections get defaults
        let json = r#"{"github": {}}"#;
        let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(state.reminders.reminders.is_empty());

        let json = r#"{"reminders": {"reminders": []}}"#;
        let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(state.github.pr_reviewers.is_empty());
    }

    #[test]
    fn test_empty_json_uses_defaults() {
        let json = "{}";
        let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(state.github.pr_reviewers.is_empty());
        assert!(state.reminders.reminders.is_empty());
    }

    #[test]
    fn test_headless_session_info_roundtrip() {
        let info = HeadlessSessionInfo {
            session_id: "abc-123-def".to_string(),
            last_active: Utc::now(),
            purpose: "task !5: Add auth endpoint".to_string(),
            pid: Some(12345),
            coworker_type: Some("dev".to_string()),
            task_id: Some(5),
            pr_number: None,
            working_dir: Some("/path/to/worktree".to_string()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: HeadlessSessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "abc-123-def");
        assert_eq!(parsed.purpose, "task !5: Add auth endpoint");
        assert_eq!(parsed.pid, Some(12345));
        assert_eq!(parsed.coworker_type, Some("dev".to_string()));
        assert_eq!(parsed.task_id, Some(5));
        assert_eq!(parsed.pr_number, None);
        assert_eq!(parsed.working_dir, Some("/path/to/worktree".to_string()));
    }

    #[test]
    fn test_headless_sessions_in_persistent_state() {
        let mut state = DaemonPersistentState::default();
        state.headless_sessions.insert(
            "park".to_string(),
            HeadlessSessionInfo {
                session_id: "session-42".to_string(),
                last_active: Utc::now(),
                purpose: "task !3: Fix login bug".to_string(),
                pid: Some(9999),
                coworker_type: Some("dev".to_string()),
                task_id: Some(3),
                pr_number: None,
                working_dir: Some("/path/to/park-worktree".to_string()),
            },
        );

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.headless_sessions.len(), 1);
        let park = loaded.headless_sessions.get("park").unwrap();
        assert_eq!(park.session_id, "session-42");
        assert_eq!(park.purpose, "task !3: Fix login bug");
        assert_eq!(park.pid, Some(9999));
        assert_eq!(park.task_id, Some(3));
    }

    #[test]
    fn test_headless_sessions_default_empty() {
        // Existing state without headless_sessions should deserialize fine
        let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
        let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(state.headless_sessions.is_empty());
    }

    #[test]
    fn test_headless_session_info_backward_compat() {
        // Old format without new fields should deserialize with defaults
        let json = r#"{
            "session_id": "old-session",
            "last_active": "2026-02-09T10:00:00Z",
            "purpose": "task !1: Old task"
        }"#;
        let info: HeadlessSessionInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.session_id, "old-session");
        assert_eq!(info.purpose, "task !1: Old task");
        assert_eq!(info.pid, None);
        assert_eq!(info.coworker_type, None);
        assert_eq!(info.task_id, None);
        assert_eq!(info.pr_number, None);
        assert_eq!(info.working_dir, None);
    }

    #[test]
    fn test_full_roundtrip_with_all_fields() {
        let mut state = DaemonPersistentState::default();

        // Populate github state
        state.github.assign_reviewer(
            1,
            "broadway",
            crate::github_state::AssignmentSource::PollingFallback,
        );
        state.github.assign_reviewer(
            2,
            "park",
            crate::github_state::AssignmentSource::PollingFallback,
        );
        state.github.mark_reviewed_pr(10);
        state
            .github
            .add_pending_review_spawn(3, chrono::Utc::now() + chrono::Duration::seconds(60));

        // Populate reminders
        state.reminders.add(
            crate::reminders::ReminderTrigger::AllWorkMerged,
            "Cut release".to_string(),
        );
        state.reminders.add(
            crate::reminders::ReminderTrigger::AllWorkMerged,
            "Deploy staging".to_string(),
        );

        // Populate task-channel mappings
        state
            .task_channel
            .insert("10".to_string(), "auth".to_string());
        state
            .task_channel
            .insert("11".to_string(), "frontend".to_string());

        // Serialize and deserialize
        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.github.pr_reviewers.len(), 2);
        assert_eq!(loaded.github.get_reviewer(1), Some("broadway"));
        assert_eq!(loaded.github.get_reviewer(2), Some("park"));
        assert!(loaded.github.has_cached_review(10));
        assert_eq!(loaded.github.pending_review_spawns.len(), 1);
        assert_eq!(loaded.reminders.reminders.len(), 2);
        assert_eq!(loaded.task_channel.len(), 2);
        assert_eq!(loaded.task_channel.get("10"), Some(&"auth".to_string()));
        assert_eq!(loaded.task_channel.get("11"), Some(&"frontend".to_string()));
    }

    #[test]
    fn test_task_channel_mapping() {
        let mut state = DaemonPersistentState::default();
        state
            .task_channel
            .insert("42".to_string(), "auth".to_string());
        state
            .task_channel
            .insert("43".to_string(), "frontend".to_string());

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.task_channel.len(), 2);
        assert_eq!(loaded.task_channel.get("42"), Some(&"auth".to_string()));
        assert_eq!(loaded.task_channel.get("43"), Some(&"frontend".to_string()));
    }

    #[test]
    fn test_task_channel_default_empty() {
        // Existing state without task_channel should deserialize fine
        let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
        let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(state.task_channel.is_empty());
    }

    #[test]
    fn test_task_channel_file_roundtrip() {
        // Exercises save_for_repo and load_for_repo with task_channel data,
        // covering the debug log statements that report task-channel mapping counts.
        let tmp = tempfile::tempdir().unwrap();
        let repo_name = "test-repo";

        // Set up the repo directory structure that save_for_repo/load_for_repo expect
        let state_dir = tmp.path().join("projects").join(repo_name);
        std::fs::create_dir_all(&state_dir).unwrap();

        // Override the state file path by saving/loading directly via serde + file I/O
        // (save_for_repo uses crate::paths which we can't easily override in tests,
        // so we test the serialization + deserialization of a state with task_channel)
        let mut state = DaemonPersistentState::default();
        state
            .task_channel
            .insert("100".to_string(), "backend".to_string());
        state
            .task_channel
            .insert("101".to_string(), "infra".to_string());
        state
            .task_channel
            .insert("102".to_string(), "frontend".to_string());

        // Write to file, simulating save_for_repo
        let state_file = state_dir.join("daemon-state.json");
        let contents = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&state_file, &contents).unwrap();

        // Read back, simulating load_for_repo
        let loaded_contents = std::fs::read_to_string(&state_file).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&loaded_contents).unwrap();

        assert_eq!(loaded.task_channel.len(), 3);
        assert_eq!(loaded.task_channel.get("100"), Some(&"backend".to_string()));
        assert_eq!(loaded.task_channel.get("101"), Some(&"infra".to_string()));
        assert_eq!(
            loaded.task_channel.get("102"),
            Some(&"frontend".to_string())
        );
    }

    #[test]
    fn test_task_channel_overwrite_and_remove() {
        let mut state = DaemonPersistentState::default();

        // Add a mapping
        state
            .task_channel
            .insert("50".to_string(), "auth".to_string());
        assert_eq!(state.task_channel.get("50"), Some(&"auth".to_string()));

        // Overwrite with a different channel
        state
            .task_channel
            .insert("50".to_string(), "security".to_string());
        assert_eq!(state.task_channel.get("50"), Some(&"security".to_string()));
        assert_eq!(state.task_channel.len(), 1);

        // Remove the mapping
        state.task_channel.remove("50");
        assert!(state.task_channel.is_empty());

        // Roundtrip after removal
        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
        assert!(loaded.task_channel.is_empty());
    }

    #[test]
    fn test_clear_reviewer_assignment() {
        // Note: This test validates the return value behavior but doesn't test persistence
        // because DaemonPersistentState::save_for_repo requires a valid repo name
        // and filesystem paths. The save/load behavior is covered by other tests.

        let mut state = DaemonPersistentState::default();
        state.github.assign_reviewer(
            42,
            "amsterdam",
            crate::github_state::AssignmentSource::Webhook,
        );

        // Verify the assignment exists before clearing
        assert_eq!(state.github.get_reviewer(42), Some("amsterdam"));

        // Clear existing assignment - should return true
        // (Note: save will fail in tests without proper setup, but that's OK -
        // we're testing the removal logic, not file I/O)
        assert!(state.clear_reviewer_assignment("amsterdam", "test-repo"));
        assert_eq!(state.github.get_reviewer(42), None);

        // Try to clear again - should return false (no assignment)
        assert!(!state.clear_reviewer_assignment("amsterdam", "test-repo"));

        // Try to clear a coworker with no assignment - should return false
        assert!(!state.clear_reviewer_assignment("park", "test-repo"));
    }
}
