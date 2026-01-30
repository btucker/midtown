//! Rules engine types for the daemon tick loop.
//!
//! This module defines the core abstractions for a rules-based daemon:
//! [`Rule`], [`Condition`], [`Action`], [`RuleContext`], and [`CooldownTracker`].
//!
//! **Phase 2 — types only, no behavior changes.**

// These types are defined now but wired up in later phases (3–6).
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::daemon::DaemonState;

// ---------------------------------------------------------------------------
// Rule
// ---------------------------------------------------------------------------

/// A named rule that pairs a condition with an action and optional cooldown.
pub(crate) struct Rule {
    /// Human-readable identifier, e.g. `"idle_shutdown"`.
    pub name: &'static str,
    /// Logical grouping used for ordering and diagnostics.
    pub category: RuleCategory,
    /// Returns `true` when the rule should fire.
    pub condition: Box<dyn Condition>,
    /// Side-effect to execute when the condition is met.
    pub action: Box<dyn Action>,
    /// Optional cooldown to avoid firing too frequently.
    pub cooldown: Option<CooldownConfig>,
}

// ---------------------------------------------------------------------------
// Condition / Action traits
// ---------------------------------------------------------------------------

/// Evaluated once per tick to decide whether a [`Rule`] should fire.
pub(crate) trait Condition: Send + Sync {
    fn evaluate(&self, ctx: &RuleContext) -> bool;
}

/// Executed when a [`Rule`]'s condition is met.
pub(crate) trait Action: Send + Sync {
    fn execute(&self, ctx: &mut RuleContext) -> ActionResult;
}

/// Outcome of an [`Action::execute`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionResult {
    /// The action completed successfully.
    Done(String),
    /// The action determined it had nothing to do.
    Skipped(String),
    /// The action encountered an error.
    Failed(String),
}

// ---------------------------------------------------------------------------
// RuleCategory
// ---------------------------------------------------------------------------

/// Logical grouping for rules — used for ordering and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RuleCategory {
    Lifecycle,
    PrReview,
    TaskAssignment,
    Spawning,
    Detection,
    RateLimiting,
}

// ---------------------------------------------------------------------------
// RuleContext
// ---------------------------------------------------------------------------

/// Snapshot of daemon state built once per tick so rules don't re-query.
pub(crate) struct RuleContext {
    pub active_coworkers: Vec<CoworkerSnapshot>,
    pub busy_coworkers: HashSet<String>,
    pub coworkers_with_open_prs: HashSet<String>,
    pub active_reviewers: HashSet<String>,
    pub pane_contents: HashMap<String, String>,
    pub state: Arc<DaemonState>,
}

/// Lightweight snapshot of a coworker at a point in time.
#[derive(Debug, Clone)]
pub(crate) struct CoworkerSnapshot {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub isolated_tasks: bool,
}

// ---------------------------------------------------------------------------
// Cooldown types
// ---------------------------------------------------------------------------

/// Configuration for a rule's cooldown behaviour.
#[derive(Debug, Clone)]
pub(crate) struct CooldownConfig {
    /// Minimum time between firings for the same key.
    pub duration: Duration,
    /// Produces the cache key from the rule name and current context.
    /// If `None`, the rule name alone is used as the key.
    pub key_fn: Option<fn(&str, &RuleContext) -> String>,
}

/// Unified cooldown tracker that replaces six separate mechanisms in DaemonState.
///
/// Keys are `(rule_name, context_key)` pairs mapped to the [`Instant`] they
/// were last recorded. Call [`check`](CooldownTracker::check) before firing
/// and [`record`](CooldownTracker::record) after a successful fire.
#[derive(Debug, Default)]
pub(crate) struct CooldownTracker {
    entries: HashMap<(String, String), Instant>,
}

impl CooldownTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the cooldown has expired (or was never recorded).
    pub fn check(&self, rule_name: &str, key: &str, duration: Duration) -> bool {
        match self.entries.get(&(rule_name.to_owned(), key.to_owned())) {
            None => true,
            Some(last) => last.elapsed() >= duration,
        }
    }

    /// Records the current instant for the given rule/key pair.
    pub fn record(&mut self, rule_name: &str, key: &str) {
        self.entries
            .insert((rule_name.to_owned(), key.to_owned()), Instant::now());
    }

    /// Removes entries whose cooldown has long expired (2× duration),
    /// preventing unbounded growth.
    pub fn cleanup(&mut self, max_age: Duration) {
        self.entries.retain(|_k, v| v.elapsed() < max_age);
    }

    /// Number of tracked entries (useful for tests / diagnostics).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tracker has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn check_returns_true_when_never_recorded() {
        let tracker = CooldownTracker::new();
        assert!(tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn check_returns_false_within_cooldown() {
        let mut tracker = CooldownTracker::new();
        tracker.record("idle_shutdown", "coworker:york");
        assert!(!tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn check_returns_true_after_cooldown_expires() {
        let mut tracker = CooldownTracker::new();
        // Manually insert an expired entry.
        tracker.entries.insert(
            ("idle_shutdown".to_owned(), "coworker:york".to_owned()),
            Instant::now() - Duration::from_secs(120),
        );
        assert!(tracker.check("idle_shutdown", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn record_overwrites_previous_entry() {
        let mut tracker = CooldownTracker::new();
        // Insert an old entry.
        tracker.entries.insert(
            ("orphan".to_owned(), "global".to_owned()),
            Instant::now() - Duration::from_secs(300),
        );
        assert!(tracker.check("orphan", "global", Duration::from_secs(60)));

        // Record fresh — should now be in cooldown.
        tracker.record("orphan", "global");
        assert!(!tracker.check("orphan", "global", Duration::from_secs(60)));
    }

    #[test]
    fn different_keys_are_independent() {
        let mut tracker = CooldownTracker::new();
        tracker.record("idle_shutdown", "coworker:york");
        // Same rule, different key — should be clear.
        assert!(tracker.check(
            "idle_shutdown",
            "coworker:broadway",
            Duration::from_secs(60)
        ));
        // Different rule, same key — should be clear.
        assert!(tracker.check("prompt_nudge", "coworker:york", Duration::from_secs(60)));
    }

    #[test]
    fn cleanup_removes_expired_entries() {
        let mut tracker = CooldownTracker::new();
        // Old entry.
        tracker.entries.insert(
            ("old_rule".to_owned(), "key1".to_owned()),
            Instant::now() - Duration::from_secs(600),
        );
        // Fresh entry.
        tracker.record("fresh_rule", "key2");

        assert_eq!(tracker.len(), 2);
        tracker.cleanup(Duration::from_secs(300));
        assert_eq!(tracker.len(), 1);
        assert!(tracker.check("old_rule", "key1", Duration::from_secs(1)));
        assert!(!tracker.check("fresh_rule", "key2", Duration::from_secs(60)));
    }

    #[test]
    fn cleanup_keeps_recent_entries() {
        let mut tracker = CooldownTracker::new();
        tracker.record("rule_a", "k1");
        tracker.record("rule_b", "k2");
        tracker.cleanup(Duration::from_secs(300));
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn len_and_is_empty() {
        let mut tracker = CooldownTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);

        tracker.record("r", "k");
        assert!(!tracker.is_empty());
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn check_respects_short_durations() {
        let mut tracker = CooldownTracker::new();
        tracker.record("fast", "k");

        // Should be in cooldown right after recording (10ms window).
        assert!(!tracker.check("fast", "k", Duration::from_millis(10)));

        // Sleep past the cooldown.
        thread::sleep(Duration::from_millis(15));
        assert!(tracker.check("fast", "k", Duration::from_millis(10)));
    }
}
