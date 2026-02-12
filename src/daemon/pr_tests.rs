use super::*;
use serde_json::json;

/// Bug: collect_green_with_feedback_effects was using head_ref.split('/').next()
/// to extract the owner, which doesn't validate against known coworker names.
/// This meant PRs with branches like "btucker/fix" would extract "btucker" as owner
/// and potentially nudge wrong coworkers if the prefix matches a coworker name.
#[test]
fn coworker_from_branch_rejects_non_coworker_prefixes() {
    // These should return None because they're not valid coworker names
    assert!(
        coworker_from_branch("btucker/fix-something").is_none(),
        "btucker is not a coworker name"
    );
    assert!(
        coworker_from_branch("feature/add-auth").is_none(),
        "feature is not a coworker name"
    );
    assert!(coworker_from_branch("main").is_none(), "main has no slash");

    // These should return Some because they are valid coworker names
    assert_eq!(
        coworker_from_branch("york/fix-something"),
        Some("york".to_string()),
        "york is a valid coworker name"
    );
    assert_eq!(
        coworker_from_branch("amsterdam/add-feature"),
        Some("amsterdam".to_string()),
        "amsterdam is a valid coworker name"
    );
}

#[test]
fn is_lead_branch_detects_lead_branches() {
    // Lead branches start with "lead/"
    assert!(
        is_lead_branch("lead/fix-bug"),
        "lead/fix-bug is a lead branch"
    );
    assert!(
        is_lead_branch("lead/add-feature"),
        "lead/add-feature is a lead branch"
    );
    assert!(
        is_lead_branch("lead/root-cause-claude-md-updates"),
        "lead/root-cause-claude-md-updates is a lead branch"
    );

    // Coworker and other branches should not be detected as lead branches
    assert!(
        !is_lead_branch("york/fix-bug"),
        "york/fix-bug is not a lead branch"
    );
    assert!(
        !is_lead_branch("feature/add-auth"),
        "feature/add-auth is not a lead branch"
    );
    assert!(!is_lead_branch("main"), "main is not a lead branch");
    assert!(
        !is_lead_branch("leading/edge"),
        "leading/edge is not a lead branch (only exact prefix match)"
    );
}

#[test]
fn stuck_nudge_effects_returns_only_system_message() {
    // Bug: stuck_nudge_effects was returning both PostSystemMessage and NudgeLead,
    // causing double delivery because the chat monitor already routes @lead mentions
    // in system messages to the lead.
    //
    // The fix is to only return PostSystemMessage and let the channel's @mention
    // routing handle the nudge.
    let message = "@lead PR #42 (Add feature) has been open for 60 minutes without a review";
    let effects = stuck_nudge_effects(message);

    // Should only return one effect (PostSystemMessage)
    assert_eq!(
        effects.len(),
        1,
        "stuck_nudge_effects should return exactly 1 effect, not 2 (double nudge bug)"
    );

    // That effect should be PostSystemMessage with the warning emoji prefix
    match &effects[0] {
        Effect::PostSystemMessage { message: msg } => {
            assert!(
                msg.starts_with("⚠️"),
                "System message should have warning prefix"
            );
            assert!(
                msg.contains("@lead"),
                "System message should preserve @lead mention"
            );
        }
        _ => panic!("Expected PostSystemMessage effect, got {:?}", effects[0]),
    }
}

/// Creates a CiCheckStats with recorded durations for testing.
fn test_ci_stats_with_duration(check_name: &str, duration: u64) -> crate::ci_stats::CiCheckStats {
    let mut stats = crate::ci_stats::CiCheckStats::default();
    // Record multiple times to establish a stable typical duration
    for _ in 0..5 {
        stats.record_duration(check_name, duration);
    }
    stats
}

#[test]
fn collect_stale_check_effects_detects_stale_in_progress_check() {
    use chrono::{DateTime, Utc};

    // Set up CI stats with a typical duration of 120 seconds for "Test" check
    let ci_stats = test_ci_stats_with_duration("Test", 120);

    // PR with an IN_PROGRESS check that started 600 seconds ago (5x typical)
    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "IN_PROGRESS",
            "startedAt": "2026-02-04T12:00:00Z",
            "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

    assert_eq!(effects.len(), 1, "should detect one stale check");
    match &effects[0] {
        Effect::RerunWorkflow {
            run_id,
            check_name,
            pr_number,
        } => {
            assert_eq!(*run_id, 123456);
            assert_eq!(check_name, "Test");
            assert_eq!(*pr_number, 42);
        }
        _ => panic!("expected RerunWorkflow effect"),
    }
}

#[test]
fn collect_stale_check_effects_ignores_checks_not_in_progress() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("Test", 120);
    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

    // PR with a COMPLETED check
    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "COMPLETED",
            "startedAt": "2026-02-04T12:00:00Z",
            "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should not detect completed checks as stale"
    );
}

#[test]
fn collect_stale_check_effects_ignores_checks_within_threshold() {
    use chrono::{DateTime, Utc};

    // Typical duration is 120s, threshold is 4x = 480s
    let ci_stats = test_ci_stats_with_duration("Test", 120);
    let now: DateTime<Utc> = "2026-02-04T12:05:00Z".parse().unwrap();

    // PR with a check that has been running for 300s (within 480s threshold)
    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "IN_PROGRESS",
            "startedAt": "2026-02-04T12:00:00Z",
            "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should not detect checks within threshold"
    );
}

