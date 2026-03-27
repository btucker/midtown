//! Reminder system for the midtown daemon.
//!
//! Supports reminders that fire when a trigger condition is met.
//! Currently supports the `AllWorkMerged` trigger, which fires when there are
//! no pending/in_progress tasks and no coworkers with open PRs.
//! Reminders can fire once, a fixed number of times, or indefinitely.

use chrono::{DateTime, Utc};
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use std::str::FromStr;
use tracing::{debug, warn};

use crate::persistence::JsonPersistable;

/// Conditions that can trigger a reminder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReminderTrigger {
    /// Fire when all tasks are completed and all coworker PRs are merged.
    AllWorkMerged,
    /// Fire on a cron schedule (evaluated in UTC).
    CronUtc { cron_expr: String },
}

impl std::fmt::Display for ReminderTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReminderTrigger::AllWorkMerged => write!(f, "all-work-merged"),
            ReminderTrigger::CronUtc { cron_expr } => write!(f, "cron-utc({})", cron_expr),
        }
    }
}

/// How many times a reminder can fire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatPolicy {
    /// Fire once (default, backward compatible with old `fired: bool`)
    #[default]
    Once,
    /// Fire N additional times after the first (N+1 total fires).
    /// E.g., Times(3) fires 4 times total, matching `--repeat 3` CLI semantics.
    Times(u32),
    /// Fire indefinitely
    Indefinite,
}

impl std::fmt::Display for RepeatPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepeatPolicy::Once => write!(f, "once"),
            RepeatPolicy::Times(n) => write!(f, "{}x more", n),
            RepeatPolicy::Indefinite => write!(f, "indefinite"),
        }
    }
}

/// A reminder that fires when its trigger condition is met.
#[derive(Debug, Clone, Serialize)]
pub struct Reminder {
    /// Unique identifier (short hex string)
    pub id: String,
    /// What condition triggers this reminder
    pub trigger: ReminderTrigger,
    /// Message to display when the reminder fires
    pub message: String,
    /// When the reminder was created
    pub created_at: DateTime<Utc>,
    /// How many times this reminder should fire
    #[serde(default)]
    pub repeat_policy: RepeatPolicy,
    /// How many times the reminder has fired so far
    #[serde(default)]
    pub fire_count: u32,
    /// Last time this reminder was evaluated (for cron window-based matching).
    #[serde(default)]
    pub last_evaluated_at: Option<DateTime<Utc>>,
}

/// Intermediary for backward-compatible deserialization.
/// Handles both old format (`fired: bool`) and new format (`repeat_policy` + `fire_count`).
#[derive(Deserialize)]
struct ReminderRaw {
    id: String,
    trigger: ReminderTrigger,
    message: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    repeat_policy: RepeatPolicy,
    #[serde(default)]
    fire_count: u32,
    #[serde(default)]
    fired: Option<bool>,
    #[serde(default)]
    last_evaluated_at: Option<DateTime<Utc>>,
}

impl<'de> Deserialize<'de> for Reminder {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ReminderRaw::deserialize(deserializer)?;
        let fire_count = if raw.fire_count > 0 {
            raw.fire_count
        } else if raw.fired == Some(true) {
            1
        } else {
            0
        };
        Ok(Reminder {
            id: raw.id,
            trigger: raw.trigger,
            message: raw.message,
            created_at: raw.created_at,
            repeat_policy: raw.repeat_policy,
            fire_count,
            last_evaluated_at: raw.last_evaluated_at,
        })
    }
}

impl Reminder {
    /// Whether this reminder can still fire.
    pub fn is_active(&self) -> bool {
        match self.repeat_policy {
            RepeatPolicy::Once => self.fire_count == 0,
            RepeatPolicy::Times(n) => self.fire_count <= n,
            RepeatPolicy::Indefinite => true,
        }
    }

    /// Human-readable description of remaining fires.
    pub fn fires_remaining(&self) -> String {
        match self.repeat_policy {
            RepeatPolicy::Once => {
                if self.fire_count == 0 {
                    "1 fire remaining".to_string()
                } else {
                    "exhausted".to_string()
                }
            }
            RepeatPolicy::Times(n) => {
                let total = n.saturating_add(1);
                let remaining = total.saturating_sub(self.fire_count);
                format!("{}/{} fires remaining", remaining, total)
            }
            RepeatPolicy::Indefinite => format!("\u{221e} (fired {} times)", self.fire_count),
        }
    }
}

/// Persistent state for reminders.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReminderState {
    #[serde(default)]
    pub reminders: Vec<Reminder>,
}

impl JsonPersistable for ReminderState {}

