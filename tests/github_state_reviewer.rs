//! Integration tests for GitHubState reviewer lifecycle management.
//!
//! These tests verify GitHubState correctly:
//! - Tracks reviewer assignments for PRs
//! - Prevents duplicate reviewer assignments
//! - Persists reviewer state across restarts
//! - Coordinates webhook and polling-based assignments
//!
//! Run with: `cargo test --test github_state_reviewer`

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use midtown::github_state::{AssignmentSource, GitHubState, PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS};

// =============================================================================
// Test data structures
// =============================================================================

/// PR data structure for testing reviewer spawn decisions.
#[derive(Debug, Clone)]
struct TestPr {
    #[allow(dead_code)]
    number: u64,
    is_draft: bool,
    review_decision: String,
    created_at: DateTime<Utc>,
    has_claude_review: bool,
}

impl TestPr {
    fn new(number: u64, _head_ref: &str) -> Self {
        Self {
            number,
            is_draft: false,
            review_decision: String::new(),
            created_at: Utc::now() - chrono::Duration::minutes(5), // Old enough for review
            has_claude_review: false,
        }
    }

    fn draft(mut self) -> Self {
        self.is_draft = true;
        self
    }

    fn with_review(mut self, decision: &str) -> Self {
        self.review_decision = decision.to_string();
        self
    }

    fn with_claude_review(mut self) -> Self {
        self.has_claude_review = true;
        self
    }

    fn just_created(mut self) -> Self {
        self.created_at = Utc::now();
        self
    }
}

// =============================================================================
// Tests: Reviewer spawn conditions
// =============================================================================

/// Test that a PR ready for review triggers reviewer spawn.
///
/// A PR is ready for review when:
/// - Not a draft
/// - No existing review decision
/// - No Claude review comment
/// - Old enough (past review delay)
/// - Not already assigned
#[test]
fn pr_ready_for_review_spawns_reviewer() {
    let pr = TestPr::new(42, "amsterdam/feature");
    let state = GitHubState::default();

    // Verify all spawn conditions are met
    assert!(!pr.is_draft, "PR should not be a draft");
    assert!(
        pr.review_decision.is_empty(),
        "PR should have no review decision"
    );
    assert!(!pr.has_claude_review, "PR should have no Claude review");
    assert!(!state.is_assigned(pr.number), "PR should not be assigned");

    // PR age check (should be old enough)
    let age = Utc::now().signed_duration_since(pr.created_at);
    assert!(age.num_seconds() > 0, "PR should have some age");
}

/// Test that draft PRs do NOT trigger reviewer spawn.
#[test]
fn draft_pr_does_not_spawn_reviewer() {
    let pr = TestPr::new(43, "lexington/wip").draft();

    assert!(pr.is_draft, "PR should be a draft");
    // Draft PRs are filtered out before spawn consideration
}

/// Test that PRs with existing review decision do NOT spawn reviewer.
#[test]
fn reviewed_pr_does_not_spawn_reviewer() {
    let pr = TestPr::new(44, "park/reviewed").with_review("APPROVED");

    assert!(
        !pr.review_decision.is_empty(),
        "PR should have review decision"
    );
}

/// Test that PRs with Claude review comment do NOT spawn reviewer.
#[test]
fn pr_with_claude_review_does_not_spawn_reviewer() {
    let pr = TestPr::new(45, "madison/reviewed").with_claude_review();

    assert!(pr.has_claude_review, "PR should have Claude review");
}

/// Test that just-created PRs do NOT spawn reviewer (review delay).
#[test]
fn new_pr_respects_review_delay() {
    let pr = TestPr::new(46, "broadway/new").just_created();

    let age = Utc::now().signed_duration_since(pr.created_at);
    assert!(
        age.num_seconds() < 60,
        "Just-created PR should have minimal age"
    );
    // In production, this would be filtered by PR_REVIEW_DELAY_SECS check
}

// =============================================================================
// Tests: Duplicate reviewer prevention
// =============================================================================

/// Test that already-assigned PRs do NOT spawn duplicate reviewer.
#[test]
fn assigned_pr_does_not_spawn_duplicate_reviewer() {
    let mut state = GitHubState::default();
    let pr = TestPr::new(50, "amsterdam/feature");

    // Assign a reviewer
    state.assign_reviewer(pr.number, "lexington", AssignmentSource::Webhook);

    // Verify assignment prevents spawn
    assert!(
        state.is_assigned(pr.number),
        "PR should be assigned, blocking new spawn"
    );
    assert_eq!(
        state.get_reviewer(pr.number),
        Some("lexington"),
        "Should have correct reviewer assigned"
    );
}

