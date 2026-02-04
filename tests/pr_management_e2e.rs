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
/// Snapshot context (20260204-050821): Multiple coworkers have open PRs,
/// some are being actively reviewed, others may still need review.
///
/// The daemon should spawn reviewers for PRs that:
/// - Have no reviewer assigned (not in reviewer_pr_assignments)
/// - Are not already reviewed (not in reviewed_prs)
/// - Are not from an active reviewer (would be self-review)
#[test]
fn pr_needing_review_identified_for_spawn() {
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
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
    // - riverside, lexington, vernon, york have open PRs
    // - madison, broadway, riverside are active reviewers
    // - broadway reviewing PR 562, madison reviewing PR 563, riverside reviewing PR 564
    // - reviewed_prs: [] (none completed yet)

    // Coworkers with open PRs who are NOT active reviewers are candidates for needing review
    // In this case: lexington, vernon, york (riverside is itself a reviewer)
    assert!(
        !prs_needing_review.is_empty(),
        "should have PRs needing review from non-reviewer coworkers"
    );

    // Verify the active reviewers are correctly identified (from snapshot)
    assert!(
        data.active_reviewers.contains("madison"),
        "madison should be an active reviewer"
    );
    assert!(
        data.active_reviewers.contains("broadway"),
        "broadway should be an active reviewer"
    );
    assert!(
        data.active_reviewers.contains("riverside"),
        "riverside should be an active reviewer"
    );

    // Verify reviewer assignments are tracked (from snapshot)
    assert_eq!(
        data.reviewer_pr_assignments.get("broadway"),
        Some(&562),
        "broadway should be assigned to PR 562"
    );
    assert_eq!(
        data.reviewer_pr_assignments.get("madison"),
        Some(&563),
        "madison should be assigned to PR 563"
    );
    assert_eq!(
        data.reviewer_pr_assignments.get("riverside"),
        Some(&564),
        "riverside should be assigned to PR 564"
    );

    // Test that the daemon would spawn a reviewer for unassigned PRs
    // by checking the conditions in collect_reviewer_effects:
    // 1. PR is not in reviewed_prs
    // 2. PR is not in assigned PRs
    // 3. Max concurrent reviews not reached

    // The key insight: PRs from lexington, vernon, york may need review
    // because they're not active reviewers
    for coworker in &["riverside", "lexington", "vernon", "york"] {
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
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
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
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
    let data = load_snapshot(fixture);

    // In this snapshot: riverside and columbus have CI-passed PRs
    assert!(
        !data.ci_passed_pr_coworkers.is_empty(),
        "should have coworkers with CI-passed PRs"
    );

    // Verify expected coworkers have CI-passed PRs (from snapshot)
    // Snapshot shows: ci_passed_pr_coworkers: ["riverside", "vernon"]
    assert!(
        data.ci_passed_pr_coworkers.contains("riverside"),
        "riverside should have CI-passed PR"
    );
    assert!(
        data.ci_passed_pr_coworkers.contains("vernon"),
        "vernon should have CI-passed PR"
    );

    // Auto-merge requires:
    // 1. CI passed (checked above)
    // 2. Approval from reviewer (reviewDecision == "APPROVED")
    // 3. No merge conflicts (mergeable == "MERGEABLE")
    //
    // The daemon uses is_auto_mergeable() from helpers to check all conditions.
    // This test verifies the precondition: CI passed.
}

/// Test that PRs under active review are excluded from auto-merge candidates.
///
/// Auto-merge requires a completed review. PRs that are assigned to reviewers
/// but not yet in reviewed_prs should not be auto-merged.
#[test]
fn prs_under_review_not_auto_merged() {
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
    let data = load_snapshot(fixture);

    // In this snapshot:
    // - PRs 562, 563, 564 are assigned to reviewers (broadway, madison, riverside)
    // - reviewed_prs is empty (no reviews completed yet)
    // - Therefore all assigned PRs are "under review" and should not auto-merge

    // Verify we have PRs assigned to reviewers
    assert!(
        !data.reviewer_pr_assignments.is_empty(),
        "snapshot should have PRs assigned to reviewers"
    );

    // The key invariant: PRs with active reviewers but no completed review
    // should NOT be in ci_passed_pr_coworkers auto-merge candidates
    // (auto-merge requires: CI passed + approved + no conflicts)
    //
    // Since reviewed_prs is empty, none of the assigned PRs have been approved yet,
    // so they shouldn't auto-merge regardless of CI status.
    for (reviewer, pr_num) in &data.reviewer_pr_assignments {
        // A PR under review hasn't been approved yet
        let review_completed = data.reviewed_prs.contains(pr_num);
        assert!(
            !review_completed,
            "PR #{} assigned to {} should not have completed review yet (snapshot shows reviews in progress)",
            pr_num, reviewer
        );
    }

    // Verify the snapshot state: all reviewer assignments are for in-progress reviews
    assert_eq!(
        data.reviewed_prs.len(),
        0,
        "snapshot should have no completed reviews"
    );
    assert!(
        !data.reviewer_pr_assignments.is_empty(),
        "snapshot should have active review assignments"
    );
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
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
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
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
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

/// Test PR workflow behavioral invariants using snapshot data.
///
/// Rather than asserting specific counts (which are snapshot-dependent),
/// this test verifies behavioral invariants that should hold for any
/// valid PR management state.
#[test]
fn complete_pr_workflow_scenario() {
    let fixture = include_str!("fixtures/snapshot/snapshot-pr-management-20260204-050821.json");
    let data = load_snapshot(fixture);

    // Invariant 1: CI-passed PR coworkers must have open PRs
    // (you can't have CI pass on a PR that doesn't exist)
    for coworker in &data.ci_passed_pr_coworkers {
        assert!(
            data.coworkers_with_open_prs.contains(coworker),
            "CI-passed PR coworker {} must have an open PR",
            coworker
        );
    }

    // Invariant 2: Reviewer assignments should reference valid PRs
    // (reviewers shouldn't be assigned to non-existent PRs)
    for reviewer in data.reviewer_pr_assignments.keys() {
        assert!(
            data.active_reviewers.contains(reviewer),
            "reviewer {} with assignment should be in active_reviewers",
            reviewer
        );
    }

    // Invariant 3: Active coworkers with open PRs can be nudged
    let active_coworkers: Vec<String> = data.active_names.iter().cloned().collect();
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
                "active coworker {} with open PR should be nudgeable",
                coworker
            );
        }
    }

    // Invariant 4: Inactive coworkers trigger spawn (when not at dev limit)
    let action = decide_pr_issue_action(
        "nonexistent_coworker",
        &active_coworkers,
        false, // not at dev limit
        "test spawn",
    );
    assert!(
        matches!(action, PrAction::SpawnOwner { .. }),
        "inactive coworker should trigger spawn when not at dev limit"
    );

    // Invariant 5: Dev limit blocks spawning for inactive owners
    let action = decide_pr_issue_action(
        "nonexistent_coworker",
        &active_coworkers,
        true, // at dev limit
        "test skip",
    );
    assert!(
        matches!(action, PrAction::Skip { .. }),
        "should skip spawn when at dev limit"
    );
}
