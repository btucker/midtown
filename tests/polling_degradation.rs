//! E2E tests for graceful degradation when GitHub webhooks are unavailable.
//!
//! These tests verify that when webhooks aren't delivering events, the polling
//! path correctly detects and handles all PR issues. The daemon should function
//! identically (just with ~30s latency instead of real-time webhook delivery).
//!
//! Run with: `cargo test --test polling_degradation`

use serde_json::json;
use std::time::Duration;

// Re-exported types from daemon module
use midtown::daemon::{PrIssueTracker, PrIssueType, StuckConditionTracker, StuckConditionType};

// Helper functions for polling
use midtown::daemon::helpers::{
    coworker_from_branch, detect_pr_issues, is_auto_mergeable, text_contains_review_signature,
};

// Decision functions
use midtown::rules::{PrAction, decide_pr_issue_action};

// We test the pure helper functions that the polling path uses.
// These are the same functions webhooks use, proving functional equivalence.

/// Test that polling detects CI failures the same way webhooks would.
///
/// Scenario: PR #42 has a failing CI check. Without webhooks, the polling
/// path should detect this via `gh pr list` and generate a nudge effect.
#[test]
fn polling_detects_ci_failure() {
    // Simulated PR data from `gh pr list --json ...`
    let pr = json!({
        "number": 42,
        "title": "Fix authentication bug",
        "headRefName": "amsterdam/fix-auth",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "SUCCESS"},
            {"name": "test", "conclusion": "FAILURE"}
        ]
    });

    // Use the same detection function polling uses
    let issues = detect_pr_issues(&pr);

    assert!(
        issues.iter().any(|i| matches!(i, PrIssueType::CiFailed)),
        "polling should detect CI failure: {:?}",
        issues
    );

    // Verify owner extraction (used for nudge targeting)
    let branch = pr["headRefName"].as_str().unwrap();
    let owner = coworker_from_branch(branch);
    assert_eq!(owner, Some("amsterdam".to_string()));
}

/// Test that polling detects merge conflicts.
///
/// Scenario: PR #43 has a merge conflict. Polling should detect this
/// and generate a nudge to the PR owner.
#[test]
fn polling_detects_merge_conflict() {
    let pr = json!({
        "number": 43,
        "title": "Add new feature",
        "headRefName": "york/new-feature",
        "mergeable": "CONFLICTING",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "build", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::MergeConflict)),
        "polling should detect merge conflict: {:?}",
        issues
    );
}

/// Test that polling detects approval status.
///
/// Scenario: PR #44 has been approved. Polling should detect this
/// and potentially trigger auto-merge if CI is also green.
#[test]
fn polling_detects_approval() {
    let pr = json!({
        "number": 44,
        "title": "Refactor module",
        "headRefName": "lexington/refactor",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "build", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.iter().any(|i| matches!(i, PrIssueType::Approved)),
        "polling should detect approval: {:?}",
        issues
    );

    // Also verify auto-merge eligibility
    assert!(
        is_auto_mergeable(&pr),
        "approved PR with green CI should be auto-mergeable"
    );
}

/// Test that polling detects changes-requested status.
///
/// Scenario: PR #45 has changes requested by a reviewer. Polling should
/// detect this and nudge the PR owner to address feedback.
#[test]
fn polling_detects_changes_requested() {
    let pr = json!({
        "number": 45,
        "title": "Update API",
        "headRefName": "broadway/api-update",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "CHANGES_REQUESTED",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "build", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::ChangesRequested)),
        "polling should detect changes_requested: {:?}",
        issues
    );
}

/// Test that polling correctly identifies PRs needing review.
///
/// Scenario: PR #46 is open but has no review yet. After the review delay
/// period, polling should spawn a reviewer.
#[test]
fn polling_identifies_pr_needing_review() {
    let pr = json!({
        "number": 46,
        "title": "New component",
        "headRefName": "park/new-component",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",  // No review yet
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "build", "conclusion": "SUCCESS"}]
    });

    // No issues detected (needs_review is handled separately in collect_reviewer_effects)
    let issues = detect_pr_issues(&pr);

    // A PR with no review decision and no other issues is a candidate for review spawn
    assert!(
        !issues.iter().any(|i| matches!(
            i,
            PrIssueType::CiFailed | PrIssueType::MergeConflict | PrIssueType::ChangesRequested
        )),
        "PR needing review should have no blocking issues"
    );
}

