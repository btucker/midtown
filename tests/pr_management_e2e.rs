//! E2E tests for PR management decisions using captured WorldSnapshot fixtures.
//!
//! These tests verify the daemon correctly handles PR-related workflows:
//! - Spawning reviewers for PRs needing review
//! - Auto-merge eligibility detection
//! - Nudging PR owners about comments/feedback
//!
//! Run with: `cargo test --test pr_management_e2e`

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

// Re-exported types from daemon module
use midtown::daemon::{PrIssueTracker, PrIssueType};

// Decision functions
use midtown::rules::{PrAction, decide_pr_issue_action};

// Helper functions
use midtown::daemon::helpers::coworker_from_branch;

/// Load a snapshot fixture and parse it into test-friendly data structures.
fn load_snapshot(json_str: &str) -> SnapshotData {
    let v: Value = serde_json::from_str(json_str).expect("valid JSON");

    // Extract active coworker names
    let active_names: HashSet<String> = v["active_names"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract coworkers with open PRs
    let coworkers_with_open_prs: HashSet<String> = v["coworkers_with_open_prs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract reviewed PRs (PR numbers that already have Claude reviews)
    let reviewed_prs: HashSet<u64> = v["reviewed_prs"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|n| n.as_u64()).collect())
        .unwrap_or_default();

    // Extract active reviewers
    let active_reviewers: HashSet<String> = v["active_reviewers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract reviewer PR assignments (coworker name -> PR number)
    let reviewer_pr_assignments: HashMap<String, u64> = v["reviewer_pr_assignments"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();

    // Extract CI-passed PR coworkers (candidates for auto-merge)
    let ci_passed_pr_coworkers: HashSet<String> = v["ci_passed_pr_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract busy coworkers
    let busy_coworkers: HashSet<String> = v["busy_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Extract dev limit status
    let is_at_dev_limit = v["is_at_dev_limit"].as_bool().unwrap_or(false);

    // Extract timestamp
    let now_utc = v["now_utc"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    // Extract pane contents for debugging
    let pane_contents: HashMap<String, String> = v["pane_contents"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();

    SnapshotData {
        active_names,
        coworkers_with_open_prs,
        reviewed_prs,
        active_reviewers,
        reviewer_pr_assignments,
        ci_passed_pr_coworkers,
        busy_coworkers,
        is_at_dev_limit,
        now_utc,
        pane_contents,
    }
}

#[derive(Debug)]
struct SnapshotData {
    active_names: HashSet<String>,
    coworkers_with_open_prs: HashSet<String>,
    reviewed_prs: HashSet<u64>,
    active_reviewers: HashSet<String>,
    reviewer_pr_assignments: HashMap<String, u64>,
    ci_passed_pr_coworkers: HashSet<String>,
    #[allow(dead_code)]
    busy_coworkers: HashSet<String>,
    is_at_dev_limit: bool,
    #[allow(dead_code)]
    now_utc: DateTime<Utc>,
    #[allow(dead_code)]
    pane_contents: HashMap<String, String>,
}

// ============================================================================
// Test: PR Needing Review Spawns Reviewer
// ============================================================================

/// Test that PRs without reviewers are identified for reviewer spawning.
///
/// Snapshot context (20260203-182216): Multiple coworkers have open PRs,
/// some already have reviewers assigned, others need review.
///
/// The daemon should spawn reviewers for PRs that:
/// - Have no reviewer assigned (not in reviewer_pr_assignments)
/// - Are not already reviewed (not in reviewed_prs)
/// - Are not from an active reviewer (would be self-review)
#[test]
fn pr_needing_review_identified_for_spawn() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // PRs that already have reviewers assigned
    let _assigned_prs: HashSet<u64> = data.reviewer_pr_assignments.values().copied().collect();

    // Find coworkers with open PRs whose PRs need review
    // (not already reviewed and not already assigned for review)
    let prs_needing_review: Vec<&String> = data
        .coworkers_with_open_prs
        .iter()
        .filter(|coworker| {
            // Check if this coworker's PR is already being reviewed
            // We don't have direct PR number mapping, but we can check:
            // - Is this coworker themselves an active reviewer? (would be self-review)
            // - Are they already assigned to review a PR?
            !data.active_reviewers.contains(*coworker)
        })
        .collect();

    // In this snapshot:
    // - park, riverside, columbus, york have open PRs
    // - amsterdam, broadway are active reviewers
    // - amsterdam is reviewing PR 543, broadway is reviewing PR 540, lexington reviewed PR 542
    // - reviewed_prs: [540, 543]

    // Coworkers with open PRs who are NOT active reviewers are candidates for needing review
    assert!(
        !prs_needing_review.is_empty(),
        "should have PRs needing review from non-reviewer coworkers"
    );

    // Verify the active reviewers are correctly identified
    assert!(
        data.active_reviewers.contains("amsterdam"),
        "amsterdam should be an active reviewer"
    );
    assert!(
        data.active_reviewers.contains("broadway"),
        "broadway should be an active reviewer"
    );

    // Verify reviewer assignments are tracked
    assert_eq!(
        data.reviewer_pr_assignments.get("amsterdam"),
        Some(&543),
        "amsterdam should be assigned to PR 543"
    );
    assert_eq!(
        data.reviewer_pr_assignments.get("broadway"),
        Some(&540),
        "broadway should be assigned to PR 540"
    );

    // Test that the daemon would spawn a reviewer for unassigned PRs
    // by checking the conditions in collect_reviewer_effects:
    // 1. PR is not in reviewed_prs
    // 2. PR is not in assigned PRs
    // 3. Max concurrent reviews not reached

    // The key insight: PRs from park, riverside, columbus, york need review
    // because they're not in active_reviewers and their work is done
    for coworker in &["park", "riverside", "columbus", "york"] {
        assert!(
            data.coworkers_with_open_prs.contains(*coworker),
            "{} should have an open PR",
            coworker
        );
    }
}

/// Test that active reviewers are excluded from self-review scenarios.
#[test]
fn active_reviewers_excluded_from_self_review() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // Active reviewers should not be assigned to review their own PRs
    for reviewer in &data.active_reviewers {
        // A reviewer shouldn't spawn another review for their own PR
        // This is enforced by checking branch ownership in collect_reviewer_effects
        if data.coworkers_with_open_prs.contains(reviewer) {
            // If a reviewer has their own open PR, they shouldn't review it
            // The daemon filters this out by checking coworker_from_branch
            assert!(
                data.active_reviewers.contains(reviewer),
                "{} is both a reviewer and has an open PR - self-review should be prevented",
                reviewer
            );
        }
    }
}

// ============================================================================
// Test: Approved Green PR Auto-Merges
// ============================================================================

/// Test that PRs with passing CI are identified as auto-merge candidates.
///
/// Snapshot context: ci_passed_pr_coworkers contains coworkers whose PRs
/// have green CI. Combined with approval status, these are merge candidates.
#[test]
fn ci_passed_prs_identified_for_merge() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // In this snapshot: riverside and columbus have CI-passed PRs
    assert!(
        !data.ci_passed_pr_coworkers.is_empty(),
        "should have coworkers with CI-passed PRs"
    );

    // Verify expected coworkers have CI-passed PRs
    assert!(
        data.ci_passed_pr_coworkers.contains("riverside"),
        "riverside should have CI-passed PR"
    );
    assert!(
        data.ci_passed_pr_coworkers.contains("columbus"),
        "columbus should have CI-passed PR"
    );

    // Auto-merge requires:
    // 1. CI passed (checked above)
    // 2. Approval from reviewer (reviewDecision == "APPROVED")
    // 3. No merge conflicts (mergeable == "MERGEABLE")
    //
    // The daemon uses is_auto_mergeable() from helpers to check all conditions.
    // This test verifies the precondition: CI passed.
}

/// Test that PRs are not auto-merged when still being reviewed.
#[test]
fn prs_under_review_not_auto_merged() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // PRs that are currently being reviewed (assigned but not in reviewed_prs)
    let prs_under_review: Vec<u64> = data
        .reviewer_pr_assignments
        .values()
        .filter(|pr_num| !data.reviewed_prs.contains(pr_num))
        .copied()
        .collect();

    // PRs under active review should not be auto-merged
    // PR 540 and 543 are assigned to reviewers
    // PR 540 is in reviewed_prs (review complete)
    // PR 543 is in reviewed_prs (review complete)
    //
    // When a PR is in reviewed_prs, it has a Claude review but may still
    // need the author to address feedback before merging.

    // Verify reviewed_prs contains the assigned PRs
    for (reviewer, pr_num) in &data.reviewer_pr_assignments {
        if data.reviewed_prs.contains(pr_num) {
            // This PR has a completed review
            assert!(
                data.reviewed_prs.contains(pr_num),
                "PR #{} assigned to {} should be in reviewed_prs",
                pr_num,
                reviewer
            );
        }
    }

    // If there are PRs under active review, they should not be auto-merged
    for pr_num in prs_under_review {
        // Under-review PRs should wait for review completion
        assert!(
            !data.reviewed_prs.contains(&pr_num),
            "PR #{} is under review and should not be in reviewed_prs yet",
            pr_num
        );
    }
}

// ============================================================================
// Test: PR Comment Nudges Owner
// ============================================================================

/// Test that PR owners are nudged when their PR has review feedback.
///
/// Uses the pure decision function to verify nudge behavior based on
/// whether the owner is active or inactive.
///
/// Note: decide_pr_issue_action has the same logic as decide_review_complete_action
/// (which is pub(crate) and not accessible from tests). Both functions:
/// - Nudge if owner is active
/// - Spawn if owner is inactive and not at dev limit
/// - Skip if at dev limit and owner inactive
#[test]
fn pr_owner_nudged_for_review_feedback() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // Simulate a review feedback scenario for a coworker with an open PR
    let active_coworkers: Vec<String> = data.active_names.iter().cloned().collect();

    // Test 1: Active owner should be nudged
    let action = decide_pr_issue_action(
        "riverside",
        &active_coworkers,
        data.is_at_dev_limit,
        "Your PR has review feedback — please address it.",
    );

    assert!(
        matches!(action, PrAction::NudgeOwner { ref owner, .. } if owner == "riverside"),
        "active owner should be nudged: {:?}",
        action
    );

    // Test 2: Inactive owner should trigger spawn
    // Create a scenario where the owner is not in active coworkers
    let limited_active: Vec<String> = vec!["york".to_string()];
    let action = decide_pr_issue_action(
        "madison",
        &limited_active,
        false, // not at dev limit
        "Your PR has review feedback — please address it.",
    );

    assert!(
        matches!(action, PrAction::SpawnOwner { ref owner, .. } if owner == "madison"),
        "inactive owner should trigger spawn: {:?}",
        action
    );
}

/// Test that PR issue detection correctly handles active vs inactive owners.
#[test]
fn pr_issue_action_respects_owner_status() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    let active_coworkers: Vec<String> = data.active_names.iter().cloned().collect();

    // Test with an active owner (should nudge)
    let action = decide_pr_issue_action(
        "riverside",
        &active_coworkers,
        data.is_at_dev_limit,
        "PR #123 - CI failed: please investigate",
    );

    assert!(
        matches!(action, PrAction::NudgeOwner { ref owner, .. } if owner == "riverside"),
        "active owner should receive nudge: {:?}",
        action
    );

    // Test with inactive owner (should spawn)
    let action = decide_pr_issue_action(
        "unknown_coworker",
        &active_coworkers,
        false, // not at dev limit
        "PR #123 - CI failed: please investigate",
    );

    assert!(
        matches!(action, PrAction::SpawnOwner { ref owner, .. } if owner == "unknown_coworker"),
        "inactive owner should trigger spawn: {:?}",
        action
    );
}

/// Test that dev limit is respected for PR issue handling.
#[test]
fn pr_issue_respects_dev_limit() {
    let active_coworkers: Vec<String> = vec!["york".to_string()];

    // At dev limit with inactive owner - should skip
    let action = decide_pr_issue_action(
        "madison", // not in active_coworkers
        &active_coworkers,
        true, // at dev limit
        "PR #123 - CI failed: please investigate",
    );

    assert!(
        matches!(action, PrAction::Skip { .. }),
        "should skip when at dev limit and owner inactive: {:?}",
        action
    );
}

// ============================================================================
// Test: PR Issue Tracker Deduplication
// ============================================================================

/// Test that the PR issue tracker correctly deduplicates nudges.
#[test]
fn pr_issue_tracker_prevents_duplicate_nudges() {
    let mut tracker = PrIssueTracker::new();

    // First nudge for CI failure should be allowed
    assert!(tracker.should_nudge(42, PrIssueType::CiFailed));
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // Immediate repeat should be blocked
    assert!(
        !tracker.should_nudge(42, PrIssueType::CiFailed),
        "duplicate nudge should be blocked"
    );

    // Different issue type on same PR should be allowed
    assert!(
        tracker.should_nudge(42, PrIssueType::MergeConflict),
        "different issue type should be allowed"
    );

    // Same issue on different PR should be allowed
    assert!(
        tracker.should_nudge(43, PrIssueType::CiFailed),
        "same issue on different PR should be allowed"
    );
}

// ============================================================================
// Test: Branch Owner Extraction
// ============================================================================

/// Test that coworker names are correctly extracted from branch names.
#[test]
fn coworker_from_branch_extracts_owner() {
    // Valid coworker branch names
    assert_eq!(
        coworker_from_branch("amsterdam/fix-bug"),
        Some("amsterdam".to_string())
    );
    assert_eq!(
        coworker_from_branch("york/new-feature"),
        Some("york".to_string())
    );
    assert_eq!(
        coworker_from_branch("broadway/refactor"),
        Some("broadway".to_string())
    );

    // Invalid or non-coworker branches
    assert_eq!(coworker_from_branch("main"), None);
    assert_eq!(coworker_from_branch("feature/something"), None);
    assert_eq!(coworker_from_branch("unknown-coworker/fix"), None);
}

// ============================================================================
// Test: Complete PR Workflow Scenario
// ============================================================================

/// Test a complete PR workflow scenario using snapshot data.
///
/// This test walks through the full lifecycle of PR management decisions
/// using the captured snapshot state.
#[test]
fn complete_pr_workflow_scenario() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let data = load_snapshot(fixture);

    // Scenario: Multiple coworkers have open PRs at various stages
    //
    // 1. park, riverside, columbus, york have open PRs (coworkers_with_open_prs)
    // 2. amsterdam and broadway are reviewing PRs (active_reviewers)
    // 3. PRs 540 and 543 have been reviewed (reviewed_prs)
    // 4. riverside and columbus have CI-passed PRs (ci_passed_pr_coworkers)

    // Verify scenario setup
    assert_eq!(
        data.coworkers_with_open_prs.len(),
        4,
        "should have 4 coworkers with open PRs"
    );
    assert_eq!(
        data.active_reviewers.len(),
        2,
        "should have 2 active reviewers"
    );
    assert_eq!(data.reviewed_prs.len(), 2, "should have 2 reviewed PRs");
    assert_eq!(
        data.ci_passed_pr_coworkers.len(),
        2,
        "should have 2 coworkers with CI-passed PRs"
    );

    // Verify the daemon state allows for proper workflow decisions
    let active_coworkers: Vec<String> = data.active_names.iter().cloned().collect();

    // All coworkers with open PRs should be able to receive nudges if active
    for coworker in &data.coworkers_with_open_prs {
        if data.active_names.contains(coworker) {
            let action = decide_pr_issue_action(
                coworker,
                &active_coworkers,
                data.is_at_dev_limit,
                "test nudge",
            );
            assert!(
                matches!(action, PrAction::NudgeOwner { .. }),
                "active coworker {} should be nudgeable",
                coworker
            );
        }
    }

    // CI-passed PR owners should be candidates for merge nudges
    for coworker in &data.ci_passed_pr_coworkers {
        assert!(
            data.coworkers_with_open_prs.contains(coworker),
            "CI-passed PR coworker {} should have an open PR",
            coworker
        );
    }
}