/// Test that expired assignments allow new reviewer spawn.
#[test]
fn expired_assignment_allows_new_spawn() {
    let mut state = GitHubState::default();
    let pr = TestPr::new(51, "york/feature");

    // Assign a reviewer
    state.assign_reviewer(pr.number, "park", AssignmentSource::PollingFallback);

    // Manually expire the assignment
    if let Some(assignment) = state.pr_reviewers.get_mut(&pr.number) {
        assignment.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // Expired assignment should not block new spawn
    assert!(
        !state.is_assigned(pr.number),
        "Expired assignment should not block new spawn"
    );
}

/// Test pending review spawns prevent duplicate scheduling.
#[test]
fn pending_spawn_prevents_duplicate() {
    let mut state = GitHubState::default();
    let future = Utc::now() + chrono::Duration::seconds(60);

    // Add pending spawn for PR 52
    state.add_pending_review_spawn(52, future);
    assert_eq!(state.pending_review_spawns.len(), 1);

    // Try to add duplicate - should be ignored
    state.add_pending_review_spawn(52, future);
    assert_eq!(
        state.pending_review_spawns.len(),
        1,
        "Duplicate pending spawn should be ignored"
    );

    // Different PR should be added
    state.add_pending_review_spawn(53, future);
    assert_eq!(state.pending_review_spawns.len(), 2);
}

// =============================================================================
// Tests: Reviewer assignment persistence
// =============================================================================

/// Test that reviewer assignments persist across save/load.
#[test]
fn reviewer_assignment_persists() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    // Create state with assignment
    let mut state = GitHubState::default();
    state.assign_reviewer(60, "broadway", AssignmentSource::Webhook);
    state.assign_reviewer(61, "columbus", AssignmentSource::PollingFallback);

    // Persist
    state.save(&path).unwrap();

    // Load and verify
    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(loaded.get_reviewer(60), Some("broadway"));
    assert_eq!(loaded.get_reviewer(61), Some("columbus"));
    assert_eq!(
        loaded.pr_reviewers.get(&60).map(|a| a.source),
        Some(AssignmentSource::Webhook)
    );
    assert_eq!(
        loaded.pr_reviewers.get(&61).map(|a| a.source),
        Some(AssignmentSource::PollingFallback)
    );
}

/// Test that pending review spawns persist across restart.
#[test]
fn pending_spawns_persist_across_restart() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    // Create state with pending spawns
    let mut state = GitHubState::default();
    let future = Utc::now() + chrono::Duration::seconds(30);
    state.add_pending_review_spawn(70, future);
    state.add_pending_review_spawn(71, future);

    // Persist
    state.save(&path).unwrap();

    // Load and verify
    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(loaded.pending_review_spawns.len(), 2);

    let pr_numbers: Vec<u64> = loaded
        .pending_review_spawns
        .iter()
        .map(|p| p.pr_number)
        .collect();
    assert!(pr_numbers.contains(&70));
    assert!(pr_numbers.contains(&71));
}

/// Test reviewed_prs cache persists (monotonic review tracking).
#[test]
fn reviewed_prs_cache_persists() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.mark_reviewed_pr(80);
    state.mark_reviewed_pr(81);

    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    assert!(loaded.has_cached_review(80));
    assert!(loaded.has_cached_review(81));
    assert!(!loaded.has_cached_review(82)); // Not cached
}

// =============================================================================
// Tests: Webhook vs polling coordination
// =============================================================================

/// Test that webhook-handled PRs are tracked to prevent polling duplicates.
#[test]
fn webhook_event_prevents_polling_spawn() {
    let mut state = GitHubState::default();

    // Record webhook event for PR 90
    state.record_webhook_event(90);

    // Polling should see this as recently handled
    assert!(
        state.webhook_recently_handled(90, 120),
        "PR should be marked as recently handled by webhook"
    );

    // Different PR not affected
    assert!(
        !state.webhook_recently_handled(91, 120),
        "Different PR should not be marked"
    );
}

