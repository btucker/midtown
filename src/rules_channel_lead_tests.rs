//! Tests for channel lead behavior in decision functions.
//!
//! Channel leads are long-running domain experts for topic channels. They are
//! exempt from task dispatch and orphan recovery.

use std::collections::{HashMap, HashSet};

use super::{
    OrphanRecoveryContext, PendingTaskAction, decide_orphan_recovery, decide_pending_task_action,
};

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn names(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn channel_lead_owned_task_is_skipped() {
    let result = decide_pending_task_action(
        "42",
        "some task",
        "ops",
        &HashSet::new(), // active_names
        false,           // at_dev_limit
        false,           // on_nudge_cooldown
        false,           // is_owner_reviewer
        false,           // has_in_progress_task
        true,            // is_channel_lead
    );
    assert!(
        matches!(result, PendingTaskAction::Skip { .. }),
        "channel lead owned task should be skipped"
    );
}

#[test]
fn channel_lead_owned_task_not_orphan_recovered() {
    // A task owned by a channel lead (e.g. "ops") should NOT trigger orphan recovery,
    // even if the channel lead is not in the active_names set.
    let tasks = vec![("99".to_string(), "ops task".to_string(), "ops".to_string())];
    let empty = empty_set();
    let empty_map: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let leads = names(&["ops"]);
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        at_dev_limit: false,
        coworkers_with_open_prs: &empty,
        review_feedback_pr_coworkers: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map,
        channel_lead_names: &leads,
        spawn_failure_cooldown_names: &empty,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(
        result.is_none(),
        "channel lead owned task should not be orphan recovered"
    );
}

#[test]
fn orphan_recovery_skips_cooldown_blocked_owner_recovers_next() {
    // Two orphaned tasks: first owner ("lexington") is on spawn failure cooldown,
    // second owner ("park") is not. Recovery should skip lexington and return park's task.
    let tasks = vec![
        (
            "10".to_string(),
            "lexington task".to_string(),
            "lexington".to_string(),
        ),
        (
            "20".to_string(),
            "park task".to_string(),
            "park".to_string(),
        ),
    ];
    let empty = empty_set();
    let empty_map: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let cooldown = names(&["lexington"]);
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        at_dev_limit: false,
        coworkers_with_open_prs: &empty,
        review_feedback_pr_coworkers: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map,
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(result.is_some(), "should recover park's task");
    let recovery = result.unwrap();
    assert_eq!(recovery.task_id, "20");
    assert_eq!(recovery.owner, "park");
}

#[test]
fn orphan_recovery_returns_none_when_all_on_cooldown() {
    // All owners on cooldown — no recovery should happen.
    let tasks = vec![
        (
            "10".to_string(),
            "lexington task".to_string(),
            "lexington".to_string(),
        ),
        (
            "20".to_string(),
            "park task".to_string(),
            "park".to_string(),
        ),
    ];
    let empty = empty_set();
    let empty_map: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let cooldown = names(&["lexington", "park"]);
    let ctx = OrphanRecoveryContext {
        in_progress: &tasks,
        active_names: &empty,
        at_dev_limit: false,
        coworkers_with_open_prs: &empty,
        review_feedback_pr_coworkers: &empty,
        recently_stopped: &empty,
        attached_coworkers: &empty_map,
        channel_lead_names: &empty,
        spawn_failure_cooldown_names: &cooldown,
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(
        result.is_none(),
        "should not recover when all owners are on cooldown"
    );
}