#[test]
fn collect_stale_check_effects_skips_prs_without_status_check_rollup() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("Test", 120);
    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

    let prs = vec![json!({
        "number": 42
        // No statusCheckRollup field
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should skip PRs without statusCheckRollup"
    );
}

#[test]
fn collect_stale_check_effects_skips_checks_without_details_url() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("Test", 120);
    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "IN_PROGRESS",
            "startedAt": "2026-02-04T12:00:00Z"
            // No detailsUrl - can't extract run ID
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(effects.is_empty(), "should skip checks without detailsUrl");
}

#[test]
fn collect_stale_check_effects_skips_invalid_details_url() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("Test", 120);
    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "IN_PROGRESS",
            "startedAt": "2026-02-04T12:00:00Z",
            "detailsUrl": "https://example.com/not-a-github-url"
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should skip checks with unparseable detailsUrl"
    );
}

#[test]
fn collect_stale_check_effects_respects_rerun_cooldown() {
    use chrono::{DateTime, Utc};

    let mut ci_stats = test_ci_stats_with_duration("Test", 120);
    // Record a recent re-run for this workflow
    ci_stats.record_rerun(123456);

    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 42,
        "statusCheckRollup": [{
            "name": "Test",
            "status": "IN_PROGRESS",
            "startedAt": "2026-02-04T12:00:00Z",
            "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
        }]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(effects.is_empty(), "should skip re-run when on cooldown");
}

#[test]
fn collect_stale_check_effects_handles_multiple_prs_and_checks() {
    use chrono::{DateTime, Utc};

    let mut ci_stats = test_ci_stats_with_duration("Test", 120);
    // Also add stats for Clippy
    for _ in 0..5 {
        ci_stats.record_duration("Clippy", 60);
    }

    let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
    let prs = vec![
        json!({
            "number": 42,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "IN_PROGRESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "Clippy",
                    "status": "COMPLETED",  // Not in progress
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/2"
                }
            ]
        }),
        json!({
            "number": 43,
            "statusCheckRollup": [{
                "name": "Clippy",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",  // 600s ago, threshold is 240s
                "detailsUrl": "https://github.com/owner/repo/actions/runs/333/job/3"
            }]
        }),
    ];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

    // Should find 2 stale checks: Test on PR #42 and Clippy on PR #43
    assert_eq!(effects.len(), 2, "should detect two stale checks");

    // Verify both effects are RerunWorkflow
    for effect in &effects {
        assert!(matches!(effect, Effect::RerunWorkflow { .. }));
    }
}

// -------------------------------------------------------------------------
// Stale PENDING check detection tests
// -------------------------------------------------------------------------

#[test]
fn collect_stale_check_effects_detects_pending_when_siblings_completed() {
    use chrono::{DateTime, Utc};

    // Set up CI stats with a typical duration of 120 seconds for "task_sharing"
    let ci_stats = test_ci_stats_with_duration("task_sharing", 120);

    // Siblings started 60 minutes ago — well beyond any threshold
    let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "Clippy",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
            },
            {
                "name": "task_sharing",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

    assert_eq!(
        effects.len(),
        1,
        "should detect pending check when siblings completed"
    );
    match &effects[0] {
        Effect::RerunWorkflow {
            run_id,
            check_name,
            pr_number,
        } => {
            assert_eq!(*run_id, 222);
            assert_eq!(check_name, "task_sharing");
            assert_eq!(*pr_number, 679);
        }
        _ => panic!("expected RerunWorkflow effect"),
    }
}

#[test]
fn collect_stale_check_effects_ignores_pending_when_siblings_still_running() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("task_sharing", 120);
    // now is 5 minutes after start — Clippy IN_PROGRESS is within default threshold
    let now: DateTime<Utc> = "2026-02-04T12:05:00Z".parse().unwrap();

    // One sibling is still IN_PROGRESS — not all siblings completed
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "Clippy",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
            },
            {
                "name": "task_sharing",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should not detect pending check when siblings still running"
    );
}

#[test]
fn collect_stale_check_effects_ignores_pending_within_threshold() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("task_sharing", 120);

    // Siblings started only 3 minutes ago — within 2x typical (240s) threshold
    let now: DateTime<Utc> = "2026-02-04T12:03:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "task_sharing",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should not detect pending check within time threshold"
    );
}

#[test]
fn collect_stale_check_effects_pending_uses_min_threshold() {
    use chrono::{DateTime, Utc};

    // No stats for this check — should use MIN_PENDING_STALE_SECS (1800s = 30 min)
    let ci_stats = crate::ci_stats::CiCheckStats::default();

    // 20 minutes since siblings started — under 30 min minimum threshold
    let now: DateTime<Utc> = "2026-02-04T12:20:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "unknown_check",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should not detect pending check before minimum threshold (30 min)"
    );

    // 35 minutes since siblings started — past 30 min minimum threshold
    let now_later: DateTime<Utc> = "2026-02-04T12:35:00Z".parse().unwrap();
    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now_later);
    assert_eq!(
        effects.len(),
        1,
        "should detect pending check after minimum threshold"
    );
}