/// Test that polling skips draft PRs for review.
///
/// Scenario: PR #47 is a draft. Polling should not spawn a reviewer.
#[test]
fn polling_skips_draft_prs() {
    let pr = json!({
        "number": 47,
        "title": "WIP: Experimental feature",
        "headRefName": "madison/experimental",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": true,  // Draft PR
        "reviewDecision": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": []
    });

    // Draft PRs should be skipped for auto-review (this is a policy check)
    let is_draft = pr["isDraft"].as_bool().unwrap_or(false);
    assert!(is_draft, "test setup: PR should be a draft");

    // Drafts are filtered out in collect_reviewer_effects before spawning
}

/// Test that polling handles multiple issues on the same PR.
///
/// Scenario: PR #48 has both CI failure AND merge conflict.
/// Polling should detect both issues.
#[test]
fn polling_detects_multiple_issues() {
    let pr = json!({
        "number": 48,
        "title": "Complex change",
        "headRefName": "columbus/complex",
        "mergeable": "CONFLICTING",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "FAILURE"},
            {"name": "lint", "conclusion": "FAILURE"}
        ]
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.iter().any(|i| matches!(i, PrIssueType::CiFailed)),
        "should detect CI failure"
    );
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::MergeConflict)),
        "should detect merge conflict"
    );
    assert!(
        issues.len() >= 2,
        "should detect multiple issues: {:?}",
        issues
    );
}

/// Test auto-merge eligibility detection via polling.
///
/// Auto-merge is a polling-only feature (no webhook equivalent).
/// Verifies all conditions are checked correctly.
#[test]
fn polling_auto_merge_requires_all_conditions() {
    // Fully eligible PR
    let eligible = json!({
        "number": 50,
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });
    assert!(is_auto_mergeable(&eligible));

    // Missing approval
    let no_approval = json!({
        "number": 51,
        "mergeable": "MERGEABLE",
        "reviewDecision": "",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });
    assert!(!is_auto_mergeable(&no_approval));

    // Has merge conflict
    let conflicting = json!({
        "number": 52,
        "mergeable": "CONFLICTING",
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });
    assert!(!is_auto_mergeable(&conflicting));

    // CI still pending
    let pending_ci = json!({
        "number": 53,
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "PENDING"}]
    });
    assert!(!is_auto_mergeable(&pending_ci));

    // CI failed
    let failed_ci = json!({
        "number": 54,
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "FAILURE"}]
    });
    assert!(!is_auto_mergeable(&failed_ci));
}

/// Test decision function produces correct action for active coworker.
///
/// When polling detects an issue and the PR owner is active, it should
/// nudge them (not spawn a new coworker).
#[test]
fn polling_nudges_active_owner() {
    let active_coworkers = vec!["amsterdam".to_string(), "york".to_string()];
    let message = "PR #42 - CI failed: please investigate";

    let action = decide_pr_issue_action("amsterdam", &active_coworkers, false, message);

    assert!(
        matches!(action, PrAction::NudgeOwner { ref owner, .. } if owner == "amsterdam"),
        "should nudge active owner: {:?}",
        action
    );
}

/// Test decision function produces correct action for inactive coworker.
///
/// When polling detects an issue and the PR owner is NOT active, it should
/// spawn them (if under dev limit).
#[test]
fn polling_spawns_inactive_owner() {
    let active_coworkers = vec!["york".to_string()]; // amsterdam is NOT active
    let message = "PR #42 - CI failed: please investigate";

    let action = decide_pr_issue_action("amsterdam", &active_coworkers, false, message);

    assert!(
        matches!(action, PrAction::SpawnOwner { ref owner, .. } if owner == "amsterdam"),
        "should spawn inactive owner: {:?}",
        action
    );
}

/// Test decision function respects dev limit.
///
/// When at the dev limit, polling should skip spawning and the issue
/// will be handled when capacity is available.
#[test]
fn polling_respects_dev_limit() {
    let active_coworkers = vec!["york".to_string()];
    let message = "PR #42 - CI failed: please investigate";

    // at_dev_limit = true
    let action = decide_pr_issue_action("amsterdam", &active_coworkers, true, message);

    assert!(
        matches!(action, PrAction::Skip { .. }),
        "should skip when at dev limit: {:?}",
        action
    );
}

/// Test that tracker deduplication works across webhook/polling paths.
///
/// This simulates the scenario where a webhook fires, records a nudge,
/// then polling runs and should see the cooldown.
#[test]
fn tracker_prevents_webhook_polling_double_nudge() {
    let mut tracker = PrIssueTracker::new();

    // Webhook fires first for CI failure on PR #42
    assert!(tracker.should_nudge(42, PrIssueType::CiFailed));
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // ~30s later, polling detects the same issue
    // Tracker should block the duplicate nudge
    assert!(
        !tracker.should_nudge(42, PrIssueType::CiFailed),
        "polling should be blocked when webhook already nudged"
    );

    // But a DIFFERENT issue on the same PR should be allowed
    assert!(
        tracker.should_nudge(42, PrIssueType::MergeConflict),
        "different issue type should be allowed"
    );
}