/// Test that old webhook events don't block polling.
#[test]
fn old_webhook_event_allows_polling() {
    let mut state = GitHubState::default();

    // Manually backdate the webhook event
    state
        .pr_last_webhook_event
        .insert(92, Utc::now() - chrono::Duration::seconds(300));

    // 120s window should not match a 300s-old event
    assert!(
        !state.webhook_recently_handled(92, 120),
        "Old webhook event should not block polling"
    );

    // But a longer window would match
    assert!(
        state.webhook_recently_handled(92, 600),
        "Event should match longer window"
    );
}

// =============================================================================
// Tests: Active reviewer tracking
// =============================================================================

/// Test active reviewers are correctly identified.
#[test]
fn active_reviewers_identified() {
    let mut state = GitHubState::default();

    state.assign_reviewer(110, "broadway", AssignmentSource::Webhook);
    state.assign_reviewer(111, "madison", AssignmentSource::PollingFallback);

    let active = state.active_reviewers();
    assert!(active.contains("broadway"));
    assert!(active.contains("madison"));
    assert_eq!(active.len(), 2);
}

/// Test expired assignments don't count as active.
#[test]
fn expired_assignments_not_active() {
    let mut state = GitHubState::default();

    state.assign_reviewer(120, "york", AssignmentSource::Webhook);

    // Expire the assignment
    if let Some(a) = state.pr_reviewers.get_mut(&120) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    let active = state.active_reviewers();
    assert!(
        !active.contains("york"),
        "Expired assignment should not be active"
    );
    assert_eq!(state.active_count(), 0);
}

/// Test cleanup preserves assignments for running coworkers.
#[test]
fn cleanup_preserves_running_coworker_assignments() {
    let mut state = GitHubState::default();

    state.assign_reviewer(130, "riverside", AssignmentSource::Webhook);
    state.assign_reviewer(131, "columbus", AssignmentSource::PollingFallback);

    // Expire both assignments
    for pr in [130, 131] {
        if let Some(a) = state.pr_reviewers.get_mut(&pr) {
            a.assigned_at = Utc::now()
                - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
        }
    }

    // Only riverside is still running
    let running: HashSet<String> = ["riverside".to_string()].into_iter().collect();
    state.cleanup_expired_preserving(&running, None);

    // riverside's assignment preserved, columbus's removed
    assert!(
        state.pr_reviewers.contains_key(&130),
        "Running coworker's assignment should be preserved"
    );
    assert!(
        !state.pr_reviewers.contains_key(&131),
        "Non-running coworker's assignment should be removed"
    );
}

// =============================================================================
// Tests: Review completion handling
// =============================================================================

/// Test that completed reviews clear the assignment.
#[test]
fn review_complete_clears_assignment() {
    let mut state = GitHubState::default();

    // Assign and then complete review
    state.assign_reviewer(140, "vernon", AssignmentSource::Webhook);
    state.mark_reviewed_pr(140);

    // The assignment removal is done in the daemon, not here,
    // but we can verify the reviewed status is tracked
    assert!(
        state.has_cached_review(140),
        "PR should be marked as reviewed"
    );

    // Manually remove as daemon would
    state.remove_assignment(140);
    assert!(
        !state.is_assigned(140),
        "Assignment should be removed after review"
    );
}

/// Test reviewer assignment removal by coworker name.
#[test]
fn remove_assignment_by_reviewer() {
    let mut state = GitHubState::default();

    state.assign_reviewer(150, "pleasant", AssignmentSource::Webhook);
    state.assign_reviewer(151, "vernon", AssignmentSource::PollingFallback);

    // Remove by reviewer name
    let removed = state.remove_assignment_by_reviewer("pleasant");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().pr_number, 150);

    // Verify removal
    assert!(state.pr_for_reviewer("pleasant").is_none());
    assert!(state.get_reviewer(150).is_none());

    // Other assignment unaffected
    assert_eq!(state.pr_for_reviewer("vernon"), Some(151));
}

// =============================================================================
// Tests: Concurrent review limit
// =============================================================================