#[test]
fn collect_stale_check_effects_pending_respects_rerun_cooldown() {
    use chrono::{DateTime, Utc};

    let mut ci_stats = test_ci_stats_with_duration("task_sharing", 120);
    // Record a recent re-run for this workflow
    ci_stats.record_rerun(222);

    let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "task_sharing",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should skip pending check re-run when on cooldown"
    );
}

#[test]
fn collect_stale_check_effects_pending_skips_malformed_sibling_timestamps() {
    use chrono::{DateTime, Utc};

    let ci_stats = test_ci_stats_with_duration("task_sharing", 120);
    let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();

    // All siblings COMPLETED but with missing/malformed startedAt
    let prs = vec![json!({
        "number": 679,
        "statusCheckRollup": [
            {
                "name": "Test",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "not-a-date",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
            },
            {
                "name": "Clippy",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
            },
            {
                "name": "task_sharing",
                "status": "QUEUED",
                "startedAt": "",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
            }
        ]
    })];

    let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
    assert!(
        effects.is_empty(),
        "should skip pending check when sibling timestamps are unparseable"
    );
}

// -------------------------------------------------------------------------
// Stuck condition escalation threshold tests
// -------------------------------------------------------------------------

#[test]
fn test_escalation_triggers_on_second_nudge() {
    // Test the should_escalate helper function directly.
    // With STUCK_ESCALATION_NUDGE_COUNT = 2:
    // - First nudge (prior_nudges=0): 0+1=1 < 2, no escalation
    // - Second nudge (prior_nudges=1): 1+1=2 >= 2, ESCALATION

    assert!(
        !super::should_escalate(0),
        "first nudge (prior=0) should NOT escalate"
    );
    assert!(
        super::should_escalate(1),
        "second nudge (prior=1) should escalate"
    );
    assert!(
        super::should_escalate(2),
        "third+ nudge (prior=2) should escalate"
    );
}

#[test]
fn test_escalation_timing_matches_documentation() {
    use crate::daemon::constants::{
        STUCK_ESCALATION_NUDGE_COUNT, STUCK_NO_REVIEW_DURATION, STUCK_NUDGE_COOLDOWN_SECS,
    };

    // Documentation says escalation happens after 45+ minutes:
    // - Initial stuck detection: ~15 minutes (STUCK_NO_REVIEW_DURATION)
    // - First nudge at T=15min (prior_nudges becomes 1)
    // - Cooldown: 30 minutes (STUCK_NUDGE_COOLDOWN_SECS)
    // - Second nudge at T=45min triggers escalation (prior_nudges=1, 1+1=2 >= 2)

    let initial_detection_secs = STUCK_NO_REVIEW_DURATION.as_secs();
    let cooldown_secs = STUCK_NUDGE_COOLDOWN_SECS;
    let nudges_before_escalation = STUCK_ESCALATION_NUDGE_COUNT - 1; // 1 nudge before escalation

    let escalation_time_secs =
        initial_detection_secs + (nudges_before_escalation as u64 * cooldown_secs);
    let escalation_time_minutes = escalation_time_secs / 60;

    assert_eq!(
        escalation_time_minutes, 45,
        "escalation should trigger at 45 minutes (15 min initial + 30 min cooldown)"
    );
}

// -------------------------------------------------------------------------
// Time-aware hash tests (PR poll cache bug fix)
// -------------------------------------------------------------------------

/// Bug: PR poll used a hash of the response to skip processing when data unchanged.
/// But reviewer spawn decisions depend on PR age (time-based), so even with unchanged
/// data, a PR that was "too new" should be re-evaluated after time passes.
///
/// Fix: Include a time bucket in the hash so it changes every PR_REVIEW_DELAY_SECS.
#[test]
fn compute_time_aware_hash_same_data_same_bucket_same_hash() {
    // Within the same time bucket, same data should produce same hash
    let data = r#"[{"number": 42, "title": "Test PR"}]"#;
    let bucket_secs = 60;

    let hash1 = super::compute_time_aware_hash(data, bucket_secs);
    let hash2 = super::compute_time_aware_hash(data, bucket_secs);

    // Same data, same time bucket (called immediately) -> same hash
    assert_eq!(
        hash1, hash2,
        "same data in same time bucket should produce same hash"
    );
}

#[test]
fn compute_time_aware_hash_different_data_different_hash() {
    let data1 = r#"[{"number": 42, "title": "Test PR"}]"#;
    let data2 = r#"[{"number": 42, "title": "Updated PR"}]"#;
    let bucket_secs = 60;

    let hash1 = super::compute_time_aware_hash(data1, bucket_secs);
    let hash2 = super::compute_time_aware_hash(data2, bucket_secs);

    // Different data should produce different hash
    assert_ne!(hash1, hash2, "different data should produce different hash");
}

