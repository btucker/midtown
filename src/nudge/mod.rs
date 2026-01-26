//! Agent nudging system for periodic and event-driven reminders.
//!
//! This module provides nudging capabilities for coworkers:
//! - Periodic heartbeat nudges (configurable interval, default 5 minutes)
//! - Event-driven nudges (new PR review, blocker resolved)
//! - Tracking of last nudge time per coworker
//! - Configurable message templates

mod config;
mod state;
mod tmux;

pub use config::NudgeConfig;
pub use state::{CoworkerNudgeState, NudgeTracker};
pub use tmux::{NudgeError, list_sessions, send_nudge, send_nudge_to_pane};

use std::time::{Duration, SystemTime};

/// Default nudge interval (5 minutes)
pub const DEFAULT_NUDGE_INTERVAL_SECS: u64 = 300;

/// Default nudge message template
pub const DEFAULT_NUDGE_TEMPLATE: &str =
    "Reminder: You are working on {task}. Check channel for updates: midtown channel read";

/// Reason for sending a nudge
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeReason {
    /// Periodic heartbeat nudge
    Heartbeat,
    /// New review comment on coworker's PR
    NewReview { pr_number: u64 },
    /// Blocker was resolved
    BlockerResolved { blocker_id: String },
    /// Manual nudge from another agent or user
    Manual { from: String },
}

impl NudgeReason {
    /// Get a human-readable description of the nudge reason
    pub fn description(&self) -> String {
        match self {
            NudgeReason::Heartbeat => "periodic check-in".to_string(),
            NudgeReason::NewReview { pr_number } => {
                format!("new review on PR #{}", pr_number)
            }
            NudgeReason::BlockerResolved { blocker_id } => {
                format!("blocker {} resolved", blocker_id)
            }
            NudgeReason::Manual { from } => format!("nudge from {}", from),
        }
    }
}

/// A nudge message ready to be sent
#[derive(Debug, Clone)]
pub struct Nudge {
    /// Target coworker name
    pub coworker: String,
    /// Current task the coworker is working on
    pub task: Option<String>,
    /// Reason for the nudge
    pub reason: NudgeReason,
    /// The formatted message to send
    pub message: String,
}

impl Nudge {
    /// Create a new nudge with the default template
    pub fn new(coworker: impl Into<String>, task: Option<String>, reason: NudgeReason) -> Self {
        let coworker = coworker.into();
        let message = format_nudge_message(DEFAULT_NUDGE_TEMPLATE, task.as_deref());

        Self {
            coworker,
            task,
            reason,
            message,
        }
    }

    /// Create a nudge with a custom message template
    pub fn with_template(
        coworker: impl Into<String>,
        task: Option<String>,
        reason: NudgeReason,
        template: &str,
    ) -> Self {
        let coworker = coworker.into();
        let message = format_nudge_message(template, task.as_deref());

        Self {
            coworker,
            task,
            reason,
            message,
        }
    }
}

/// Format a nudge message using the template
fn format_nudge_message(template: &str, task: Option<&str>) -> String {
    let task_str = task.unwrap_or("your current task");
    template.replace("{task}", task_str)
}

/// Service for managing nudges
pub struct NudgeService {
    config: NudgeConfig,
    tracker: NudgeTracker,
}

impl NudgeService {
    /// Create a new nudge service with default configuration
    pub fn new() -> Self {
        Self {
            config: NudgeConfig::default(),
            tracker: NudgeTracker::new(),
        }
    }

    /// Create a nudge service with custom configuration
    pub fn with_config(config: NudgeConfig) -> Self {
        Self {
            config,
            tracker: NudgeTracker::new(),
        }
    }

    /// Check if a coworker should be nudged based on the configured interval
    pub fn should_nudge(&self, coworker: &str) -> bool {
        match self.tracker.get(coworker) {
            Some(state) => {
                let elapsed = SystemTime::now()
                    .duration_since(state.last_nudge)
                    .unwrap_or(Duration::ZERO);
                elapsed >= self.config.interval
            }
            None => true, // Never nudged, should nudge
        }
    }

    /// Send a nudge to a coworker if enough time has passed
    ///
    /// Returns Ok(true) if nudge was sent, Ok(false) if skipped due to interval
    pub fn nudge_if_due(
        &mut self,
        coworker: &str,
        task: Option<&str>,
        tmux_session: &str,
    ) -> std::result::Result<bool, NudgeError> {
        if !self.should_nudge(coworker) {
            return Ok(false);
        }

        let nudge = Nudge::with_template(
            coworker,
            task.map(String::from),
            NudgeReason::Heartbeat,
            &self.config.message_template,
        );

        self.send_nudge(&nudge, tmux_session)?;
        Ok(true)
    }

    /// Send a nudge immediately (for event-driven nudges)
    pub fn send_nudge(
        &mut self,
        nudge: &Nudge,
        tmux_session: &str,
    ) -> std::result::Result<(), NudgeError> {
        send_nudge(tmux_session, &nudge.message)?;
        self.tracker.record_nudge(&nudge.coworker);
        Ok(())
    }

    /// Get the nudge configuration
    pub fn config(&self) -> &NudgeConfig {
        &self.config
    }

    /// Get mutable access to the configuration
    pub fn config_mut(&mut self) -> &mut NudgeConfig {
        &mut self.config
    }

    /// Get the nudge tracker state
    pub fn tracker(&self) -> &NudgeTracker {
        &self.tracker
    }
}

impl Default for NudgeService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_nudge_message_with_task() {
        let msg = format_nudge_message(DEFAULT_NUDGE_TEMPLATE, Some("fixing bug #123"));
        assert!(msg.contains("fixing bug #123"));
        assert!(msg.contains("midtown channel read"));
    }

    #[test]
    fn test_format_nudge_message_without_task() {
        let msg = format_nudge_message(DEFAULT_NUDGE_TEMPLATE, None);
        assert!(msg.contains("your current task"));
    }

    #[test]
    fn test_nudge_reason_description() {
        assert_eq!(NudgeReason::Heartbeat.description(), "periodic check-in");
        assert_eq!(
            NudgeReason::NewReview { pr_number: 42 }.description(),
            "new review on PR #42"
        );
        assert_eq!(
            NudgeReason::BlockerResolved {
                blocker_id: "BUG-1".to_string()
            }
            .description(),
            "blocker BUG-1 resolved"
        );
        assert_eq!(
            NudgeReason::Manual {
                from: "witness".to_string()
            }
            .description(),
            "nudge from witness"
        );
    }

    #[test]
    fn test_nudge_creation() {
        let nudge = Nudge::new(
            "polecat1",
            Some("task-123".to_string()),
            NudgeReason::Heartbeat,
        );
        assert_eq!(nudge.coworker, "polecat1");
        assert_eq!(nudge.task, Some("task-123".to_string()));
        assert!(nudge.message.contains("task-123"));
    }

    #[test]
    fn test_nudge_with_custom_template() {
        let nudge = Nudge::with_template(
            "polecat1",
            Some("task-456".to_string()),
            NudgeReason::Heartbeat,
            "Hey! Working on {task}? Check in!",
        );
        assert_eq!(nudge.message, "Hey! Working on task-456? Check in!");
    }
}
