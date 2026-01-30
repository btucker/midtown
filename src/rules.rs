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
// Lifecycle decision types
// ---------------------------------------------------------------------------

/// Decision to shut down an idle coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownDecision {
    pub name: String,
    pub is_isolated: bool,
}

/// Decision to nudge an interrupted coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterruptNudge {
    pub name: String,
}

/// Decision to alert the lead about a prompted coworker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptNudge {
    pub name: String,
    pub label: String,
}

// ---------------------------------------------------------------------------
// Lifecycle decision functions (pure — no async, no side effects)
// ---------------------------------------------------------------------------

/// Decide which coworkers should be shut down due to idleness.
///
/// Takes pre-collected state snapshots and mutable idle tracking.
/// Returns shutdown decisions without performing any side effects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_idle_shutdowns(
    coworkers: &[CoworkerSnapshot],
    busy_coworkers: &HashSet<String>,
    coworkers_with_open_prs: &HashSet<String>,
    active_reviewers: &HashSet<String>,
    idle_since: &mut HashMap<String, Instant>,
    now: Instant,
    now_utc: DateTime<Utc>,
    idle_break_duration: Duration,
    minimum_lifetime: Duration,
) -> Vec<ShutdownDecision> {
    let mut to_shutdown = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        // Check minimum lifetime
        let lifetime = now_utc.signed_duration_since(cw.started_at);
        if lifetime < chrono::Duration::from_std(minimum_lifetime).unwrap_or_default() {
            idle_since.remove(coworker);
            continue;
        }

        let is_busy = busy_coworkers
            .iter()
            .any(|b| b.eq_ignore_ascii_case(coworker));
        let has_open_pr = coworkers_with_open_prs
            .iter()
            .any(|c| c.eq_ignore_ascii_case(coworker));
        let is_reviewing = active_reviewers
            .iter()
            .any(|r| r.eq_ignore_ascii_case(coworker));

        if is_busy || has_open_pr || is_reviewing {
            idle_since.remove(coworker);
        } else if cw.isolated_tasks {
            // Isolated coworkers (reviewers) go on break immediately when idle
            to_shutdown.push(ShutdownDecision {
                name: coworker.clone(),
                is_isolated: true,
            });
        } else {
            match idle_since.get(coworker) {
                Some(since) => {
                    if now.duration_since(*since) >= idle_break_duration {
                        to_shutdown.push(ShutdownDecision {
                            name: coworker.clone(),
                            is_isolated: false,
                        });
                    }
                }
                None => {
                    idle_since.insert(coworker.clone(), now);
                }
            }
        }
    }

    // Remove shutdown coworkers from tracking
    for decision in &to_shutdown {
        idle_since.remove(&decision.name);
    }

    to_shutdown
}

/// Decide which coworkers should be nudged due to interrupted sessions.
///
/// Takes pane contents and mutable interruption tracking.
/// Returns nudge decisions without performing any side effects.
pub(crate) fn decide_interrupt_nudges(
    coworkers: &[CoworkerSnapshot],
    pane_contents: &HashMap<String, String>,
    interrupted_since: &mut HashMap<String, Instant>,
    now: Instant,
    nudge_duration: Duration,
) -> Vec<InterruptNudge> {
    let mut to_nudge = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        let pane_content = match pane_contents.get(coworker) {
            Some(content) => content,
            None => {
                interrupted_since.remove(coworker);
                continue;
            }
        };

        let is_interrupted = pane_content.contains("Interrupted")
            || pane_content.contains("What should Claude do instead?");

        if is_interrupted {
            match interrupted_since.get(coworker) {
                Some(since) => {
                    if now.duration_since(*since) >= nudge_duration {
                        to_nudge.push(InterruptNudge {
                            name: coworker.clone(),
                        });
                        interrupted_since.remove(coworker);
                    }
                }
                None => {
                    interrupted_since.insert(coworker.clone(), now);
                }
            }
        } else if interrupted_since.remove(coworker).is_some() {
            // No longer interrupted — cleared
        }
    }

    to_nudge
}