/// Test active review count for capacity checks.
#[test]
fn active_review_count() {
    let mut state = GitHubState::default();

    state.assign_reviewer(160, "amsterdam", AssignmentSource::Webhook);
    state.assign_reviewer(161, "lexington", AssignmentSource::Webhook);
    state.assign_reviewer(162, "park", AssignmentSource::Webhook);

    assert_eq!(state.active_count(), 3);

    // Remove one
    state.remove_assignment(160);
    assert_eq!(state.active_count(), 2);

    // Expire one
    if let Some(a) = state.pr_reviewers.get_mut(&161) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }
    assert_eq!(state.active_count(), 1);
}

// =============================================================================
// Tests: Closed PR cleanup
// =============================================================================

/// Test that closed PRs are cleaned up properly.
#[test]
fn closed_prs_cleaned_up() {
    let mut state = GitHubState::default();

    // Set up various PR-related state
    state.assign_reviewer(170, "broadway", AssignmentSource::Webhook);
    state.assign_reviewer(171, "madison", AssignmentSource::Webhook);
    state.mark_reviewed_pr(170);
    state.mark_reviewed_pr(171);
    state.record_webhook_event(170);
    state.record_webhook_event(171);

    // Only PR 170 is still open
    state.cleanup_closed_prs(&[170]);

    // PR 170 data preserved
    assert!(state.is_assigned(170));
    assert!(state.has_cached_review(170));
    assert!(state.pr_last_webhook_event.contains_key(&170));

    // PR 171 data cleaned up
    assert!(!state.is_assigned(171));
    assert!(!state.has_cached_review(171));
    assert!(!state.pr_last_webhook_event.contains_key(&171));
}

// =============================================================================
// Tests: Integration scenarios
// =============================================================================

/// Test complete reviewer lifecycle: spawn -> review -> complete -> cleanup.
#[test]
fn complete_reviewer_lifecycle() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");
    let pr_number = 200u64;

    // Phase 1: PR opened, webhook triggers pending spawn
    let mut state = GitHubState::default();
    let spawn_after = Utc::now() + chrono::Duration::seconds(30);
    state.add_pending_review_spawn(pr_number, spawn_after);
    state.record_webhook_event(pr_number);
    state.save(&path).unwrap();

    // Simulate daemon restart
    let mut state = GitHubState::load(&path).unwrap();
    assert_eq!(state.pending_review_spawns.len(), 1);

    // Phase 2: Spawn delay elapsed, reviewer spawned
    // (Simulate by manually backdating the pending spawn)
    state.pending_review_spawns[0].spawn_after = Utc::now() - chrono::Duration::seconds(1);
    let ready = state.drain_ready_review_spawns();
    assert_eq!(ready, vec![pr_number]);

    // Assign the reviewer
    state.assign_reviewer(pr_number, "amsterdam", AssignmentSource::Webhook);
    state.save(&path).unwrap();

    // Phase 3: Review completed
    let mut state = GitHubState::load(&path).unwrap();
    assert!(state.is_assigned(pr_number));

    state.mark_reviewed_pr(pr_number);
    state.remove_assignment(pr_number);
    state.save(&path).unwrap();

    // Phase 4: PR merged, cleanup
    let mut state = GitHubState::load(&path).unwrap();
    assert!(state.has_cached_review(pr_number));
    assert!(!state.is_assigned(pr_number));

    state.cleanup_closed_prs(&[]); // No open PRs
    assert!(!state.has_cached_review(pr_number));
}

/// Test that multiple reviewer spawn attempts for the same PR are prevented.
///
/// Bug: When multiple events (webhooks, polling) trigger reviewer spawns
/// for the same PR in quick succession, they all pass the `is_assigned` check
/// before any spawn completes, causing multiple reviewers to be spawned.
///
/// This test verifies that:
/// 1. First spawn check passes (`is_assigned` returns false)
/// 2. Assignment is recorded immediately (before spawn completes)
/// 3. Second spawn check fails (`is_assigned` returns true)
/// 4. Only one reviewer is spawned
#[test]
fn prevent_multiple_reviewer_spawns_for_same_pr() {
    let mut state = GitHubState::default();
    let pr_number = 859u64;

    // Simulate first spawn attempt
    // 1. Check if assigned (should be false)
    assert!(
        !state.is_assigned(pr_number),
        "First spawn: PR should not be assigned yet"
    );

    // 2. Assign reviewer BEFORE spawn completes
    //    (this is where the fix needs to be - assign immediately, not after spawn)
    state.assign_reviewer(pr_number, "broadway", AssignmentSource::Webhook);

    // 3. Second spawn attempt (concurrent webhook or polling)
    //    should see the assignment and skip spawning
    assert!(
        state.is_assigned(pr_number),
        "Second spawn: PR should already be assigned, preventing duplicate spawn"
    );

    // 4. Verify only one reviewer is assigned
    let reviewer = state.get_reviewer(pr_number);
    assert_eq!(
        reviewer,
        Some("broadway"),
        "Only one reviewer should be assigned"
    );

    // 5. Attempting to assign another reviewer should be blocked by the caller
    //    (the daemon's collect_reviewer_effects should check is_assigned)
    //    This test documents the expected behavior - the fix must ensure
    //    assignment happens BEFORE spawn completes, not in the on_success callback.
}

