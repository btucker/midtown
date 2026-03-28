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

/// Build a default `IdleShutdownContext` with a single coworker 120s old and all
/// exclusion sets empty. The coworker is old enough to be shut down unless an
/// exclusion set is populated.
fn default_ctx_with_coworker(
    name: &'static str,
) -> (CoworkerSnapshot, IdleShutdownContext<'static>) {
    let coworker = CoworkerSnapshot {
        name: name.to_string(),
        started_at: Utc::now() - chrono::Duration::seconds(120),
        session_id: None,
    };
    let ctx = IdleShutdownContext {
        coworkers: Box::leak(Box::new([CoworkerSnapshot {
            name: name.to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(120),
            session_id: None,
        }])),
        busy_coworkers: Box::leak(Box::new(HashSet::new())),
        coworkers_with_open_prs: Box::leak(Box::new(HashSet::new())),
        active_reviewers: Box::leak(Box::new(HashSet::new())),
        coworkers_with_unblocked_deps: Box::leak(Box::new(HashSet::new())),
        ci_passed_pr_coworkers: Box::leak(Box::new(HashSet::new())),
        usage_limited_coworkers: Box::leak(Box::new(HashSet::new())),
        api_error_coworkers: Box::leak(Box::new(HashSet::new())),
        auth_error_coworkers: Box::leak(Box::new(HashSet::new())),
        pending_task_owners: Box::leak(Box::new(HashSet::new())),
        review_feedback_pr_coworkers: Box::leak(Box::new(HashSet::new())),
        coworkers_with_active_tools: Box::leak(Box::new(HashSet::new())),
        now_utc: Utc::now(),
        minimum_lifetime: Duration::from_secs(90),
        repo_name: "test-repo",
        channel_lead_names: Box::leak(Box::new(HashSet::new())),
    };
    (coworker, ctx)
}

#[test]
fn idle_shutdown_skips_busy_coworker() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.busy_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_coworker_with_open_pr() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_open_prs = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_active_reviewer() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.active_reviewers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_unblocked_deps() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_unblocked_deps = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_ci_passed_pr() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.ci_passed_pr_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_usage_limited() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.usage_limited_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_api_error() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.api_error_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_auth_error() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.auth_error_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_pending_task_owner() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.pending_task_owners = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_review_feedback() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.review_feedback_pr_coworkers = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_active_tools() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.coworkers_with_active_tools = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_skips_channel_lead() {
    let (_cw, mut ctx) = default_ctx_with_coworker("york");
    let set = Box::leak(Box::new(HashSet::from(["york".to_string()])));
    ctx.channel_lead_names = set;
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_empty_coworkers_returns_empty() {
    let ctx = IdleShutdownContext {
        coworkers: &[],
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
    assert!(decide_idle_shutdowns(&ctx).is_empty());
}

#[test]
fn idle_shutdown_multiple_coworkers_only_idle_ones() {
    // "busy-one": 120s old but in busy set — excluded
    // "young-one": 60s old — excluded by startup window
    // "idle-one": 120s old, no exclusions — should be shut down
    let coworkers = vec![
        CoworkerSnapshot {
            name: "busy-one".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(120),
            session_id: None,
        },
        CoworkerSnapshot {
            name: "young-one".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(60),
            session_id: None,
        },
        CoworkerSnapshot {
            name: "idle-one".to_string(),
            started_at: Utc::now() - chrono::Duration::seconds(120),
            session_id: None,
        },
    ];
    let busy = HashSet::from(["busy-one".to_string()]);
    let ctx = IdleShutdownContext {
        coworkers: &coworkers,
        busy_coworkers: &busy,
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
        "only the idle coworker should be shut down, got: {:?}",
        decisions
    );
    assert_eq!(decisions[0].name, "idle-one");
}
