//! Tests for channel lead behavior in decision functions.
//!
//! Channel leads are on-demand sessions that are idle-shutdown like any other coworker,
//! but are exempt from task dispatch and orphan recovery.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::Utc;

use super::{
    CoworkerSnapshot, IdleShutdownContext, OrphanRecoveryContext, PendingTaskAction,
    decide_idle_shutdowns, decide_orphan_recovery, decide_pending_task_action,
};

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn names(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn channel_lead_is_idle_shutdown() {
    // Channel leads are now on-demand and should be shut down when idle,
    // just like any other coworker.
    let coworkers = vec![CoworkerSnapshot {
        name: "ops".to_string(),
        started_at: Utc::now() - chrono::Duration::minutes(10),
        session_id: None,
    }];
    let empty = empty_set();
    let ctx = IdleShutdownContext {
        coworkers: &coworkers,
        busy_coworkers: &empty,
        coworkers_with_open_prs: &empty,
        active_reviewers: &empty,
        coworkers_with_unblocked_deps: &empty,
        ci_passed_pr_coworkers: &empty,
        usage_limited_coworkers: &empty,
        api_error_coworkers: &empty,
        auth_error_coworkers: &empty,
        pending_task_owners: &empty,
        review_feedback_pr_coworkers: &empty,
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(300),
        repo_name: "test-repo",
    };
    let result = decide_idle_shutdowns(&ctx);
    assert_eq!(
        result.len(),
        1,
        "channel lead should be idle-shutdown when idle (on-demand behavior)"
    );
    assert_eq!(result[0].name, "ops");
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
    };
    let result = decide_orphan_recovery(&ctx);
    assert!(
        result.is_none(),
        "channel lead owned task should not be orphan recovered"
    );
}
