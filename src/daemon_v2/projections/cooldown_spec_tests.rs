//! Behavioral tests for v2-spec.md Section 6.4: CooldownTracker
//!
//! Each test maps to a specific SHALL requirement from the spec.
//! Pure — no I/O, no async.

use std::time::Duration;

use crate::daemon_v2::projections::cooldowns::{CooldownCategory, CooldownTracker};

// ── Section 6.4: CooldownTracker ─────────────────────────────────────────────

/// Spec 6.4: WHEN OrphanSpawn cooldown recorded THEN 60s cooldown active
#[test]
fn orphan_spawn_cooldown_is_60s() {
    assert_eq!(
        CooldownCategory::OrphanSpawn.duration(),
        Duration::from_secs(60),
        "OrphanSpawn cooldown duration should be 60s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());

    assert!(
        tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"),
        "OrphanSpawn cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN is_active checked for an expired or unrecorded cooldown THEN
/// false returned
#[test]
fn unrecorded_cooldown_is_not_active() {
    let tracker = CooldownTracker::default();

    assert!(
        !tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"),
        "unrecorded OrphanSpawn cooldown should not be active"
    );
    assert!(
        !tracker.is_active(CooldownCategory::AgentDispatch, "any-key"),
        "unrecorded AgentDispatch cooldown should not be active"
    );
}

/// Spec 6.4: WHEN AgentDispatch cooldown recorded THEN 30s cooldown active
#[test]
fn agent_dispatch_cooldown_is_30s() {
    assert_eq!(
        CooldownCategory::AgentDispatch.duration(),
        Duration::from_secs(30),
        "AgentDispatch cooldown duration should be 30s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::AgentDispatch, "task-1".into());

    assert!(
        tracker.is_active(CooldownCategory::AgentDispatch, "task-1"),
        "AgentDispatch cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN SpawnFailure cooldown recorded THEN 120s cooldown active
#[test]
fn spawn_failure_cooldown_is_120s() {
    assert_eq!(
        CooldownCategory::SpawnFailure.duration(),
        Duration::from_secs(120),
        "SpawnFailure cooldown duration should be 120s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::SpawnFailure, "agent-fail".into());

    assert!(
        tracker.is_active(CooldownCategory::SpawnFailure, "agent-fail"),
        "SpawnFailure cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN MergeRebaseNudge cooldown recorded THEN 3600s cooldown active
#[test]
fn merge_rebase_nudge_cooldown_is_3600s() {
    assert_eq!(
        CooldownCategory::MergeRebaseNudge.duration(),
        Duration::from_secs(3600),
        "MergeRebaseNudge cooldown duration should be 3600s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::MergeRebaseNudge, "agent-merge".into());

    assert!(
        tracker.is_active(CooldownCategory::MergeRebaseNudge, "agent-merge"),
        "MergeRebaseNudge cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN RebaseRegression cooldown recorded THEN 3600s cooldown active
#[test]
fn rebase_regression_cooldown_is_3600s() {
    assert_eq!(
        CooldownCategory::RebaseRegression.duration(),
        Duration::from_secs(3600),
        "RebaseRegression cooldown duration should be 3600s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::RebaseRegression, "agent-rebase".into());

    assert!(
        tracker.is_active(CooldownCategory::RebaseRegression, "agent-rebase"),
        "RebaseRegression cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN LeadWorktreeFreshness cooldown recorded THEN 300s cooldown active
#[test]
fn lead_worktree_freshness_cooldown_is_300s() {
    assert_eq!(
        CooldownCategory::LeadWorktreeFreshness.duration(),
        Duration::from_secs(300),
        "LeadWorktreeFreshness cooldown duration should be 300s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::LeadWorktreeFreshness, "lead-main".into());

    assert!(
        tracker.is_active(CooldownCategory::LeadWorktreeFreshness, "lead-main"),
        "LeadWorktreeFreshness cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN TaskNudge cooldown recorded THEN 3600s cooldown active
#[test]
fn task_nudge_cooldown_is_3600s() {
    assert_eq!(
        CooldownCategory::TaskNudge.duration(),
        Duration::from_secs(3600),
        "TaskNudge cooldown duration should be 3600s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::TaskNudge, "task-nudge-key".into());

    assert!(
        tracker.is_active(CooldownCategory::TaskNudge, "task-nudge-key"),
        "TaskNudge cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: WHEN NoteStaleness cooldown recorded THEN 3600s cooldown active
#[test]
fn note_staleness_cooldown_is_3600s() {
    assert_eq!(
        CooldownCategory::NoteStaleness.duration(),
        Duration::from_secs(3600),
        "NoteStaleness cooldown duration should be 3600s"
    );

    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::NoteStaleness, "note-key".into());

    assert!(
        tracker.is_active(CooldownCategory::NoteStaleness, "note-key"),
        "NoteStaleness cooldown should be active immediately after recording"
    );
}

/// Spec 6.4: Cooldown for one key does not affect a different key of the same category
#[test]
fn cooldown_is_scoped_to_key() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-a".into());

    assert!(
        tracker.is_active(CooldownCategory::OrphanSpawn, "agent-a"),
        "agent-a should be on cooldown"
    );
    assert!(
        !tracker.is_active(CooldownCategory::OrphanSpawn, "agent-b"),
        "agent-b should NOT be on cooldown — different key"
    );
}

/// Spec 6.4: Cooldown for one category does not affect a different category for the same key
#[test]
fn cooldown_is_scoped_to_category() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());

    assert!(
        tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"),
        "OrphanSpawn should be active"
    );
    assert!(
        !tracker.is_active(CooldownCategory::AgentDispatch, "agent-1"),
        "AgentDispatch should NOT be active — different category"
    );
}