/// Decide which coworkers should trigger a lead prompt nudge.
///
/// Takes pane contents and mutable prompt tracking.
/// Returns nudge decisions without performing any side effects.
pub(crate) fn decide_prompt_nudges(
    coworkers: &[CoworkerSnapshot],
    pane_contents: &HashMap<String, String>,
    prompted_nudged: &mut HashMap<String, String>,
) -> Vec<PromptNudge> {
    let mut to_nudge = Vec::new();

    for cw in coworkers {
        let coworker = &cw.name;

        // Skip the lead — they're the human
        if coworker == "lead" {
            continue;
        }

        let pane_content = match pane_contents.get(coworker) {
            Some(content) => content,
            None => {
                prompted_nudged.remove(coworker);
                continue;
            }
        };

        match detect_interactive_prompt(pane_content) {
            Some(label) => {
                let fingerprint = label.to_string();
                match prompted_nudged.get(coworker) {
                    Some(prev) if prev == &fingerprint => {
                        // Already nudged for this exact prompt
                    }
                    _ => {
                        prompted_nudged.insert(coworker.clone(), fingerprint);
                        to_nudge.push(PromptNudge {
                            name: coworker.clone(),
                            label: label.to_string(),
                        });
                    }
                }
            }
            None => {
                prompted_nudged.remove(coworker);
            }
        }
    }

    to_nudge
}

/// Patterns that indicate a coworker is waiting on an interactive prompt.
const INTERACTIVE_PROMPT_PATTERNS: &[(&str, &str)] = &[
    ("Yes, and don't ask again for this project", "plan approval"),
    ("Yes, and bypass permissions", "plan approval"),
    ("Yes, clear context and bypass permissions", "plan approval"),
    ("Do you want to proceed?", "confirmation prompt"),
    ("Would you like to proceed?", "confirmation prompt"),
    ("Allow once", "permission request"),
    ("Allow always", "permission request"),
    ("Select an option", "question prompt"),
];

/// Check if pane content contains an interactive prompt that needs human input.
pub(crate) fn detect_interactive_prompt(pane_content: &str) -> Option<&'static str> {
    for (pattern, label) in INTERACTIVE_PROMPT_PATTERNS {
        if pane_content.contains(pattern) {
            return Some(label);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Detection types and functions
// ---------------------------------------------------------------------------

/// Patterns that indicate a coworker has hit a usage/rate limit.
const USAGE_LIMIT_PATTERNS: &[&str] = &[
    "usage limit",
    "rate limit",
    "Usage limit reached",
    "rate_limit_error",
    "You've hit your",
    "limit resets",
];

/// Decision output for usage limit detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageLimitDecision {
    /// Usage limit detected in pane — schedule a nudge.
    Detected { coworker: String },
    /// Nudge is already scheduled — skip re-detection.
    AlreadyScheduled,
    /// No usage limit found in any pane.
    NoneDetected,
}

/// Decide whether pane contents indicate a usage limit.
///
/// Scans pane contents for known usage/rate limit patterns. If a nudge is
/// already scheduled (`nudge_already_scheduled`), skips re-detection.
pub(crate) fn decide_usage_limit_detection(
    pane_contents: &[(String, String)], // (coworker_name, pane_content)
    nudge_already_scheduled: bool,
) -> UsageLimitDecision {
    if nudge_already_scheduled {
        return UsageLimitDecision::AlreadyScheduled;
    }

    for (name, content) in pane_contents {
        let has_limit = USAGE_LIMIT_PATTERNS
            .iter()
            .any(|p| content.to_lowercase().contains(&p.to_lowercase()));

        if has_limit {
            return UsageLimitDecision::Detected {
                coworker: name.clone(),
            };
        }
    }

    UsageLimitDecision::NoneDetected
}

/// Decision output for usage limit expiry check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageLimitExpiryDecision {
    /// Nudge time has arrived — nudge all coworkers.
    NudgeNow,
    /// Nudge is scheduled but not yet due.
    NotYet,
    /// No nudge is scheduled.
    NoNudge,
}

/// Decide whether a scheduled usage limit nudge should fire.
pub(crate) fn decide_usage_limit_expiry(
    nudge_at: Option<tokio::time::Instant>,
    now: tokio::time::Instant,
) -> UsageLimitExpiryDecision {
    match nudge_at {
        Some(at) if now >= at => UsageLimitExpiryDecision::NudgeNow,
        Some(_) => UsageLimitExpiryDecision::NotYet,
        None => UsageLimitExpiryDecision::NoNudge,
    }
}