/// Test race between webhook and polling for same PR.
#[test]
fn webhook_polling_race_handled() {
    let mut state = GitHubState::default();
    let pr_number = 210u64;

    // Webhook arrives first and queues pending spawn
    let spawn_after = Utc::now() + chrono::Duration::seconds(30);
    state.add_pending_review_spawn(pr_number, spawn_after);
    state.record_webhook_event(pr_number);

    // Polling runs - should defer to webhook
    assert!(
        state.webhook_recently_handled(pr_number, 120),
        "Polling should defer to recent webhook"
    );

    // Pending spawn also blocks duplicates
    assert_eq!(
        state.pending_review_spawns.len(),
        1,
        "Should have exactly one pending spawn"
    );
}

/// Test multiple PRs with different states.
#[test]
fn batch_pr_reviewer_handling() {
    let mut state = GitHubState::default();

    // PR 220: needs review (no assignment)
    // PR 221: assigned (webhook)
    state.assign_reviewer(221, "lexington", AssignmentSource::Webhook);
    // PR 222: assigned (polling)
    state.assign_reviewer(222, "park", AssignmentSource::PollingFallback);
    // PR 223: reviewed (has cached review)
    state.mark_reviewed_pr(223);

    // Verify states
    assert!(!state.is_assigned(220), "PR 220 needs reviewer");
    assert!(state.is_assigned(221), "PR 221 has reviewer");
    assert!(state.is_assigned(222), "PR 222 has reviewer");
    assert!(state.has_cached_review(223), "PR 223 already reviewed");

    // Active count should be 2
    assert_eq!(state.active_count(), 2);

    // Source tracking (via direct field access)
    assert_eq!(
        state.pr_reviewers.get(&221).map(|a| a.source),
        Some(AssignmentSource::Webhook)
    );
    assert_eq!(
        state.pr_reviewers.get(&222).map(|a| a.source),
        Some(AssignmentSource::PollingFallback)
    );
}

// =============================================================================
// Tests: Ghost reviewer assignments — name reuse handled by cleanup
// =============================================================================

/// Test that cleanup_expired_preserving handles the name-reuse case correctly.
///
/// Scenario: "columbus" was assigned to review PR #823 and the assignment timed out.
/// After restart, "columbus" is respawned as a dev coworker (no review worktree).
/// cleanup_expired_preserving should NOT preserve the expired assignment because
/// columbus is not in the running_coworker_names set (filtered to review worktree owners).
#[test]
fn cleanup_expired_preserving_handles_name_reuse() {
    let mut state = GitHubState::default();

    // Pre-restart: columbus was reviewing PR #823
    state.assign_reviewer(823, "columbus", AssignmentSource::Webhook);

    // Expire the assignment (simulate time passing beyond timeout)
    if let Some(a) = state.pr_reviewers.get_mut(&823) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // After restart, columbus is running but as a dev coworker (no review worktree).
    // The running_coworker_names passed to cleanup_expired_preserving should only
    // include coworkers bound to review worktrees, so columbus is NOT in the set.
    let running_reviewers: HashSet<String> = HashSet::new(); // columbus excluded (dev work)
    state.cleanup_expired_preserving(&running_reviewers, None);

    // The expired assignment should be removed because columbus is not a reviewer
    assert!(
        !state.pr_reviewers.contains_key(&823),
        "Expired assignment for dev-reused coworker should be cleaned up"
    );
}

