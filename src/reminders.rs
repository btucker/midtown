//! Reminder system for the midtown daemon.
//!
//! Supports one-shot reminders that fire when a trigger condition is met.
//! Currently supports the `AllWorkMerged` trigger, which fires when there are
//! no pending/in_progress tasks and no coworkers with open PRs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use tracing::{debug, warn};

/// Conditions that can trigger a reminder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReminderTrigger {
    /// Fire when all tasks are completed and all coworker PRs are merged.
    AllWorkMerged,
}

impl std::fmt::Display for ReminderTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReminderTrigger::AllWorkMerged => write!(f, "all-work-merged"),
        }
    }
}

/// A one-shot reminder that fires when its trigger condition is met.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    /// Unique identifier (short hex string)
    pub id: String,
    /// What condition triggers this reminder
    pub trigger: ReminderTrigger,
    /// Message to display when the reminder fires
    pub message: String,
    /// When the reminder was created
    pub created_at: DateTime<Utc>,
    /// Whether the reminder has already fired
    pub fired: bool,
}

/// Persistent state for reminders.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReminderState {
    #[serde(default)]
    pub reminders: Vec<Reminder>,
}

impl ReminderState {
    /// Load state from a file, returning default if file doesn't exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse reminders.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                debug!("Loaded {} reminders", state.reminders.len());
                Ok(state)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!("reminders.json not found, using defaults");
                Ok(Self::default())
            }
            Err(e) => Err(e),
        }
    }

    /// Save state to a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(path, contents)?;
        debug!("Saved {} reminders", self.reminders.len());
        Ok(())
    }

    /// Add a new reminder and return its ID.
    pub fn add(&mut self, trigger: ReminderTrigger, message: String) -> String {
        let id = generate_short_id();
        self.reminders.push(Reminder {
            id: id.clone(),
            trigger,
            message,
            created_at: Utc::now(),
            fired: false,
        });
        id
    }

    /// Cancel a reminder by ID. Returns true if found and removed.
    pub fn cancel(&mut self, id: &str) -> bool {
        let before = self.reminders.len();
        self.reminders.retain(|r| r.id != id);
        self.reminders.len() < before
    }

    /// Get all active (unfired) reminders.
    pub fn active(&self) -> Vec<&Reminder> {
        self.reminders.iter().filter(|r| !r.fired).collect()
    }
}

/// Evaluate whether a trigger condition is met.
///
/// For `AllWorkMerged`: checks that there are no pending/in_progress tasks
/// AND no coworkers with open PRs.
pub fn evaluate_trigger(trigger: &ReminderTrigger, open_pr_coworkers: &[String]) -> bool {
    match trigger {
        ReminderTrigger::AllWorkMerged => {
            let pending = crate::tasks::get_pending_tasks();
            let in_progress = crate::tasks::get_in_progress_tasks();
            let has_work = !pending.is_empty() || !in_progress.is_empty();
            let has_prs = !open_pr_coworkers.is_empty();
            !has_work && !has_prs
        }
    }
}

/// Generate a short hex ID for reminders.
fn generate_short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", nanos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_state_default() {
        let state = ReminderState::default();
        assert!(state.reminders.is_empty());
        assert!(state.active().is_empty());
    }

    #[test]
    fn test_add_reminder() {
        let mut state = ReminderState::default();
        let id = state.add(ReminderTrigger::AllWorkMerged, "Cut release".to_string());
        assert!(!id.is_empty());
        assert_eq!(state.reminders.len(), 1);
        assert_eq!(state.active().len(), 1);
        assert_eq!(state.reminders[0].message, "Cut release");
        assert_eq!(state.reminders[0].trigger, ReminderTrigger::AllWorkMerged);
        assert!(!state.reminders[0].fired);
    }

    #[test]
    fn test_cancel_reminder() {
        let mut state = ReminderState::default();
        let id = state.add(ReminderTrigger::AllWorkMerged, "Test".to_string());
        assert_eq!(state.reminders.len(), 1);

        assert!(state.cancel(&id));
        assert!(state.reminders.is_empty());

        // Cancel non-existent ID returns false
        assert!(!state.cancel("nonexistent"));
    }

    #[test]
    fn test_active_excludes_fired() {
        let mut state = ReminderState::default();
        state.add(ReminderTrigger::AllWorkMerged, "Active".to_string());
        state.add(ReminderTrigger::AllWorkMerged, "Fired".to_string());
        state.reminders[1].fired = true;

        let active = state.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].message, "Active");
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reminders.json");

        let mut state = ReminderState::default();
        state.add(ReminderTrigger::AllWorkMerged, "Release v1".to_string());
        state.add(ReminderTrigger::AllWorkMerged, "Deploy".to_string());
        state.reminders[1].fired = true;

        state.save(&path).unwrap();

        let loaded = ReminderState::load(&path).unwrap();
        assert_eq!(loaded.reminders.len(), 2);
        assert_eq!(loaded.reminders[0].message, "Release v1");
        assert!(!loaded.reminders[0].fired);
        assert_eq!(loaded.reminders[1].message, "Deploy");
        assert!(loaded.reminders[1].fired);
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");

        let state = ReminderState::load(&path).unwrap();
        assert!(state.reminders.is_empty());
    }

    #[test]
    fn test_trigger_display() {
        assert_eq!(
            format!("{}", ReminderTrigger::AllWorkMerged),
            "all-work-merged"
        );
    }

    #[test]
    fn test_evaluate_trigger_all_work_merged_with_prs() {
        // If there are open PRs, trigger should NOT fire
        let coworkers = vec!["park".to_string()];
        let result = evaluate_trigger(&ReminderTrigger::AllWorkMerged, &coworkers);
        assert!(!result, "Should not fire when coworkers have open PRs");
    }
}
