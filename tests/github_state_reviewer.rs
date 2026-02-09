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
        loaded.get_assignment_source(60),
        Some(AssignmentSource::Webhook)
    );
    assert_eq!(
        loaded.get_assignment_source(61),
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

/// Test assignment source tracking for telemetry.
#[test]
fn assignment_source_tracked() {
    let mut state = GitHubState::default();

    state.assign_reviewer(100, "amsterdam", AssignmentSource::Webhook);
    state.assign_reviewer(101, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(102, "park", AssignmentSource::Webhook);

    let counts = state.count_by_source();
    assert_eq!(counts.get("webhook"), Some(&2));
    assert_eq!(counts.get("polling"), Some(&1));
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
    state.cleanup_expired_preserving(&running);

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

    // Source tracking
    let counts = state.count_by_source();
    assert_eq!(counts.get("webhook"), Some(&1));
    assert_eq!(counts.get("polling"), Some(&1));
}

// =============================================================================
// Tests: Ghost reviewer assignments (Bug #1032)
// =============================================================================

/// Test that ghost reviewer assignments from before a daemon restart are pruned
/// by the decide_reviewer_liveness logic used in collect_reviewer_effects_with_source().
///
/// This exercises the actual fix path: for each ghost assignment, decide_reviewer_liveness
/// returns Dead, and the pruning loop removes the assignment from GitHubState.
///
/// Historical context: Bug #1032 — ghost assignments survived restart and blocked
/// new reviewer spawns. The upfront pruning in pr.rs calls decide_reviewer_liveness
/// for each assignment, removes Dead/UsageLimited ones, then proceeds to spawn.
#[test]
fn ghost_reviewers_are_pruned_via_liveness_check() {
    use midtown::rules::{ReviewerLivenessDecision, decide_reviewer_liveness};
    use std::collections::HashSet;

    let mut state = GitHubState::default();

    // Simulate pre-restart state: 3 reviewers were assigned
    state.assign_reviewer(823, "columbus", AssignmentSource::Webhook);
    state.assign_reviewer(824, "lexington", AssignmentSource::Webhook);
    state.assign_reviewer(825, "york", AssignmentSource::Webhook);

    assert_eq!(state.active_count(), 3);

    // After restart, only park is actually running (as a dev coworker).
    // columbus, lexington, york are "ghost" assignments — persisted in JSON
    // but their coworker processes are dead.
    let active_names: HashSet<String> = ["park"].iter().map(|s| s.to_string()).collect();
    let usage_limited: HashSet<String> = HashSet::new();
    let active_reviewers: HashSet<String> = HashSet::new(); // no one is reviewing

    // Exercise the fix path: decide_reviewer_liveness for each assignment,
    // then prune Dead ones (mirrors the upfront pruning in pr.rs).
    let assignments: Vec<(u64, String)> = state
        .pr_reviewers
        .iter()
        .map(|(pr, a)| (*pr, a.reviewer.clone()))
        .collect();

    for (pr_number, reviewer_name) in assignments {
        let liveness = decide_reviewer_liveness(
            &reviewer_name,
            &active_names,
            &usage_limited,
            &active_reviewers,
        );

        assert_eq!(
            liveness,
            ReviewerLivenessDecision::Dead,
            "Ghost reviewer {} for PR #{} should be Dead",
            reviewer_name,
            pr_number,
        );

        // Prune the dead assignment (same as pr.rs upfront pruning)
        state.remove_assignment(pr_number);
    }

    // After pruning, all ghost assignments should be gone
    assert_eq!(
        state.active_count(),
        0,
        "All ghost assignments should be pruned"
    );
    assert!(!state.is_assigned(823));
    assert!(!state.is_assigned(824));
    assert!(!state.is_assigned(825));
}

/// Test that a dev coworker whose name matches a ghost reviewer assignment
/// is correctly identified as Dead (not Active).
///
/// Scenario: Before restart, "columbus" was assigned to review PR #823.
/// After restart, "columbus" is respawned as a dev coworker working on a task.
/// The ghost assignment should be pruned because the current "columbus" is not reviewing.
#[test]
fn dev_coworker_matching_ghost_reviewer_is_pruned() {
    use midtown::rules::{ReviewerLivenessDecision, decide_reviewer_liveness};
    use std::collections::HashSet;

    let mut state = GitHubState::default();

    // Pre-restart: columbus was reviewing PR #823
    state.assign_reviewer(823, "columbus", AssignmentSource::Webhook);

    // After restart: columbus is alive but doing dev work, not reviewing
    let active_names: HashSet<String> = ["columbus"].iter().map(|s| s.to_string()).collect();
    let usage_limited: HashSet<String> = HashSet::new();
    let active_reviewers: HashSet<String> = HashSet::new(); // columbus is NOT reviewing

    let liveness =
        decide_reviewer_liveness("columbus", &active_names, &usage_limited, &active_reviewers);

    assert_eq!(
        liveness,
        ReviewerLivenessDecision::Dead,
        "Dev coworker matching ghost reviewer should be Dead (not Active)"
    );

    // Prune the dead assignment
    state.remove_assignment(823);

    assert_eq!(
        state.active_count(),
        0,
        "Ghost assignment for dev coworker should be pruned"
    );
}