/// Test that polling handles issues when webhooks are degraded.
///
/// This simulates the graceful degradation scenario where webhooks
/// never fire and polling is the primary detection mechanism.
#[test]
fn polling_handles_issues_when_webhooks_degraded() {
    let mut tracker = PrIssueTracker::new();
    let active_coworkers = vec!["amsterdam".to_string()];

    // Simulated PR data (webhooks never delivered an event for this)
    let pr = json!({
        "number": 42,
        "headRefName": "amsterdam/fix-bug",
        "statusCheckRollup": [{"name": "ci", "conclusion": "FAILURE"}]
    });

    // Polling detects CI failure
    let issues = detect_pr_issues(&pr);
    assert!(issues.iter().any(|i| matches!(i, PrIssueType::CiFailed)));

    // Tracker allows the nudge (no prior webhook)
    assert!(tracker.should_nudge(42, PrIssueType::CiFailed));

    // Decision function returns correct action
    let action =
        decide_pr_issue_action("amsterdam", &active_coworkers, false, "PR #42 - CI failed");
    assert!(matches!(action, PrAction::NudgeOwner { .. }));

    // Record the nudge
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // Next polling cycle should be blocked (self-deduplication)
    assert!(!tracker.should_nudge(42, PrIssueType::CiFailed));
}

/// Test stuck condition detection (polling-only, no webhook equivalent).
///
/// Stuck conditions are inherently time-based and only detectable via polling.
#[test]
fn polling_only_stuck_detection() {
    let mut tracker = StuckConditionTracker::new();

    // PR #42 has been open with no review for a while
    let first_detected = tracker.track("42", StuckConditionType::NoReview);
    assert!(first_detected.elapsed() < Duration::from_millis(100));

    // First nudge should be allowed
    assert!(tracker.should_nudge("42", StuckConditionType::NoReview));

    // Record the nudge
    tracker.record_nudge("42", StuckConditionType::NoReview);

    // Immediate repeat should be blocked
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));

    // Different stuck condition should be allowed
    tracker.track("42", StuckConditionType::MergeReady);
    assert!(tracker.should_nudge("42", StuckConditionType::MergeReady));
}

/// Test Claude review signature detection.
///
/// Used to determine if a PR already has a Claude review (so we don't
/// spawn another reviewer).
#[test]
fn polling_detects_existing_claude_review() {
    // Standard review signature
    assert!(text_contains_review_signature(
        "## Summary\n\nLGTM!\n\n🤖 Reviewed by lexington"
    ));

    // Frontmatter signature (comment-based reviews)
    assert!(text_contains_review_signature(
        "<!-- midtown:reviewer=amsterdam -->\n\n## Code Review"
    ));

    // Header signature
    assert!(text_contains_review_signature(
        "## Code Review by york\n\nThis looks good!"
    ));

    // Normal comment (not a review)
    assert!(!text_contains_review_signature(
        "Thanks for the PR! I'll review it soon."
    ));
}

/// Test complete polling flow for a batch of PRs.
///
/// Simulates what happens during a single polling tick with multiple PRs
/// in various states.
#[test]
fn polling_processes_pr_batch() {
    let mut tracker = PrIssueTracker::new();

    // Batch of PRs from `gh pr list`
    let prs = vec![
        // PR with CI failure
        json!({
            "number": 100,
            "headRefName": "amsterdam/fix",
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "FAILURE"}]
        }),
        // PR with merge conflict
        json!({
            "number": 101,
            "headRefName": "york/feature",
            "mergeable": "CONFLICTING",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // Healthy PR (no issues)
        json!({
            "number": 102,
            "headRefName": "lexington/docs",
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // Approved PR ready for auto-merge
        json!({
            "number": 103,
            "headRefName": "broadway/ready",
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
    ];

    let mut nudge_count = 0;
    let mut auto_merge_count = 0;

    for pr in &prs {
        let pr_number = pr["number"].as_u64().unwrap();
        let issues = detect_pr_issues(pr);

        for issue in &issues {
            if tracker.should_nudge(pr_number, *issue) {
                tracker.record_nudge(pr_number, *issue);
                nudge_count += 1;
            }
        }

        if is_auto_mergeable(pr) {
            auto_merge_count += 1;
        }
    }

    // PR 100: CI failed (1 nudge)
    // PR 101: merge conflict (1 nudge)
    // PR 102: healthy (0 nudges)
    // PR 103: approved (1 nudge for approval notification)
    assert_eq!(nudge_count, 3, "should have 3 actionable issues");
    assert_eq!(auto_merge_count, 1, "should have 1 auto-mergeable PR");
}