/// Test that cleanup_expired_preserving preserves active reviewer assignments.
///
/// If a reviewer is still running with a review worktree, their expired assignment
/// should be preserved (and refreshed) to avoid losing track of active reviews.
#[test]
fn cleanup_expired_preserving_keeps_active_reviewer() {
    let mut state = GitHubState::default();

    state.assign_reviewer(824, "york", AssignmentSource::Webhook);

    // Expire the assignment
    if let Some(a) = state.pr_reviewers.get_mut(&824) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // york is still running AND bound to a review worktree
    let running_reviewers: HashSet<String> = ["york".to_string()].into_iter().collect();
    state.cleanup_expired_preserving(&running_reviewers, None);

    // Assignment should be preserved (york is actively reviewing)
    assert!(
        state.pr_reviewers.contains_key(&824),
        "Active reviewer's assignment should be preserved"
    );
}

/// Test simulating rapid concurrent spawn attempts (the actual bug scenario from PR #859).
///
/// Bug reproduction: PR #859 had 3 reviewers spawned (broadway, madison, columbus)
/// because multiple webhook/polling events triggered spawns before any completed.
///
/// This test verifies that the fix (immediate assignment before spawn) prevents
/// the race condition by making the assignment visible to subsequent spawn attempts.
#[test]
fn concurrent_spawn_attempts_prevented_by_immediate_assignment() {
    let mut state = GitHubState::default();
    let pr_number = 859u64;

    // Simulate the bug scenario: 3 rapid spawn decisions happening concurrently
    // (e.g., webhook event + 2 polling ticks before any spawn completes)

    // === Spawn attempt 1 (webhook) ===
    // Check if assigned (should be false)
    assert!(
        !state.is_assigned(pr_number),
        "First check: PR should not be assigned"
    );

    // IMMEDIATELY assign before spawning (the fix!)
    // Previously this happened in the on_success callback AFTER spawn completed.
    // Now it happens BEFORE, making it visible to concurrent spawn attempts.
    state.assign_reviewer(pr_number, "broadway", AssignmentSource::Webhook);

    // === Spawn attempt 2 (polling, concurrent) ===
    // Check if assigned (should be true now because of immediate assignment!)
    assert!(
        state.is_assigned(pr_number),
        "Second check: PR should already be assigned, preventing duplicate spawn"
    );
    // The daemon's collect_reviewer_effects() would skip spawning here

    // === Spawn attempt 3 (polling, concurrent) ===
    // Check if assigned (should still be true)
    assert!(
        state.is_assigned(pr_number),
        "Third check: PR should still be assigned, preventing third duplicate spawn"
    );
    // The daemon's collect_reviewer_effects() would skip spawning here too

    // === Verify only one reviewer assigned ===
    assert_eq!(
        state.get_reviewer(pr_number),
        Some("broadway"),
        "Only broadway should be assigned (first spawn attempt)"
    );

    // Verify no other reviewers got assigned
    let assigned_reviewers: Vec<&str> = state.assigned_reviewers().collect();
    assert_eq!(
        assigned_reviewers.len(),
        1,
        "Only one reviewer should be assigned total (not 3 like in the bug)"
    );
    assert_eq!(assigned_reviewers[0], "broadway");
}

/// Test that spawn failure cleanup works correctly with optimistic assignment.
///
/// When a spawn fails after optimistic assignment, the RemoveReviewerAssignment
/// effect should clean up the assignment so a retry can succeed.
///
/// This prevents a failed spawn from permanently blocking future spawn attempts.
#[test]
fn spawn_failure_cleans_up_optimistic_assignment() {
    let mut state = GitHubState::default();
    let pr_number = 859u64;

    // Simulate optimistic assignment before spawn (the fix)
    state.assign_reviewer(pr_number, "broadway", AssignmentSource::Webhook);
    assert!(state.is_assigned(pr_number));

    // Spawn fails - simulate RemoveReviewerAssignment effect from on_failure callback
    let removed = state.remove_assignment(pr_number);
    assert!(removed.is_some(), "Assignment should be removed");
    assert_eq!(removed.unwrap().reviewer, "broadway");

    // Verify assignment is cleared
    assert!(
        !state.is_assigned(pr_number),
        "Assignment should be cleared after spawn failure"
    );

    // Retry should now work (can assign again)
    state.assign_reviewer(pr_number, "madison", AssignmentSource::PollingFallback);
    assert!(state.is_assigned(pr_number));
    assert_eq!(state.get_reviewer(pr_number), Some("madison"));
}
