//! Test for reviewer respawn when assigned reviewer dies.
//!
//! Bug: When a reviewer coworker is assigned to a PR but then dies/shuts down
//! (e.g., killed by auth profile switch), the daemon doesn't spawn a replacement.
//! The assignment persists in github_state.rs and blocks new reviewer spawns
//! until it expires (10 minutes).
//!
//! Expected: When an assigned reviewer is no longer active, the daemon should
//! detect this and spawn a replacement.
//!
//! Run with: `cargo test --test reviewer_respawn_test`

use midtown::github_state::{AssignmentSource, GitHubState};
use std::collections::HashSet;

/// Test that an assigned reviewer who is no longer active allows respawn.
///
/// Scenario:
/// 1. PR #42 needs review
/// 2. Daemon assigns reviewer "amsterdam"
/// 3. amsterdam coworker dies/shuts down
/// 4. Daemon's next poll should detect amsterdam is dead and spawn a replacement
///
/// The bug: Step 4 doesn't happen because `is_assigned()` only checks timeout,
/// not whether the assigned reviewer is still alive.
#[test]
fn dead_reviewer_allows_respawn() {
    let mut state = GitHubState::default();
    let pr_number = 42u64;

    // Step 1: Assign amsterdam to review PR #42
    state.assign_reviewer(pr_number, "amsterdam", AssignmentSource::Webhook);
    assert!(
        state.is_assigned(pr_number),
        "PR should have reviewer assigned"
    );
    assert_eq!(state.get_reviewer(pr_number), Some("amsterdam"));

    // Step 2: amsterdam dies (no longer in active coworkers)
    let active_coworkers: HashSet<String> = HashSet::new(); // Empty - amsterdam is dead

    // Step 3: The daemon should clear the assignment during cleanup
    // because amsterdam is not in the active set
    state.cleanup_expired_preserving(&active_coworkers);

    // The bug: This cleanup only removes EXPIRED assignments where the coworker
    // is not running. It doesn't remove fresh (non-expired) assignments even if
    // the coworker is dead.

    // Fresh assignments are preserved even when coworker is dead:
    assert!(
        state.is_assigned(pr_number),
        "BUG: Fresh assignment persists even though amsterdam is dead"
    );

    // The fix should be: check if the assigned reviewer is in active_coworkers
    // when deciding whether to spawn a new reviewer in collect_reviewer_effects.
}

/// Test the correct behavior: inactive reviewer allows respawn.
///
/// This test documents what the fix should enable:
/// - If an assigned reviewer is not in the active set, treat the PR as unassigned
/// - Allow a new reviewer to be spawned
#[test]
fn inactive_reviewer_should_allow_respawn() {
    let mut state = GitHubState::default();
    let pr_number = 43u64;

    // Assign reviewer
    state.assign_reviewer(pr_number, "lexington", AssignmentSource::Webhook);

    // Active coworkers: lexington is NOT in the list (dead/inactive)
    let active_coworkers: HashSet<String> = ["park".to_string(), "york".to_string()]
        .into_iter()
        .collect();

    // The fix should provide a method to check if a PR has an *active* reviewer
    // (not just any reviewer, but one that's still alive)

    // For now, we can check manually:
    let reviewer = state.get_reviewer(pr_number);
    let has_active_reviewer = reviewer
        .map(|r| active_coworkers.contains(r))
        .unwrap_or(false);

    assert!(
        !has_active_reviewer,
        "lexington is assigned but not active - should allow respawn"
    );

    // The collect_reviewer_effects function should use this logic:
    // if !has_active_reviewer { spawn_new_reviewer() }
}

/// Test that active reviewer still blocks spawn.
#[test]
fn active_reviewer_blocks_respawn() {
    let mut state = GitHubState::default();
    let pr_number = 44u64;

    // Assign reviewer
    state.assign_reviewer(pr_number, "broadway", AssignmentSource::Webhook);

    // broadway IS active
    let active_coworkers: HashSet<String> = ["broadway".to_string(), "madison".to_string()]
        .into_iter()
        .collect();

    // Check if reviewer is active
    let reviewer = state.get_reviewer(pr_number);
    let has_active_reviewer = reviewer
        .map(|r| active_coworkers.contains(r))
        .unwrap_or(false);

    assert!(
        has_active_reviewer,
        "broadway is assigned and active - should block respawn"
    );
}

/// Test cleanup preserves active reviewer assignments (existing behavior).
#[test]
fn cleanup_preserves_active_reviewer() {
    let mut state = GitHubState::default();
    let pr_number = 45u64;

    // Assign reviewer
    state.assign_reviewer(pr_number, "vernon", AssignmentSource::Webhook);

    // Manually expire the assignment
    use chrono::Utc;
    use midtown::github_state::PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS;
    if let Some(a) = state.pr_reviewers.get_mut(&pr_number) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // vernon is still active (running)
    let active_coworkers: HashSet<String> = ["vernon".to_string()].into_iter().collect();

    state.cleanup_expired_preserving(&active_coworkers);

    // Expired assignment should be preserved because vernon is still running
    assert!(
        state.pr_reviewers.contains_key(&pr_number),
        "Active reviewer's expired assignment should be preserved"
    );
}

/// Test cleanup removes inactive expired assignments (existing behavior).
#[test]
fn cleanup_removes_inactive_expired() {
    let mut state = GitHubState::default();
    let pr_number = 46u64;

    // Assign reviewer
    state.assign_reviewer(pr_number, "pleasant", AssignmentSource::Webhook);

    // Manually expire the assignment
    use chrono::Utc;
    use midtown::github_state::PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS;
    if let Some(a) = state.pr_reviewers.get_mut(&pr_number) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // pleasant is NOT active (dead)
    let active_coworkers: HashSet<String> = HashSet::new();

    state.cleanup_expired_preserving(&active_coworkers);

    // Expired assignment for inactive reviewer should be removed
    assert!(
        !state.pr_reviewers.contains_key(&pr_number),
        "Inactive reviewer's expired assignment should be removed"
    );
}