impl ReminderState {
    /// Load state from a file, returning default if file doesn't exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::load_json(path)
            .inspect(|state| debug!("Loaded {} reminders", state.reminders.len()))
            .inspect_err(|e| warn!("Failed to load reminders.json: {}", e))
    }

    /// Save state to a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.save_json(path)?;
        debug!("Saved {} reminders", self.reminders.len());
        Ok(())
    }

    /// Add a new reminder and return its ID.
    pub fn add(
        &mut self,
        trigger: ReminderTrigger,
        message: String,
        repeat_policy: RepeatPolicy,
    ) -> String {
        let id = generate_short_id();
        self.reminders.push(Reminder {
            id: id.clone(),
            trigger,
            message,
            created_at: Utc::now(),
            repeat_policy,
            fire_count: 0,
            last_evaluated_at: None,
        });
        id
    }

    /// Cancel a reminder by ID. Returns true if found and removed.
    pub fn cancel(&mut self, id: &str) -> bool {
        let before = self.reminders.len();
        self.reminders.retain(|r| r.id != id);
        self.reminders.len() < before
    }

    /// Get all active (unfired or still repeating) reminders.
    pub fn active(&self) -> Vec<&Reminder> {
        self.reminders.iter().filter(|r| r.is_active()).collect()
    }
}

/// Evaluate whether a trigger condition is met.
///
/// For `AllWorkMerged`: checks that there are no pending/in_progress tasks
/// AND no coworkers with open PRs.
pub fn evaluate_trigger(trigger: &ReminderTrigger, open_pr_coworkers: &[String]) -> bool {
    match trigger {
        ReminderTrigger::AllWorkMerged => {
            // Check if any non-completed tasks exist using TaskStore via paths
            let repo = crate::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
            let task_store = crate::task_store::TaskStore::new(
                crate::paths::projects_dir_for_repo(&repo).join("tasks"),
            );
            let all_tasks = task_store.load_all();
            let has_work = all_tasks.iter().any(|t| {
                t.status == crate::task_store::TaskStatus::Pending
                    || t.status == crate::task_store::TaskStatus::InProgress
            });
            let has_prs = !open_pr_coworkers.is_empty();
            !has_work && !has_prs
        }
        ReminderTrigger::CronUtc { .. } => {
            // Cron triggers are evaluated via evaluate_cron_trigger with time windows,
            // not through this condition-based path.
            false
        }
    }
}

/// Validate a cron expression string.
pub fn validate_cron_expression(expr: &str) -> Result<(), String> {
    Cron::from_str(expr).map_err(|e| format!("Invalid cron expression: {}", e))?;
    Ok(())
}