/// Try to parse a duration from usage limit text.
///
/// Looks for patterns like "try again in 15 minutes", "resets in 2 hours",
/// "available after 30 minutes". Returns a default of 15 minutes if no
/// parseable duration is found.
pub(crate) fn parse_usage_limit_duration(pane_content: &str) -> Duration {
    let lower = pane_content.to_lowercase();

    for keyword in &["in ", "after "] {
        let mut search_from = 0;
        while let Some(rel_idx) = lower[search_from..].find(keyword) {
            let idx = search_from + rel_idx;
            let after = &lower[idx + keyword.len()..];
            search_from = idx + keyword.len();

            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = num_str.parse::<u64>() {
                if num == 0 {
                    continue;
                }
                let remaining = after[num_str.len()..].trim_start();
                if remaining.starts_with("hour") {
                    return Duration::from_secs(num * 3600);
                } else if remaining.starts_with("min") {
                    return Duration::from_secs(num * 60);
                } else if remaining.starts_with("sec") {
                    return Duration::from_secs(num);
                }
            }
        }
    }

    // Look for HH:MM timestamp pattern like "resets at 3:45" or "at 15:30"
    if let Some(idx) = lower.find("at ") {
        let after = &lower[idx + 3..];
        let time_str: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ':')
            .collect();
        if let Some((h, m)) = time_str.split_once(':')
            && let (Ok(hour), Ok(min)) = (h.parse::<u32>(), m.parse::<u32>())
        {
            let now = chrono::Utc::now();
            let mut target = now
                .date_naive()
                .and_hms_opt(hour, min, 0)
                .unwrap_or_default();
            if target < now.naive_utc() {
                target += chrono::Duration::days(1);
            }
            let diff = target - now.naive_utc();
            if let Ok(std_diff) = diff.to_std() {
                return std_diff;
            }
        }
    }

    // Default: 15 minutes
    Duration::from_secs(15 * 60)
}