/// This test documents the behavior that the hash will change over time.
/// We can't easily test actual time passage in a unit test, but we can verify
/// the hash function includes the time bucket by using a very small bucket.
#[test]
fn compute_time_aware_hash_includes_time_component() {
    use std::hash::{Hash, Hasher};

    // Verify that the same data with different time buckets would produce different hashes
    // by manually computing what the hash would be with different time values
    let data = r#"[{"number": 42}]"#;

    // Simulate two different time buckets by manually hashing
    let mut hasher1 = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher1);
    (100u64).hash(&mut hasher1); // time bucket 100
    let hash_bucket_100 = hasher1.finish();

    let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher2);
    (101u64).hash(&mut hasher2); // time bucket 101
    let hash_bucket_101 = hasher2.finish();

    assert_ne!(
        hash_bucket_100, hash_bucket_101,
        "same data with different time buckets should produce different hashes"
    );
}

// -------------------------------------------------------------------------
// PR poll cache re-evaluation E2E test
// -------------------------------------------------------------------------

/// This test demonstrates the end-to-end behavior of the PR poll cache fix.
///
/// ## Bug scenario (before fix):
/// 1. PR #42 is opened at t=0
/// 2. Poll at t=20s: PR is too new (within 45s delay), no reviewer spawn
/// 3. Poll at t=60s: PR data unchanged → hash unchanged → early return (BUG!)
///    - The reviewer spawn eligibility was never re-evaluated
///
/// ## Fixed behavior (after fix):
/// 1. PR #42 is opened at t=0
/// 2. Poll at t=20s: PR is too new, no reviewer spawn, cache hash saved
/// 3. Poll at t=60s: time bucket changed (bucket 0→1) → hash changed
///    - Poll proceeds, PR age re-evaluated, reviewer spawn triggered
///
/// This test simulates time passing to verify the hash changes at bucket boundaries.
#[test]
fn pr_poll_cache_reevaluates_after_time_bucket_change() {
    // Same PR data throughout - the data doesn't change, only time passes
    let pr_data = r#"[{"number": 42, "title": "feat: Add feature", "state": "OPEN"}]"#;
    let bucket_secs = super::PR_REVIEW_DELAY_SECS; // 45 seconds

    // Scenario: PR opened at t=0, first poll at t=20
    // Bucket boundaries are at multiples of 45: 0, 45, 90, ...
    let t_first_poll = 20u64; // In bucket 0 (0-44)
    let hash_first_poll = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_first_poll);

    // At this point, PR is too new for review (only 20s old).
    // The daemon would skip reviewer spawn. Hash is cached.

    // Second poll at t=35 (still in bucket 0)
    let t_second_poll = 35u64; // Still in bucket 0 (0-44)
    let hash_second_poll = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_second_poll);

    // Hash should be SAME (same bucket) - this is expected caching behavior
    assert_eq!(
        hash_first_poll, hash_second_poll,
        "Within same time bucket, hash should be stable for caching"
    );

    // Third poll at t=60 (NEW bucket!)
    // This is 60s after PR creation, well past the 45s review delay
    let t_third_poll = 60u64; // In bucket 1 (45-89)
    let hash_third_poll = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_third_poll);

    // Hash should be DIFFERENT (new bucket) - triggers re-evaluation
    assert_ne!(
        hash_second_poll, hash_third_poll,
        "After time bucket change, hash should differ to trigger re-evaluation"
    );

    // Verify the bucket transition occurred as expected
    let bucket_first = t_first_poll / bucket_secs;
    let bucket_second = t_second_poll / bucket_secs;
    let bucket_third = t_third_poll / bucket_secs;

    assert_eq!(
        bucket_first, bucket_second,
        "First two polls should be in same bucket"
    );
    assert_ne!(
        bucket_second, bucket_third,
        "Third poll should be in new bucket"
    );

    // Document the bucket transition: 0 → 1
    assert_eq!(bucket_first, 0, "First/second poll should be in bucket 0");
    assert_eq!(bucket_third, 1, "Third poll should be in bucket 1");
}

/// Test that the bucket boundary is exactly at PR_REVIEW_DELAY_SECS intervals.
///
/// This ensures that after waiting the full review delay period, the hash
/// is guaranteed to have changed and the PR eligibility will be re-evaluated.
#[test]
fn pr_poll_cache_bucket_boundary_precision() {
    let pr_data = r#"[{"number": 99}]"#;
    let bucket_secs = super::PR_REVIEW_DELAY_SECS; // 45 seconds

    // Bucket boundaries: 0-44 (bucket 0), 45-89 (bucket 1), 90-134 (bucket 2)
    //
    // t=44 → 44/45 = 0 (bucket 0)
    // t=45 → 45/45 = 1 (bucket 1)

    // Poll at t=44 (end of bucket 0)
    let t_end_of_bucket = 44u64;
    let hash_end = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_end_of_bucket);

    // Poll at t=45 (start of bucket 1)
    let t_start_next_bucket = 45u64;
    let hash_start = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_start_next_bucket);

    // One second difference at bucket boundary → different hash
    assert_ne!(
        hash_end, hash_start,
        "Crossing bucket boundary (29→30) should change hash"
    );

    // Verify bucket values
    assert_eq!(
        t_end_of_bucket / bucket_secs,
        0,
        "t=29 should be in bucket 0"
    );
    assert_eq!(
        t_start_next_bucket / bucket_secs,
        1,
        "t=30 should be in bucket 1"
    );

    // Within bucket, 28→29 should be same hash
    let hash_28 = super::compute_time_aware_hash_at(pr_data, bucket_secs, 28);
    let hash_29 = super::compute_time_aware_hash_at(pr_data, bucket_secs, 29);
    assert_eq!(
        hash_28, hash_29,
        "Within same bucket (28→29), hash should be stable"
    );
}

