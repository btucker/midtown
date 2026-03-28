use super::*;
use std::time::Duration;

#[test]
fn new_cooldown_is_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());
    assert!(tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"));
}

#[test]
fn unknown_cooldown_is_not_active() {
    let tracker = CooldownTracker::default();
    assert!(!tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"));
}

#[test]
fn different_key_not_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());
    assert!(!tracker.is_active(CooldownCategory::OrphanSpawn, "agent-2"));
}

#[test]
fn different_category_not_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());
    assert!(!tracker.is_active(CooldownCategory::SpawnFailure, "agent-1"));
}

#[test]
fn category_durations_are_positive() {
    let categories = [
        CooldownCategory::OrphanSpawn,
        CooldownCategory::AgentDispatch,
        CooldownCategory::SpawnFailure,
        CooldownCategory::MergeRebaseNudge,
        CooldownCategory::RebaseRegression,
        CooldownCategory::LeadWorktreeFreshness,
        CooldownCategory::TaskNudge,
        CooldownCategory::NoteStaleness,
    ];
    for cat in categories {
        assert!(cat.duration() > Duration::ZERO, "{cat:?} has zero duration");
    }
}