/// Evaluate whether a cron trigger should fire in the window (last_eval, now].
pub fn evaluate_cron_trigger(
    trigger: &ReminderTrigger,
    last_evaluated_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    let cron_expr = match trigger {
        ReminderTrigger::CronUtc { cron_expr } => cron_expr,
        _ => return false,
    };
    let cron = match Cron::from_str(cron_expr) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to parse cron expression '{}': {}", cron_expr, e);
            return false;
        }
    };
    // Find the next occurrence after last_evaluated_at. If it falls <= now, fire.
    match cron.find_next_occurrence(&last_evaluated_at, false) {
        Ok(next) => next <= now,
        Err(_) => false,
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
        let id = state.add(
            ReminderTrigger::AllWorkMerged,
            "Cut release".to_string(),
            RepeatPolicy::Once,
        );
        assert!(!id.is_empty());
        assert_eq!(state.reminders.len(), 1);
        assert_eq!(state.active().len(), 1);
        assert_eq!(state.reminders[0].message, "Cut release");
        assert_eq!(state.reminders[0].trigger, ReminderTrigger::AllWorkMerged);
        assert_eq!(state.reminders[0].fire_count, 0);
    }

    #[test]
    fn test_cancel_reminder() {
        let mut state = ReminderState::default();
        let id = state.add(
            ReminderTrigger::AllWorkMerged,
            "Test".to_string(),
            RepeatPolicy::Once,
        );
        assert_eq!(state.reminders.len(), 1);

        assert!(state.cancel(&id));
        assert!(state.reminders.is_empty());

        // Cancel non-existent ID returns false
        assert!(!state.cancel("nonexistent"));
    }

    #[test]
    fn test_active_excludes_fired() {
        let mut state = ReminderState::default();
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Active".to_string(),
            RepeatPolicy::Once,
        );
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Fired".to_string(),
            RepeatPolicy::Once,
        );
        state.reminders[1].fire_count = 1;

        let active = state.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].message, "Active");
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reminders.json");

        let mut state = ReminderState::default();
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Release v1".to_string(),
            RepeatPolicy::Once,
        );
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Deploy".to_string(),
            RepeatPolicy::Once,
        );
        state.reminders[1].fire_count = 1;

        state.save(&path).unwrap();

        let loaded = ReminderState::load(&path).unwrap();
        assert_eq!(loaded.reminders.len(), 2);
        assert_eq!(loaded.reminders[0].message, "Release v1");
        assert_eq!(loaded.reminders[0].fire_count, 0);
        assert_eq!(loaded.reminders[1].message, "Deploy");
        assert_eq!(loaded.reminders[1].fire_count, 1);
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

    #[test]
    fn test_repeat_policy_default_is_once() {
        let policy = RepeatPolicy::default();
        assert_eq!(policy, RepeatPolicy::Once);
    }

    #[test]
    fn test_active_with_repeat_once_after_fire() {
        let mut state = ReminderState::default();
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Test".to_string(),
            RepeatPolicy::Once,
        );
        state.reminders[0].fire_count = 1;
        assert!(
            state.active().is_empty(),
            "Once reminder with fire_count=1 should be inactive"
        );
    }

    #[test]
    fn test_active_with_repeat_times() {
        let mut state = ReminderState::default();
        // Times(3) means 3 additional fires = 4 total
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Test".to_string(),
            RepeatPolicy::Times(3),
        );
        state.reminders[0].fire_count = 3;
        assert_eq!(
            state.active().len(),
            1,
            "Times(3) with fire_count=3 still has 1 fire left"
        );

        state.reminders[0].fire_count = 4;
        assert!(
            state.active().is_empty(),
            "Times(3) with fire_count=4 should be exhausted"
        );
    }

    #[test]
    fn test_active_with_repeat_indefinite() {
        let mut state = ReminderState::default();
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Test".to_string(),
            RepeatPolicy::Indefinite,
        );
        state.reminders[0].fire_count = 100;
        assert_eq!(
            state.active().len(),
            1,
            "Indefinite reminder is always active"
        );
    }

    #[test]
    fn test_backward_compat_fired_true_deserializes() {
        // Old format: { "fired": true } with no repeat_policy/fire_count
        let json = r#"{
            "id": "abc123",
            "trigger": {"type": "AllWorkMerged"},
            "message": "old reminder",
            "created_at": "2026-01-01T00:00:00Z",
            "fired": true
        }"#;
        let reminder: Reminder = serde_json::from_str(json).unwrap();
        assert_eq!(
            reminder.fire_count, 1,
            "fired:true should map to fire_count=1"
        );
        assert_eq!(reminder.repeat_policy, RepeatPolicy::Once);
    }

    #[test]
    fn test_backward_compat_fired_false_deserializes() {
        let json = r#"{
            "id": "abc123",
            "trigger": {"type": "AllWorkMerged"},
            "message": "old reminder",
            "created_at": "2026-01-01T00:00:00Z",
            "fired": false
        }"#;
        let reminder: Reminder = serde_json::from_str(json).unwrap();
        assert_eq!(
            reminder.fire_count, 0,
            "fired:false should map to fire_count=0"
        );
    }

    #[test]
    fn test_new_format_roundtrip() {
        let mut state = ReminderState::default();
        state.add(
            ReminderTrigger::AllWorkMerged,
            "Test".to_string(),
            RepeatPolicy::Times(5),
        );
        state.reminders[0].fire_count = 2;

        let json = serde_json::to_string(&state).unwrap();
        let loaded: ReminderState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.reminders[0].repeat_policy, RepeatPolicy::Times(5));
        assert_eq!(loaded.reminders[0].fire_count, 2);
    }

    #[test]
    fn test_cron_utc_trigger_display() {
        let trigger = ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        };
        assert_eq!(format!("{}", trigger), "cron-utc(0 9 * * MON)");
    }

    #[test]
    fn test_evaluate_cron_trigger_matching_time() {
        use chrono::TimeZone;
        // Monday 2026-03-16 09:00 UTC
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 0).unwrap();
        let last_eval = now - chrono::Duration::seconds(30);
        let trigger = ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        };
        assert!(
            evaluate_cron_trigger(&trigger, last_eval, now),
            "Should fire at 09:00 on Monday"
        );
    }

    #[test]
    fn test_evaluate_cron_trigger_no_match() {
        use chrono::TimeZone;
        // Monday 2026-03-16 09:01 UTC — the cron fires at 09:00, last_eval was 09:00:30
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 9, 1, 0).unwrap();
        let last_eval = now - chrono::Duration::seconds(30);
        let trigger = ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        };
        assert!(
            !evaluate_cron_trigger(&trigger, last_eval, now),
            "Should not fire at 09:01 with last_eval at 09:00:30"
        );
    }

    #[test]
    fn test_evaluate_cron_trigger_fires_within_window() {
        use chrono::TimeZone;
        // last_eval was 08:59:30, now is 09:00:15 — the 09:00 fire is within the window
        let last_eval = Utc.with_ymd_and_hms(2026, 3, 16, 8, 59, 30).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();
        let trigger = ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        };
        assert!(
            evaluate_cron_trigger(&trigger, last_eval, now),
            "Should fire when cron time falls between last_eval and now"
        );
    }

    #[test]
    fn test_evaluate_cron_trigger_no_double_fire() {
        use chrono::TimeZone;
        // last_eval was 09:00:15 (after the fire), now is 09:00:45
        let last_eval = Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 45).unwrap();
        let trigger = ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        };
        assert!(
            !evaluate_cron_trigger(&trigger, last_eval, now),
            "Should not double-fire in same minute"
        );
    }

    #[test]
    fn test_validate_cron_expression_valid() {
        assert!(validate_cron_expression("0 9 * * MON").is_ok());
        assert!(validate_cron_expression("*/5 * * * *").is_ok());
        assert!(validate_cron_expression("0 0 1 1 *").is_ok());
    }

    #[test]
    fn test_validate_cron_expression_invalid() {
        assert!(validate_cron_expression("not a cron").is_err());
        assert!(validate_cron_expression("").is_err());
        assert!(validate_cron_expression("60 * * * *").is_err());
    }
}