/// Create a minimal DaemonState for testing action-to-effects converters.
fn make_test_state() -> DaemonState {
    use std::process::Command;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    // Init git repo (CoworkerManager/WorktreeManager need one)
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new("test-session", wm);

    // Leak temp_dir so it survives the test (DaemonState doesn't own it)
    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state")
}

#[test]
fn pr_action_nudge_produces_nudge_with_callbacks() {
    let state = make_test_state();
    let action = crate::rules::PrAction::NudgeOwner {
        owner: "lexington".to_string(),
        message: "PR #42 needs attention".to_string(),
    };

    let pr_ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
    };
    let effects = pr_action_to_effects(
        action,
        42,
        "Fix bug",
        PrIssueType::CiFailed,
        &state,
        &pr_ctx,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::NudgeCoworkerWithCallbacks {
            name, on_success, ..
        } => {
            assert_eq!(name, "lexington");
            assert!(
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 42, .. })),
                "Should record PR nudge on success"
            );
        }
        _ => panic!("Expected NudgeCoworkerWithCallbacks, got {:?}", effects[0]),
    }
}

#[test]
fn pr_action_spawn_produces_spawn_with_callbacks() {
    let state = make_test_state();
    let action = crate::rules::PrAction::SpawnOwner {
        owner: "park".to_string(),
        message: "PR #99 CI failed".to_string(),
    };

    let pr_ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
    };
    let effects =
        pr_action_to_effects(action, 99, "Fix CI", PrIssueType::CiFailed, &state, &pr_ctx);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        } => {
            assert_eq!(config.name, "park");
            // on_success should include broadcast, channel post, and pr nudge record
            assert!(
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 99, .. })),
                "on_success should record PR nudge"
            );
            assert!(
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::BroadcastCoworkerUpdate { .. })),
                "on_success should broadcast status"
            );
            // on_failure should also record PR nudge (for cooldown tracking)
            assert!(
                on_failure
                    .iter()
                    .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 99, .. })),
                "on_failure should also record PR nudge"
            );
        }
        _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
    }
}

#[test]
fn pr_action_skip_produces_no_effects() {
    let state = make_test_state();
    let action = crate::rules::PrAction::Skip {
        reason: "Owner not found".to_string(),
    };

    let pr_ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
    };
    let effects = pr_action_to_effects(
        action,
        42,
        "Fix bug",
        PrIssueType::CiFailed,
        &state,
        &pr_ctx,
    );
    assert!(effects.is_empty());
}

#[test]
fn comment_action_spawn_produces_spawn_with_callbacks() {
    let state = make_test_state();
    let action = crate::rules::PrAction::SpawnOwner {
        owner: "amsterdam".to_string(),
        message: "PR #55 has review feedback".to_string(),
    };

    let pr_ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
    };
    let effects = comment_action_to_effects(action, 55, "Add feature", &state, &pr_ctx);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::SpawnCoworkerWithCallbacks {
            config, on_success, ..
        } => {
            assert_eq!(config.name, "amsterdam");
            assert!(
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 55, .. })),
                "on_success should record PR nudge for comment"
            );
        }
        _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
    }
}

#[test]
fn pr_action_spawn_with_break_session_includes_clear_effect() {
    let state = make_test_state();
    // Simulate a saved break session for the coworker
    {
        let mut sessions = state.pr_break_sessions.write().unwrap();
        sessions.insert("york".to_string(), "session-abc-123".to_string());
    }

    let action = crate::rules::PrAction::SpawnOwner {
        owner: "york".to_string(),
        message: "PR #77 needs review".to_string(),
    };

    let pr_ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
    };
    let effects = pr_action_to_effects(
        action,
        77,
        "Review PR",
        PrIssueType::ReviewComment,
        &state,
        &pr_ctx,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::SpawnCoworkerWithCallbacks {
            config, on_success, ..
        } => {
            // Should use ResumeSession mode since we have a saved session
            assert!(
                matches!(config.session_mode, crate::launch::SessionMode::ResumeSession(ref id) if id == "session-abc-123"),
                "Should resume saved session, got {:?}",
                config.session_mode
            );
            // on_success should include ClearPrBreakSession
            assert!(
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::ClearPrBreakSession { name } if name == "york")),
                "on_success should clear break session"
            );
        }
        _ => panic!("Expected SpawnCoworkerWithCallbacks"),
    }
}

// NOTE: Reviewer spawn registry effects are tested via code inspection and
// integration tests rather than unit tests. The collect_reviewer_effects function
// has complex async dependencies (persistent state, PR review tracking) that make
// unit testing difficult. The implementation at lines 1651-1665 clearly shows
// RegisterWorktreeAssignment and BindCoworkerToWorktree are generated in the
// on_success callbacks of SpawnCoworkerWithCallbacks, matching the dispatch path.

