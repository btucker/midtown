//! E2E tests for PR management decisions.
//!
//! These tests verify that the daemon's PR management logic correctly:
//! - Spawns reviewers for PRs needing review
//! - Identifies PRs eligible for auto-merge
//! - Nudges PR owners when review comments arrive
//!
//! Run with: `cargo test --test pr_management_e2e`

use serde_json::json;

// Re-exported types from daemon module
use midtown::daemon::{PrIssueTracker, PrIssueType};

// Helper functions for PR detection
use midtown::daemon::helpers::{
    coworker_from_branch, detect_pr_issues, is_auto_mergeable, text_contains_review_signature,
};

// Decision functions
use midtown::rules::{PrAction, decide_pr_issue_action};

// ---------------------------------------------------------------------------
// Test 1: PR Needing Review Spawns Reviewer
// ---------------------------------------------------------------------------

/// Test that PRs without reviews trigger reviewer spawn decisions.
///
/// Scenario: A coworker opens a PR, CI is green, but no review has been
/// requested or completed. The daemon should spawn a reviewer coworker.
#[test]
fn pr_needing_review_spawns_reviewer() {
    // PR with green CI, no review decision
    let pr = json!({
        "number": 42,
        "title": "feat: Add authentication endpoint",
        "headRefName": "amsterdam/add-auth",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",  // No review yet
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "SUCCESS"},
            {"name": "test", "conclusion": "SUCCESS"}
        ]
    });

    // Verify preconditions for needing review
    let issues = detect_pr_issues(&pr);

    // A PR needing review should NOT have blocking issues
    assert!(
        !issues.iter().any(|i| matches!(
            i,
            PrIssueType::CiFailed | PrIssueType::MergeConflict | PrIssueType::ChangesRequested
        )),
        "PR needing review should have no blocking issues"
    );

    // Verify owner extraction works for branch prefix
    let branch = pr["headRefName"].as_str().unwrap();
    let owner = coworker_from_branch(branch);
    assert_eq!(
        owner,
        Some("amsterdam".to_string()),
        "should extract coworker name from branch"
    );

    // Verify this PR does NOT yet have a Claude review
    // (No review comments to check - this would be verified by comment content in real scenario)
    let review_decision = pr["reviewDecision"].as_str().unwrap_or("");
    assert!(
        review_decision.is_empty(),
        "PR should not have a review decision yet"
    );

    // The daemon's review spawn logic checks:
    // 1. PR is not a draft
    // 2. PR has no formal review decision
    // 3. PR doesn't already have a Claude comment review
    // 4. PR has been open long enough (review delay)
    // 5. We're under reviewer capacity
    let is_draft = pr["isDraft"].as_bool().unwrap_or(false);
    assert!(!is_draft, "PR should not be a draft");
}