/// Check if pane content contains any usage/rate limit patterns.
pub(crate) fn has_usage_limit_pattern(pane_content: &str) -> bool {
    let lower = pane_content.to_lowercase();
    USAGE_LIMIT_PATTERNS
        .iter()
        .any(|p| lower.contains(&p.to_lowercase()))
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

    // -----------------------------------------------------------------------
    // Helpers for lifecycle decision tests
    // -----------------------------------------------------------------------

    fn cw(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(minutes_old),
            isolated_tasks: false,
        }
    }

    fn cw_isolated(name: &str, minutes_old: i64) -> CoworkerSnapshot {
        CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(minutes_old),
            isolated_tasks: true,
        }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // decide_idle_shutdowns tests
    // -----------------------------------------------------------------------

    #[test]
    fn idle_shutdown_after_timeout() {
        let coworkers = vec![cw("york", 10)];
        let mut idle_since = HashMap::new();
        // york has been idle for 60s already
        idle_since.insert("york".to_string(), Instant::now() - Duration::from_secs(60));

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "york");
        assert!(!decisions[0].is_isolated);
        assert!(!idle_since.contains_key("york"));
    }

    #[test]
    fn idle_shutdown_skips_busy_coworker() {
        let coworkers = vec![cw("york", 10)];
        let mut idle_since = HashMap::new();
        idle_since.insert("york".to_string(), Instant::now() - Duration::from_secs(60));

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&["york"]),
            &set(&[]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
        // Busy coworker removed from idle tracking
        assert!(!idle_since.contains_key("york"));
    }

    #[test]
    fn idle_shutdown_skips_coworker_with_open_pr() {
        let coworkers = vec![cw("york", 10)];
        let mut idle_since = HashMap::new();
        idle_since.insert("york".to_string(), Instant::now() - Duration::from_secs(60));

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&["york"]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_active_reviewer() {
        let coworkers = vec![cw("york", 10)];
        let mut idle_since = HashMap::new();
        idle_since.insert("york".to_string(), Instant::now() - Duration::from_secs(60));

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&["york"]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
    }

    #[test]
    fn idle_shutdown_skips_young_coworker() {
        let coworkers = vec![cw("york", 2)]; // Only 2 minutes old
        let mut idle_since = HashMap::new();
        idle_since.insert("york".to_string(), Instant::now() - Duration::from_secs(60));

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert!(decisions.is_empty());
        // Young coworker also removed from idle tracking
        assert!(!idle_since.contains_key("york"));
    }

    #[test]
    fn idle_shutdown_isolated_coworker_immediate() {
        let coworkers = vec![cw_isolated("reviewer", 10)];
        let mut idle_since = HashMap::new();

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].name, "reviewer");
        assert!(decisions[0].is_isolated);
    }

    #[test]
    fn idle_shutdown_starts_tracking_newly_idle() {
        let coworkers = vec![cw("york", 10)];
        let mut idle_since = HashMap::new();

        let decisions = decide_idle_shutdowns(
            &coworkers,
            &set(&[]),
            &set(&[]),
            &set(&[]),
            &mut idle_since,
            Instant::now(),
            Utc::now(),
            Duration::from_secs(30),
            Duration::from_secs(300),
        );

        // No shutdown yet — just started tracking
        assert!(decisions.is_empty());
        assert!(idle_since.contains_key("york"));
    }

    // -----------------------------------------------------------------------
    // decide_interrupt_nudges tests
    // -----------------------------------------------------------------------

    #[test]
    fn interrupt_nudge_after_duration() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Some output\nInterrupted\n".to_string());
        let mut interrupted_since = HashMap::new();
        interrupted_since.insert("york".to_string(), Instant::now() - Duration::from_secs(90));

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut interrupted_since,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].name, "york");
        // Tracking reset after nudge
        assert!(!interrupted_since.contains_key("york"));
    }

    #[test]
    fn interrupt_nudge_not_yet_due() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Interrupted".to_string());
        let mut interrupted_since = HashMap::new();
        interrupted_since.insert("york".to_string(), Instant::now() - Duration::from_secs(10));

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut interrupted_since,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        // Still tracking
        assert!(interrupted_since.contains_key("york"));
    }

    #[test]
    fn interrupt_tracking_cleared_when_no_longer_interrupted() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "All good, working fine".to_string());
        let mut interrupted_since = HashMap::new();
        interrupted_since.insert("york".to_string(), Instant::now() - Duration::from_secs(90));

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut interrupted_since,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        // Tracking cleared
        assert!(!interrupted_since.contains_key("york"));
    }

    #[test]
    fn interrupt_starts_tracking_newly_interrupted() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "What should Claude do instead?".to_string(),
        );
        let mut interrupted_since = HashMap::new();

        let nudges = decide_interrupt_nudges(
            &coworkers,
            &pane_contents,
            &mut interrupted_since,
            Instant::now(),
            Duration::from_secs(60),
        );

        assert!(nudges.is_empty());
        assert!(interrupted_since.contains_key("york"));
    }

    // -----------------------------------------------------------------------
    // decide_prompt_nudges tests
    // -----------------------------------------------------------------------

    #[test]
    fn prompt_nudge_new_prompt_detected() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "Some output\nAllow once\nAllow always".to_string(),
        );
        let mut prompted_nudged = HashMap::new();

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut prompted_nudged);

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].name, "york");
        assert_eq!(nudges[0].label, "permission request");
        assert_eq!(prompted_nudged.get("york").unwrap(), "permission request");
    }

    #[test]
    fn prompt_nudge_skips_already_nudged_same_type() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Allow once\nAllow always".to_string());
        let mut prompted_nudged = HashMap::new();
        prompted_nudged.insert("york".to_string(), "permission request".to_string());

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut prompted_nudged);

        assert!(nudges.is_empty());
    }

    #[test]
    fn prompt_nudge_fires_for_different_prompt_type() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert(
            "york".to_string(),
            "Yes, and don't ask again for this project".to_string(),
        );
        let mut prompted_nudged = HashMap::new();
        prompted_nudged.insert("york".to_string(), "permission request".to_string());

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut prompted_nudged);

        assert_eq!(nudges.len(), 1);
        assert_eq!(nudges[0].label, "plan approval");
    }

    #[test]
    fn prompt_nudge_clears_when_no_prompt() {
        let coworkers = vec![cw("york", 10)];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("york".to_string(), "Working normally".to_string());
        let mut prompted_nudged = HashMap::new();
        prompted_nudged.insert("york".to_string(), "permission request".to_string());

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut prompted_nudged);

        assert!(nudges.is_empty());
        assert!(!prompted_nudged.contains_key("york"));
    }

    #[test]
    fn prompt_nudge_skips_lead() {
        let coworkers = vec![CoworkerSnapshot {
            name: "lead".to_string(),
            started_at: Utc::now() - chrono::Duration::minutes(10),
            isolated_tasks: false,
        }];
        let mut pane_contents = HashMap::new();
        pane_contents.insert("lead".to_string(), "Allow once\nAllow always".to_string());
        let mut prompted_nudged = HashMap::new();

        let nudges = decide_prompt_nudges(&coworkers, &pane_contents, &mut prompted_nudged);

        assert!(nudges.is_empty());
    }
}