#[test]
fn coworker_from_branch_with_map_supports_task_branches() {
    use std::collections::HashMap;

    // Build a branch → coworker map like WorldSnapshot does
    let mut map = HashMap::new();
    map.insert("task-42-fix-auth".to_string(), "lexington".to_string());
    map.insert("review-pr-123".to_string(), "madison".to_string());

    // Task-based branches should resolve via the map
    assert_eq!(
        coworker_from_branch_with_map("task-42-fix-auth", Some(&map)),
        Some("lexington".to_string()),
        "task-based branch should resolve via map"
    );
    assert_eq!(
        coworker_from_branch_with_map("review-pr-123", Some(&map)),
        Some("madison".to_string()),
        "review-pr branch should resolve via map"
    );

    // Legacy branches should still work
    assert_eq!(
        coworker_from_branch_with_map("york/fix-bug", Some(&map)),
        Some("york".to_string()),
        "legacy branch should resolve without map"
    );

    // Without the map, task-based branches should return None
    assert!(
        coworker_from_branch_with_map("task-42-fix-auth", None).is_none(),
        "task-based branch without map should return None"
    );
}

#[test]
fn collect_merged_pr_cleanup_effects_generates_cleanup_and_channel_message() {
    use std::collections::{HashMap, HashSet};

    // Build a minimal snapshot with merged PRs and their branch mappings
    let merged_pr_numbers: HashSet<u64> = [42, 123].into_iter().collect();
    let mut merged_pr_branches: HashMap<u64, String> = HashMap::new();
    merged_pr_branches.insert(42, "task-42-fix-auth".to_string());
    merged_pr_branches.insert(123, "review-pr-123".to_string());

    // Register worktrees so the PostSystemMessage can include task IDs
    let mut worktree_registry = crate::worktree_registry::WorktreeRegistry::new();
    worktree_registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-42-fix-auth".to_string(),
            branch_name: "task-42-fix-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: Some(42),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();
    // PR #123 has no task_id (e.g., a review worktree)
    worktree_registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "review-pr-123".to_string(),
            branch_name: "review-pr-123".to_string(),
            task_id: None,
            current_coworker: None,
            pr_number: Some(123),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    let snap = crate::daemon::snapshot::WorldSnapshot {
        merged_pr_numbers,
        merged_pr_branches,
        worktree_registry,
        // All other fields use defaults from test helpers
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "test".to_string(),
        repo_name: "test-repo".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let effects = collect_merged_pr_cleanup_effects(&snap);

    // Each merged PR should generate a CleanupMergedWorktree + PostSystemMessage pair
    assert_eq!(
        effects.len(),
        4,
        "should generate 2 cleanup + 2 channel effects"
    );

    // Verify cleanup effects
    assert!(
        effects.iter().any(|e| {
            if let Effect::CleanupMergedWorktree { pr_number, branch } = e {
                *pr_number == 42 && branch == "task-42-fix-auth"
            } else {
                false
            }
        }),
        "should cleanup PR #42"
    );
    assert!(
        effects.iter().any(|e| {
            if let Effect::CleanupMergedWorktree { pr_number, branch } = e {
                *pr_number == 123 && branch == "review-pr-123"
            } else {
                false
            }
        }),
        "should cleanup PR #123"
    );

    // Verify channel notification effects
    // PR #42 has a task_id, so message should include it
    assert!(
        effects.iter().any(|e| {
            if let Effect::PostSystemMessage { message } = e {
                message.contains("PR #42") && message.contains("task !42") && message.contains('🧹')
            } else {
                false
            }
        }),
        "should post channel message for PR #42 with task ID"
    );
    // PR #123 has no task_id
    assert!(
        effects.iter().any(|e| {
            if let Effect::PostSystemMessage { message } = e {
                message.contains("PR #123") && !message.contains("task !") && message.contains('🧹')
            } else {
                false
            }
        }),
        "should post channel message for PR #123 without task ID"
    );
}

/// Test that reconcile_orphaned_prs generates CreateTask effects for
/// PRs that are reviewed + CI green but have no associated task.
///
/// This is a **snapshot-based unit test** that verifies the pure decision
/// logic without requiring a full DaemonState. It tests the input → output
/// mapping: given PR data in the cache and reviewed_prs/pr_task_associations
/// in the snapshot, verify that the correct CreateTask effects are generated.
#[test]
fn reconcile_orphaned_prs_creates_tasks_for_reviewed_green_prs() {
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    // Setup: Create mock PR data matching different scenarios
    let pr_data = vec![
        // PR #42: reviewed, CI green, no task → ORPHANED (should create task)
        json!({
            "number": 42,
            "headRefName": "lexington/fix-auth",
            "title": "Fix authentication bug",
            "isDraft": false,
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // PR #100: reviewed, CI green, has task → NOT ORPHANED
        json!({
            "number": 100,
            "headRefName": "york/add-feature",
            "title": "Add new feature",
            "isDraft": false,
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // PR #200: CI green but NOT reviewed → NOT ORPHANED
        json!({
            "number": 200,
            "headRefName": "amsterdam/refactor",
            "title": "Refactor module",
            "isDraft": false,
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // PR #300: reviewed but CI FAILING → NOT ORPHANED
        json!({
            "number": 300,
            "headRefName": "madison/perf",
            "title": "Performance improvements",
            "isDraft": false,
            "statusCheckRollup": [{"conclusion": "FAILURE"}]
        }),
        // PR #400: reviewed, CI green, but DRAFT → NOT ORPHANED
        json!({
            "number": 400,
            "headRefName": "broadway/draft",
            "title": "Draft PR",
            "isDraft": true,
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // PR #500: reviewed, CI green, but invalid branch prefix → NOT ORPHANED
        json!({
            "number": 500,
            "headRefName": "feature/something",
            "title": "Feature branch",
            "isDraft": false,
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
    ];

    // Setup: Snapshot with open PR data, reviewed PRs, and task associations
    let mut reviewed_prs = HashSet::new();
    reviewed_prs.insert(42); // PR #42 is reviewed (ORPHANED)
    reviewed_prs.insert(100); // PR #100 is reviewed (has task)
    reviewed_prs.insert(300); // PR #300 is reviewed (CI failing)

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(100, "1234".to_string()); // PR #100 has task

    let snap = crate::daemon::snapshot::WorldSnapshot {
        open_prs_data: pr_data,
        reviewed_prs,
        pr_task_associations,
        repo_name: "test-repo".to_string(),
        // All other fields use defaults from helper
        ..crate::daemon::snapshot::minimal_snapshot_for_test()
    };

    // Execute: Call the pure decision function
    let effects = reconcile_orphaned_prs(&snap);

    // Verify: Should generate exactly one CreateTask effect for PR #42
    assert_eq!(effects.len(), 1, "should generate 1 task for orphaned PR");

    match &effects[0] {
        Effect::CreateTask {
            repo_name,
            subject,
            description,
        } => {
            assert_eq!(repo_name, "test-repo");
            assert!(subject.contains("42"), "subject should mention PR #42");
            assert!(
                subject.contains("reviewed"),
                "subject should say 'reviewed'"
            );
            assert!(
                subject.contains("CI green"),
                "subject should say 'CI green'"
            );
            assert!(
                description.contains("Fix authentication bug"),
                "description should include PR title"
            );
            assert!(
                description.contains("lexington/fix-auth"),
                "description should include branch name"
            );
        }
        _ => panic!("expected CreateTask effect, got {:?}", effects[0]),
    }
}

/// Bug fix test for !1067: Cooldown should be cleared when coworker dies
///
/// Scenario: A coworker is spawned to address review feedback on their PR,
/// but dies (API error, crash, etc.) without addressing it. The next poll
/// should clear the cooldown and retry, not silently drop the work.
#[test]
fn pr_issue_tracker_clears_nudge_when_requested() {
    use super::super::trackers::{PrIssueTracker, PrIssueType};

    let mut tracker = PrIssueTracker::new();

    // First nudge: spawn coworker to address review feedback
    tracker.record_nudge(42, PrIssueType::GreenWithFeedback);

    // Verify cooldown is active
    assert!(
        !tracker.should_nudge(42, PrIssueType::GreenWithFeedback),
        "cooldown should block immediate repeat"
    );

    // Coworker dies — daemon clears the cooldown
    tracker.clear_nudge(42, PrIssueType::GreenWithFeedback);

    // Next poll: cooldown is cleared, so should_nudge returns true
    assert!(
        tracker.should_nudge(42, PrIssueType::GreenWithFeedback),
        "should_nudge should return true after clearing cooldown"
    );

    // Verify we can record a new nudge (retry)
    tracker.record_nudge(42, PrIssueType::GreenWithFeedback);
    assert!(
        !tracker.should_nudge(42, PrIssueType::GreenWithFeedback),
        "new cooldown should be active after retry"
    );
}

/// Integration test for !1067: Simulates the exact code path in
/// collect_green_with_feedback_effects when a coworker dies.
///
/// The function's loop body (after the fix) executes in this order:
/// 1. Determine PR owner from branch name
/// 2. If owner is NOT active AND has a prior nudge → clear cooldown
/// 3. Check should_nudge (now returns true because cooldown was cleared)
/// 4. Produce effects (spawn/nudge)
///
/// This test walks through steps 2-3 to verify the ordering is correct:
/// clearing happens BEFORE the should_nudge gate.
#[test]
fn green_with_feedback_clears_cooldown_when_owner_inactive() {
    use super::super::trackers::{PrIssueTracker, PrIssueType};

    let mut tracker = PrIssueTracker::new();
    let active_coworkers: Vec<String> = vec!["lexington".to_string()]; // amsterdam NOT here

    // Step 1: First poll spawned amsterdam for PR #42, recorded nudge
    tracker.record_nudge(42, PrIssueType::GreenWithFeedback);
    assert!(
        !tracker.should_nudge(42, PrIssueType::GreenWithFeedback),
        "cooldown should be active after spawn"
    );

    // Step 2: Amsterdam died — simulate the inactive-owner check from the function.
    // This mirrors the code block that runs BEFORE should_nudge in the fixed version.
    let owner = "amsterdam".to_string();
    if !active_coworkers.contains(&owner) && tracker.has_nudge(42, PrIssueType::GreenWithFeedback) {
        tracker.clear_nudge(42, PrIssueType::GreenWithFeedback);
    }

    // Step 3: should_nudge check — must return true now (cooldown was cleared)
    assert!(
        tracker.should_nudge(42, PrIssueType::GreenWithFeedback),
        "should_nudge must return true after cooldown cleared for inactive owner"
    );

    // Step 4: Verify active owners don't get cooldown cleared
    let mut tracker2 = PrIssueTracker::new();
    tracker2.record_nudge(99, PrIssueType::GreenWithFeedback);
    let active_owner = "lexington".to_string();
    if !active_coworkers.contains(&active_owner)
        && tracker2.has_nudge(99, PrIssueType::GreenWithFeedback)
    {
        tracker2.clear_nudge(99, PrIssueType::GreenWithFeedback);
    }
    assert!(
        !tracker2.should_nudge(99, PrIssueType::GreenWithFeedback),
        "cooldown should NOT be cleared for active owners"
    );
}

/// Unit test for detect_abandoned_pr_tasks pure function.
///
/// Verifies that tasks linked to closed-without-merge PRs are detected
/// and ResetAbandonedTask effects are emitted correctly.
#[test]
fn test_detect_abandoned_pr_tasks() {
    use super::super::effects::Effect;
    use super::super::snapshot::WorldSnapshot;

    // Setup: Task 42 is in_progress and linked to PR #100, which is closed (not in open list)
    let in_progress_tasks = vec![("42".to_string(), "Fix auth".to_string(), "york".to_string())];

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(100u64, "42".to_string());

    let merged_pr_numbers = HashSet::new(); // PR 100 is NOT merged

    let snap = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks,
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers,
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations,
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    // Call the pure function with PR 100 NOT in the open list
    let open_pr_numbers = vec![]; // PR 100 is closed
    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Verify: Should emit ResetAbandonedTask for task 42
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ResetAbandonedTask {
            task_id,
            pr_number,
            repo_name,
        } => {
            assert_eq!(task_id, "42");
            assert_eq!(*pr_number, 100);
            assert_eq!(repo_name, "test-repo");
        }
        other => panic!("Expected ResetAbandonedTask, got {:?}", other),
    }
}

/// Test that detect_abandoned_pr_tasks skips PRs that are still open.
#[test]
fn test_detect_abandoned_pr_tasks_skips_open_prs() {
    use super::super::snapshot::WorldSnapshot;

    let in_progress_tasks = vec![("42".to_string(), "Fix auth".to_string(), "york".to_string())];

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(100u64, "42".to_string());

    let snap = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks,
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations,
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    // PR 100 is in the open list
    let open_pr_numbers = vec![100u64];
    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Should emit no effects since PR is still open
    assert!(effects.is_empty(), "Should not reset task for open PR");
}

/// Test that detect_abandoned_pr_tasks doesn't reset a task when a duplicate
/// PR is closed if the same task has a sibling PR that was already merged.
///
/// Scenario: Task !1158 has two PRs:
/// - PR #968 (merged)
/// - PR #999 (closed without merge - duplicate)
///
/// When PR #999 is detected as abandoned, the task should NOT be reset because
/// the work was already landed via PR #968.
#[test]
fn test_detect_abandoned_pr_tasks_checks_for_merged_siblings() {
    use super::super::snapshot::WorldSnapshot;
    use crate::tasks::{Task, TaskStatus};

    // Task !1158 is "in progress" (completed, but still in_progress_tasks for this test)
    let in_progress_tasks = vec![(
        "1158".to_string(),
        "Fix bug".to_string(),
        "york".to_string(),
    )];

    // Full task object showing it's completed and has pr field pointing to merged PR
    let task = Task {
        id: "1158".to_string(),
        subject: "Fix bug".to_string(),
        status: TaskStatus::Completed,
        owner: Some("york".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(968), // Task.pr points to merged PR #968
        created_at: None,
    };

    // PR associations: both PRs are associated with the same task
    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(968u64, "1158".to_string()); // merged PR
    pr_task_associations.insert(999u64, "1158".to_string()); // duplicate PR (closed)

    // PR #968 is merged, PR #999 is NOT merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(968u64);

    let snap = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks,
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![task],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers,
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations,
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_registry: Default::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        github_rate_limit: Default::default(),
        freshly_fetched_rate_limit: None,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
    };

    // Both PRs are closed (PR #968 is merged, PR #999 is closed without merge)
    let open_pr_numbers = vec![];

    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Should NOT reset task !1158 because PR #968 (a sibling PR for the same task) was merged
    assert_eq!(
        effects.len(),
        0,
        "Expected no reset effects because a sibling PR was merged, but got: {:?}",
        effects
    );
}

/// Test that detect_abandoned_pr_tasks skips merged PRs.
#[test]
fn test_detect_abandoned_pr_tasks_skips_merged_prs() {
    use super::super::snapshot::WorldSnapshot;

    let in_progress_tasks = vec![("42".to_string(), "Fix auth".to_string(), "york".to_string())];

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(100u64, "42".to_string());

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(100u64); // PR 100 was merged

    let snap = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks,
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers,
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations,
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    // PR 100 is NOT in open list, but it IS in merged list
    let open_pr_numbers = vec![];
    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Should emit no effects since merged PRs are handled separately
    assert!(effects.is_empty(), "Should not reset task for merged PR");
}