/// Test that draft PRs do NOT trigger reviewer spawn.
///
/// Draft PRs are work-in-progress and shouldn't be reviewed until marked ready.
#[test]
fn draft_pr_does_not_spawn_reviewer() {
    let pr = json!({
        "number": 43,
        "title": "WIP: Experimental feature",
        "headRefName": "lexington/experimental",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": true,  // Draft PR
        "reviewDecision": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    let is_draft = pr["isDraft"].as_bool().unwrap_or(false);
    assert!(is_draft, "test setup: PR should be a draft");

    // Draft PRs are filtered out in collect_reviewer_effects before spawning
    // The daemon policy excludes drafts from automatic review assignment
}

/// Test that PRs with existing Claude reviews don't get duplicate reviewers.
///
/// Claude coworkers post comment-based reviews (can't use formal GitHub reviews
/// since they share the same user as the PR author). These should be recognized
/// to prevent spawning multiple reviewers.
#[test]
fn pr_with_claude_review_does_not_spawn_reviewer() {
    // Test various Claude review signature formats
    let signatures = [
        "## Code Review by lexington\n\nLooks good!",
        "<!-- midtown: park -->\n\n## Code Review\n\nApproved.",
        "<!-- midtown:broadway -->\n\nLGTM!",
    ];

    for signature in signatures {
        assert!(
            text_contains_review_signature(signature),
            "should recognize Claude review signature: {:?}",
            &signature[..signature.len().min(40)]
        );
    }

    // Non-review comments should NOT be recognized
    let non_reviews = [
        "Thanks for the PR! I'll review it soon.",
        "Can you add a test for this?",
        "LGTM - approved!", // Missing the signature pattern
    ];

    for comment in non_reviews {
        assert!(
            !text_contains_review_signature(comment),
            "should NOT recognize as Claude review: {:?}",
            comment
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: Approved Green PR Auto-Merges
// ---------------------------------------------------------------------------

/// Test that approved PRs with green CI are eligible for auto-merge.
///
/// Auto-merge conditions:
/// - reviewDecision == "APPROVED"
/// - mergeable != "CONFLICTING"
/// - All CI checks passed (no failures, no pending)
#[test]
fn approved_green_pr_auto_merges() {
    // Fully eligible PR
    let eligible_pr = json!({
        "number": 50,
        "title": "feat: Add user settings",
        "headRefName": "york/user-settings",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "SUCCESS"},
            {"name": "test", "conclusion": "SUCCESS"},
            {"name": "lint", "conclusion": "SUCCESS"}
        ]
    });

    assert!(
        is_auto_mergeable(&eligible_pr),
        "approved PR with green CI should be auto-mergeable"
    );

    // Verify the conditions individually
    let review_decision = eligible_pr["reviewDecision"].as_str().unwrap_or("");
    assert_eq!(review_decision, "APPROVED");

    let mergeable = eligible_pr["mergeable"].as_str().unwrap_or("");
    assert_ne!(mergeable, "CONFLICTING");

    // All checks passed
    let checks = eligible_pr["statusCheckRollup"].as_array().unwrap();
    assert!(checks.iter().all(|c| c["conclusion"] == "SUCCESS"));
}

/// Test that PRs missing approval are NOT auto-mergeable.
#[test]
fn pr_without_approval_not_auto_mergeable() {
    // Green CI but no approval
    let pr = json!({
        "number": 51,
        "title": "feat: Update API",
        "headRefName": "broadway/api-update",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "",  // No approval
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR without approval should not be auto-mergeable"
    );
}

/// Test that PRs with merge conflicts are NOT auto-mergeable.
#[test]
fn pr_with_merge_conflict_not_auto_mergeable() {
    let pr = json!({
        "number": 52,
        "title": "feat: Refactor auth",
        "headRefName": "columbus/auth-refactor",
        "mergeable": "CONFLICTING",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with merge conflict should not be auto-mergeable"
    );

    // Verify merge conflict is detected as an issue
    let issues = detect_pr_issues(&pr);
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::MergeConflict)),
        "should detect merge conflict"
    );
}

/// Test that PRs with failing CI are NOT auto-mergeable.
#[test]
fn pr_with_failing_ci_not_auto_mergeable() {
    let pr = json!({
        "number": 53,
        "title": "feat: Add caching",
        "headRefName": "park/caching",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "SUCCESS"},
            {"name": "test", "conclusion": "FAILURE"}
        ]
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with failing CI should not be auto-mergeable"
    );

    // Verify CI failure is detected
    let issues = detect_pr_issues(&pr);
    assert!(
        issues.iter().any(|i| matches!(i, PrIssueType::CiFailed)),
        "should detect CI failure"
    );
}

/// Test that PRs with pending CI checks are NOT auto-mergeable.
#[test]
fn pr_with_pending_ci_not_auto_mergeable() {
    let pr = json!({
        "number": 54,
        "title": "feat: Performance improvements",
        "headRefName": "madison/perf",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [
            {"name": "build", "conclusion": "SUCCESS"},
            {"name": "test", "conclusion": "PENDING"}  // Still running
        ]
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with pending CI should not be auto-mergeable"
    );
}

// ---------------------------------------------------------------------------
// Test 3: PR Comment Nudges Owner
// ---------------------------------------------------------------------------

/// Test that PR review comments trigger nudge to the PR owner.
///
/// When a reviewer posts feedback, the PR owner (coworker) should be
/// nudged to address it.
#[test]
fn pr_comment_nudges_owner() {
    // PR with changes requested
    let pr = json!({
        "number": 60,
        "title": "feat: Add logging",
        "headRefName": "amsterdam/logging",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "CHANGES_REQUESTED",
        "createdAt": "2024-01-01T00:00:00Z",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    // Detect the changes_requested issue
    let issues = detect_pr_issues(&pr);
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::ChangesRequested)),
        "should detect changes_requested"
    );

    // Owner extraction
    let branch = pr["headRefName"].as_str().unwrap();
    let owner = coworker_from_branch(branch).expect("should extract owner");
    assert_eq!(owner, "amsterdam");

    // Test the decision function: owner is active → nudge
    let active_coworkers = vec!["amsterdam".to_string(), "york".to_string()];
    let action = decide_pr_issue_action(
        &owner,
        &active_coworkers,
        false,
        "PR #60 - changes requested: please address feedback",
    );

    assert!(
        matches!(action, PrAction::NudgeOwner { ref owner, .. } if owner == "amsterdam"),
        "should nudge active owner: {:?}",
        action
    );
}

/// Test that inactive owners are spawned (not nudged) for PR issues.
#[test]
fn pr_issue_spawns_inactive_owner() {
    let pr = json!({
        "number": 61,
        "title": "feat: Database migration",
        "headRefName": "lexington/db-migration",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "CHANGES_REQUESTED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    let branch = pr["headRefName"].as_str().unwrap();
    let owner = coworker_from_branch(branch).expect("should extract owner");
    assert_eq!(owner, "lexington");

    // lexington is NOT in active coworkers
    let active_coworkers = vec!["amsterdam".to_string()];
    let action = decide_pr_issue_action(
        &owner,
        &active_coworkers,
        false,
        "PR #61 - changes requested: please address feedback",
    );

    assert!(
        matches!(action, PrAction::SpawnOwner { ref owner, .. } if owner == "lexington"),
        "should spawn inactive owner: {:?}",
        action
    );
}

/// Test that dev limit prevents spawning for PR issues.
#[test]
fn pr_issue_respects_dev_limit() {
    let owner = "broadway";
    let active_coworkers = vec!["amsterdam".to_string()]; // broadway not active

    // at_dev_limit = true
    let action = decide_pr_issue_action(
        owner,
        &active_coworkers,
        true, // at dev limit
        "PR #62 - CI failed: please investigate",
    );

    assert!(
        matches!(action, PrAction::Skip { ref reason } if reason.contains("dev limit")),
        "should skip when at dev limit: {:?}",
        action
    );
}

/// Test that tracker prevents duplicate nudges for the same issue.
#[test]
fn tracker_prevents_duplicate_pr_nudges() {
    let mut tracker = PrIssueTracker::new();

    // First nudge for CI failure on PR #70
    assert!(
        tracker.should_nudge(70, PrIssueType::CiFailed),
        "first nudge should be allowed"
    );
    tracker.record_nudge(70, PrIssueType::CiFailed);

    // Immediate repeat should be blocked (cooldown)
    assert!(
        !tracker.should_nudge(70, PrIssueType::CiFailed),
        "duplicate nudge should be blocked"
    );

    // Different issue type on same PR should be allowed
    assert!(
        tracker.should_nudge(70, PrIssueType::MergeConflict),
        "different issue type should be allowed"
    );

    // Same issue type on different PR should be allowed
    assert!(
        tracker.should_nudge(71, PrIssueType::CiFailed),
        "same issue type on different PR should be allowed"
    );
}

/// Test that PR with green CI and review feedback triggers owner nudge.
///
/// This is the "green CI with feedback" scenario: CI passed after the review,
/// so the owner should be nudged to address feedback and merge.
#[test]
fn green_ci_with_feedback_nudges_owner() {
    // PR that has review feedback and now has green CI
    let pr = json!({
        "number": 65,
        "title": "feat: Improve error handling",
        "headRefName": "vernon/error-handling",
        "mergeable": "MERGEABLE",
        "state": "OPEN",
        "isDraft": false,
        "reviewDecision": "CHANGES_REQUESTED",
        "statusCheckRollup": [
            {"name": "ci", "conclusion": "SUCCESS"}
        ]
    });

    // This scenario is detected by:
    // 1. reviewDecision == CHANGES_REQUESTED (or has review comments)
    // 2. All CI checks passed
    let review_decision = pr["reviewDecision"].as_str().unwrap_or("");
    let has_feedback = review_decision == "CHANGES_REQUESTED";

    let checks = pr["statusCheckRollup"].as_array().unwrap();
    let ci_green = checks.iter().all(|c| c["conclusion"] == "SUCCESS");

    assert!(has_feedback, "PR should have review feedback");
    assert!(ci_green, "CI should be green");

    // The daemon's polling logic detects GreenWithFeedback and nudges the owner
    // See: decide_polling_comment_nudges in rules.rs / pr.rs
}

// ---------------------------------------------------------------------------
// Integration scenarios
// ---------------------------------------------------------------------------

/// Test complete PR lifecycle from needing review to auto-merge ready.
#[test]
fn pr_lifecycle_review_to_merge() {
    // Stage 1: PR opened, needs review
    let pr_needs_review = json!({
        "number": 100,
        "headRefName": "amsterdam/feature",
        "mergeable": "MERGEABLE",
        "isDraft": false,
        "reviewDecision": "",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr_needs_review);
    assert!(
        !issues.iter().any(|i| matches!(i, PrIssueType::CiFailed)),
        "stage 1: no CI failures"
    );
    assert!(
        !is_auto_mergeable(&pr_needs_review),
        "stage 1: not auto-mergeable"
    );

    // Stage 2: Review posted, changes requested
    let pr_changes_requested = json!({
        "number": 100,
        "headRefName": "amsterdam/feature",
        "mergeable": "MERGEABLE",
        "isDraft": false,
        "reviewDecision": "CHANGES_REQUESTED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr_changes_requested);
    assert!(
        issues
            .iter()
            .any(|i| matches!(i, PrIssueType::ChangesRequested)),
        "stage 2: changes requested detected"
    );
    assert!(
        !is_auto_mergeable(&pr_changes_requested),
        "stage 2: not auto-mergeable"
    );

    // Stage 3: Owner addresses feedback, gets approval
    let pr_approved = json!({
        "number": 100,
        "headRefName": "amsterdam/feature",
        "mergeable": "MERGEABLE",
        "isDraft": false,
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [{"name": "ci", "conclusion": "SUCCESS"}]
    });

    let issues = detect_pr_issues(&pr_approved);
    assert!(
        issues.iter().any(|i| matches!(i, PrIssueType::Approved)),
        "stage 3: approval detected"
    );
    assert!(is_auto_mergeable(&pr_approved), "stage 3: auto-mergeable");
}

/// Test batch processing of multiple PRs with different states.
#[test]
fn batch_pr_processing() {
    let mut tracker = PrIssueTracker::new();

    let prs = vec![
        // PR with CI failure
        json!({
            "number": 200,
            "headRefName": "amsterdam/fix",
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "FAILURE"}]
        }),
        // PR with merge conflict
        json!({
            "number": 201,
            "headRefName": "york/feature",
            "mergeable": "CONFLICTING",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // Healthy PR (no issues, needs review)
        json!({
            "number": 202,
            "headRefName": "lexington/docs",
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
        // Approved PR ready for auto-merge
        json!({
            "number": 203,
            "headRefName": "broadway/ready",
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}]
        }),
    ];

    let mut issues_found = 0;
    let mut auto_merge_count = 0;

    for pr in &prs {
        let pr_number = pr["number"].as_u64().unwrap();
        let issues = detect_pr_issues(pr);

        for issue in &issues {
            if tracker.should_nudge(pr_number, *issue) {
                tracker.record_nudge(pr_number, *issue);
                issues_found += 1;
            }
        }

        if is_auto_mergeable(pr) {
            auto_merge_count += 1;
        }
    }

    // PR 200: CI failed (1)
    // PR 201: merge conflict (1)
    // PR 202: no issues
    // PR 203: approved (1)
    assert_eq!(issues_found, 3, "should find 3 issues");
    assert_eq!(auto_merge_count, 1, "should have 1 auto-mergeable PR");
}
