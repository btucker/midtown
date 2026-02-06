//! Unified persistent state for the midtown daemon.
//!
//! Consolidates what was previously spread across multiple JSON files
//! (github-state.json, reminders.json) into a single daemon-state.json.
//! Loaded once at startup, saved after any mutation.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind};
use tracing::{debug, warn};

use crate::ci_stats::CiCheckStats;
use crate::github_state::GitHubState;
use crate::reminders::ReminderState;

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
                let state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse daemon-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                debug!(
                    "Loaded daemon state: {} PR reviewers, {} reminders, CI stats: {}",
                    state.github.pr_reviewers.len(),
                    state.reminders.reminders.len(),
                    state.ci_stats.summary()
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
            "Saved daemon state: {} PR reviewers, {} reminders, CI stats: {}",
            self.github.pr_reviewers.len(),
            self.reminders.reminders.len(),
            self.ci_stats.summary()
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

        // Serialize and deserialize
        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.github.pr_reviewers.len(), 2);
        assert_eq!(loaded.github.get_reviewer(1), Some("broadway"));
        assert_eq!(loaded.github.get_reviewer(2), Some("park"));
        assert!(loaded.github.has_cached_review(10));
        assert_eq!(loaded.github.pending_review_spawns.len(), 1);
        assert_eq!(loaded.reminders.reminders.len(), 2);
    }
}
