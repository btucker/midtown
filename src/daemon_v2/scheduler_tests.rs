use super::*;
use std::time::{Duration, Instant};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;

fn noop_decision(_proj: &Projections, _channel: &str) -> Vec<Command> {
    vec![]
}

fn other_decision(_proj: &Projections, _channel: &str) -> Vec<Command> {
    vec![]
}

#[test]
fn scheduler_returns_decisions_in_interval_order() {
    let mut sched = Scheduler::new();
    // Register longer interval first to verify ordering
    sched.register("slow", Duration::from_secs(60), noop_decision);
    sched.register("fast", Duration::from_secs(10), other_decision);

    let now = Instant::now();
    let due = sched.due_decisions(now);

    // Both should be due (never ran), returned shortest interval first
    assert_eq!(due.len(), 2);
    assert_eq!(due[0].name, "fast");
    assert_eq!(due[1].name, "slow");
}

#[test]
fn scheduler_respects_intervals() {
    let mut sched = Scheduler::new();
    sched.register("fast", Duration::from_secs(5), noop_decision);
    sched.register("slow", Duration::from_secs(60), other_decision);

    let now = Instant::now();

    // Mark "fast" as just ran
    sched.mark_ran("fast", now);

    // Both were due before; now "fast" is not due yet
    let due = sched.due_decisions(now);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].name, "slow");
}

#[test]
fn next_deadline_returns_soonest() {
    let mut sched = Scheduler::new();
    sched.register("fast", Duration::from_secs(10), noop_decision);
    sched.register("slow", Duration::from_secs(60), other_decision);

    let now = Instant::now();

    // Mark both as just ran
    sched.mark_ran("fast", now);
    sched.mark_ran("slow", now);

    // Next deadline is the shorter interval (fast = 10s)
    let deadline = sched.next_deadline(now).expect("should have a deadline");
    // Should be approximately 10s (slightly less due to elapsed time, but saturating_sub protects us)
    assert!(deadline <= Duration::from_secs(10));
    assert!(deadline >= Duration::from_secs(9));
}
