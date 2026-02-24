use super::*;
use std::collections::HashSet;

/// Regression test (!1730 item 3): a coworker in the 60–90s startup window must
/// be protected from idle shutdown. Session startup takes 40-60s; the minimum
/// lifetime was increased from 60s to 90s to provide a 30s buffer.
///
/// Previously, a coworker that was exactly 60s old would NOT be protected
/// (60s < 60s = false). With the new 90s threshold, coworkers up to 90s old
/// are protected during their initialization window.
#[test]
fn idle_shutdown_skips_coworker_in_startup_window() {
    // Coworker 75 seconds old — past old 60s threshold, within new 90s threshold.
    let coworker = CoworkerSnapshot {
        name: "york".to_string(),
        started_at: Utc::now() - chrono::Duration::seconds(75),
        session_id: None,
    };
    let ctx = IdleShutdownContext {
        coworkers: &[coworker],
        busy_coworkers: &HashSet::new(),
        coworkers_with_open_prs: &HashSet::new(),
        active_reviewers: &HashSet::new(),
        coworkers_with_unblocked_deps: &HashSet::new(),
        ci_passed_pr_coworkers: &HashSet::new(),
        usage_limited_coworkers: &HashSet::new(),
        api_error_coworkers: &HashSet::new(),
        auth_error_coworkers: &HashSet::new(),
        pending_task_owners: &HashSet::new(),
        review_feedback_pr_coworkers: &HashSet::new(),
        coworkers_with_active_tools: &HashSet::new(),
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: &HashSet::new(),
    };
    let decisions = decide_idle_shutdowns(&ctx);
    assert!(
        decisions.is_empty(),
        "coworker in 60-90s startup window must be protected (minimum_lifetime=90s), got: {:?}",
        decisions
    );
}

/// Regression test (!1730 item 3): a coworker older than 90s with no work
/// should still be shut down — the new threshold doesn't protect coworkers
/// indefinitely.
#[test]
fn idle_shutdown_triggers_after_90s_threshold() {
    let coworker = CoworkerSnapshot {
        name: "york".to_string(),
        started_at: Utc::now() - chrono::Duration::seconds(95),
        session_id: None,
    };
    let ctx = IdleShutdownContext {
        coworkers: &[coworker],
        busy_coworkers: &HashSet::new(),
        coworkers_with_open_prs: &HashSet::new(),
        active_reviewers: &HashSet::new(),
        coworkers_with_unblocked_deps: &HashSet::new(),
        ci_passed_pr_coworkers: &HashSet::new(),
        usage_limited_coworkers: &HashSet::new(),
        api_error_coworkers: &HashSet::new(),
        auth_error_coworkers: &HashSet::new(),
        pending_task_owners: &HashSet::new(),
        review_feedback_pr_coworkers: &HashSet::new(),
        coworkers_with_active_tools: &HashSet::new(),
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: &HashSet::new(),
    };
    let decisions = decide_idle_shutdowns(&ctx);
    assert_eq!(
        decisions.len(),
        1,
        "idle coworker past 90s threshold should be shut down, got: {:?}",
        decisions
    );
    assert_eq!(decisions[0].name, "york");
}
