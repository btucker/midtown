use super::super::constants::ORPHANED_PR_NUDGE_COOLDOWN_SECS;
use super::*;

// =========================================================================
// Graceful degradation tests: deduplication between webhook and polling
//
// When webhooks ARE working, they fire first and record nudges. Polling
// then sees the nudge is on cooldown and skips duplicate action.
//
// When webhooks are NOT working (degraded), polling is the first to detect
// issues and record nudges. These tests verify both paths use the same
// tracker and respect cooldowns.
// =========================================================================

// -------------------------------------------------------------------------
// PrIssueTracker — prevents double-nudging for PR issues
// -------------------------------------------------------------------------

#[test]
fn tracker_allows_first_nudge() {
    let tracker = PrIssueTracker::new();

    assert!(
        tracker.should_nudge(42, PrIssueType::CiFailed),
        "first nudge for an issue should be allowed"
    );
}

#[test]
fn tracker_blocks_immediate_repeat_nudge() {
    let mut tracker = PrIssueTracker::new();

    // Webhook fires first and records the nudge
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // Polling runs shortly after — should be blocked
    assert!(
        !tracker.should_nudge(42, PrIssueType::CiFailed),
        "immediate repeat nudge should be blocked (webhook then polling)"
    );
}

#[test]
fn tracker_allows_different_issue_types() {
    let mut tracker = PrIssueTracker::new();

    // Webhook records CI failure nudge
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // Different issue type should still be allowed
    assert!(
        tracker.should_nudge(42, PrIssueType::MergeConflict),
        "different issue type on same PR should be allowed"
    );
}

#[test]
fn tracker_allows_different_prs() {
    let mut tracker = PrIssueTracker::new();

    // Webhook records nudge for PR #42
    tracker.record_nudge(42, PrIssueType::Approved);

    // Same issue type on different PR should be allowed
    assert!(
        tracker.should_nudge(43, PrIssueType::Approved),
        "same issue type on different PR should be allowed"
    );
}

#[test]
fn tracker_cleanup_removes_expired() {
    let mut tracker = PrIssueTracker::new();

    // Insert an entry with an expired timestamp
    tracker.nudged.insert(
        (42, PrIssueType::CiFailed),
        Instant::now() - Duration::from_secs(PR_NUDGE_COOLDOWN_SECS + 1),
    );

    tracker.cleanup();

    assert!(
        tracker.nudged.is_empty(),
        "expired entries should be removed by cleanup"
    );
}

// -------------------------------------------------------------------------
// PrIssueTracker — extended cooldown for orphaned PR alerts
// -------------------------------------------------------------------------

/// Bug: Orphaned PR alerts fire every 10 minutes (PR_NUDGE_COOLDOWN_SECS)
/// even though nobody is actively working on them. The cooldown should be
/// longer for orphaned PRs since there's no coworker to address the issue.
#[test]
fn orphaned_pr_alert_suppressed_during_extended_cooldown() {
    let mut tracker = PrIssueTracker::new();

    // Simulate a nudge that happened 11 minutes ago (past standard 10-min cooldown)
    tracker.nudged.insert(
        (42, PrIssueType::MergeConflict),
        Instant::now() - Duration::from_secs(PR_NUDGE_COOLDOWN_SECS + 60),
    );

    // Standard cooldown expired — regular should_nudge would allow it
    assert!(
        tracker.should_nudge(42, PrIssueType::MergeConflict),
        "standard cooldown should have expired after 11 minutes"
    );

    // Extended cooldown for orphaned PRs should still suppress
    assert!(
        !tracker.should_nudge_with_cooldown(
            42,
            PrIssueType::MergeConflict,
            ORPHANED_PR_NUDGE_COOLDOWN_SECS
        ),
        "orphaned PR alert should be suppressed during extended cooldown window"
    );
}

/// After the extended cooldown expires, orphaned PR alerts should fire again.
#[test]
fn orphaned_pr_alert_fires_after_extended_cooldown_expires() {
    let mut tracker = PrIssueTracker::new();

    // Simulate a nudge that happened beyond the extended cooldown
    tracker.nudged.insert(
        (42, PrIssueType::MergeConflict),
        Instant::now() - Duration::from_secs(ORPHANED_PR_NUDGE_COOLDOWN_SECS + 1),
    );

    // Extended cooldown also expired — should allow re-nudging
    assert!(
        tracker.should_nudge_with_cooldown(
            42,
            PrIssueType::MergeConflict,
            ORPHANED_PR_NUDGE_COOLDOWN_SECS
        ),
        "orphaned PR alert should fire after extended cooldown expires"
    );
}

// -------------------------------------------------------------------------
// StuckConditionTracker — polling-only stuck detection
// -------------------------------------------------------------------------

#[test]
fn stuck_tracker_tracks_condition() {
    let mut tracker = StuckConditionTracker::new();

    let first_detected = tracker.track("42", StuckConditionType::NoReview);

    // Should return a reasonable timestamp (not too far in the past)
    assert!(
        first_detected.elapsed() < Duration::from_secs(1),
        "first detected should be approximately now"
    );
}

#[test]
fn stuck_tracker_allows_first_nudge() {
    let mut tracker = StuckConditionTracker::new();

    tracker.track("42", StuckConditionType::NoReview);

    assert!(
        tracker.should_nudge("42", StuckConditionType::NoReview),
        "should allow first nudge for tracked condition"
    );
}

#[test]
fn stuck_tracker_blocks_repeat_nudge() {
    let mut tracker = StuckConditionTracker::new();

    tracker.track("42", StuckConditionType::NoReview);
    tracker.record_nudge("42", StuckConditionType::NoReview);

    assert!(
        !tracker.should_nudge("42", StuckConditionType::NoReview),
        "should block immediate repeat nudge"
    );
}

#[test]
fn stuck_tracker_increments_nudge_count() {
    let mut tracker = StuckConditionTracker::new();

    tracker.track("york", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("york", StuckConditionType::SilentCoworker),
        0
    );

    tracker.record_nudge("york", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("york", StuckConditionType::SilentCoworker),
        1
    );

    // Manually reset cooldown to allow another nudge
    if let Some(entry) = tracker
        .conditions
        .get_mut(&("york".to_string(), StuckConditionType::SilentCoworker))
    {
        entry.1 = Some(Instant::now() - Duration::from_secs(STUCK_NUDGE_COOLDOWN_SECS + 1));
    }

    tracker.record_nudge("york", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("york", StuckConditionType::SilentCoworker),
        2,
        "nudge count should escalate for repeated stuck conditions"
    );
}

#[test]
fn stuck_tracker_clear_removes_condition() {
    let mut tracker = StuckConditionTracker::new();

    tracker.track("42", StuckConditionType::MergeReady);
    tracker.record_nudge("42", StuckConditionType::MergeReady);

    tracker.clear("42", StuckConditionType::MergeReady);

    assert!(
        !tracker.should_nudge("42", StuckConditionType::MergeReady),
        "cleared condition should not be nudgeable (not tracked)"
    );

    // But if we track it again, it should be fresh
    tracker.track("42", StuckConditionType::MergeReady);
    assert!(
        tracker.should_nudge("42", StuckConditionType::MergeReady),
        "re-tracked condition should be nudgeable again"
    );
}

// -------------------------------------------------------------------------
// OrphanTracker — orphaned worktree detection (polling-only)
// -------------------------------------------------------------------------

#[test]
fn orphan_tracker_blocks_warn_during_grace_period() {
    // Bug: orphan detection runs every 10s, PR poll every 30s.
    // If a coworker opens a PR and goes idle, the orphan check can fire
    // before the PR poll updates open_pr_owners, causing a false positive.
    // Fix: don't warn until grace period has elapsed since first detection.
    let mut tracker = OrphanTracker::new();

    tracker.track("lexington".to_string());

    assert!(
        !tracker.should_warn("lexington"),
        "should NOT warn during grace period after first detection"
    );
}

#[test]
fn orphan_tracker_allows_warn_after_grace_period() {
    let mut tracker = OrphanTracker::new();

    // Simulate detection that happened long ago (beyond grace period)
    tracker.entries.insert(
        "lexington".to_string(),
        OrphanEntry {
            first_detected: Instant::now() - ORPHAN_INITIAL_GRACE_PERIOD - Duration::from_secs(1),
            warned_at: None,
        },
    );

    assert!(
        tracker.should_warn("lexington"),
        "should allow warning after grace period"
    );
}

#[test]
fn orphan_tracker_blocks_repeat_warn() {
    let mut tracker = OrphanTracker::new();

    tracker.track("lexington".to_string());
    tracker.record_warn("lexington");

    assert!(
        !tracker.should_warn("lexington"),
        "should block immediate repeat warning"
    );
}

#[test]
fn orphan_tracker_prune_with_full_orphan_list_preserves_warned_at() {
    // Regression test for: orphan warning repeating every tick.
    //
    // Bug: gather_orphan_cleanup_data() called prune(&unmerged) where
    // `unmerged` was a SUBSET of all orphans (capped at 2 per tick, then
    // filtered by open PRs). This dropped tracker entries for orphans not
    // in the current batch, losing their warned_at state and causing
    // repeat warnings after the grace period.
    //
    // Fix: prune with the FULL orphan list (all_orphaned) so entries are
    // only removed when the worktree is no longer orphaned.
    let mut tracker = OrphanTracker::new();

    // Simulate: amsterdam detected past grace period and warned
    let warned_time = Instant::now() - Duration::from_secs(5); // warned 5s ago
    tracker.entries.insert(
        "amsterdam".to_string(),
        OrphanEntry {
            first_detected: Instant::now() - ORPHAN_INITIAL_GRACE_PERIOD - Duration::from_secs(300),
            warned_at: Some(warned_time),
        },
    );

    // Verify: warning is suppressed (warned recently, cooldown not elapsed)
    assert!(
        !tracker.should_warn("amsterdam"),
        "precondition: warning should be suppressed by recent warned_at"
    );

    // Tick N+1: amsterdam is still orphaned (in all_orphaned) but was NOT
    // in the 2-item processing batch this tick, so unmerged is empty.
    // FIX: prune with all_orphaned (which INCLUDES amsterdam), not unmerged.
    let all_orphaned = vec!["amsterdam".to_string()]; // still orphaned
    tracker.prune(&all_orphaned);

    // Entry should be preserved since amsterdam is in the prune list
    assert!(
        tracker.entries.contains_key("amsterdam"),
        "entry should be preserved — amsterdam is still orphaned"
    );

    // Tick N+2: amsterdam IS in the processing batch again.
    tracker.track("amsterdam".to_string()); // no-op, already exists

    // Simulate grace period having elapsed
    tracker.entries.get_mut("amsterdam").unwrap().first_detected =
        Instant::now() - ORPHAN_INITIAL_GRACE_PERIOD - Duration::from_secs(1);

    // should_warn returns false because warned_at was preserved
    assert!(
        !tracker.should_warn("amsterdam"),
        "should NOT warn again — warned_at was preserved by prune(all_orphaned)"
    );
}

#[test]
fn orphan_tracker_prune_with_subset_drops_warned_at_regression() {
    // Demonstrates the bug that was fixed: pruning with a subset loses state.
    // This test documents the problematic behavior to prevent regression.
    let mut tracker = OrphanTracker::new();

    // amsterdam was warned 5s ago
    tracker.entries.insert(
        "amsterdam".to_string(),
        OrphanEntry {
            first_detected: Instant::now() - ORPHAN_INITIAL_GRACE_PERIOD - Duration::from_secs(300),
            warned_at: Some(Instant::now() - Duration::from_secs(5)),
        },
    );

    // Pruning with empty subset (simulating amsterdam not in this tick's batch)
    // drops the entry — this was the root cause of the bug.
    tracker.prune(&[]);
    assert!(
        !tracker.entries.contains_key("amsterdam"),
        "prune with empty list drops all entries (expected behavior of prune)"
    );

    // Re-tracking creates a fresh entry with warned_at: None
    tracker.track("amsterdam".to_string());
    tracker.entries.get_mut("amsterdam").unwrap().first_detected =
        Instant::now() - ORPHAN_INITIAL_GRACE_PERIOD - Duration::from_secs(1);

    // This would cause a repeat warning — the bug behavior we fixed at the call site
    assert!(
        tracker.should_warn("amsterdam"),
        "fresh entry after prune+track has no warned_at, so should_warn is true (the bug)"
    );
}

#[test]
fn orphan_tracker_prune_removes_resolved() {
    let mut tracker = OrphanTracker::new();

    tracker.track("lexington".to_string());
    tracker.track("amsterdam".to_string());

    // Lexington's worktree is restored — no longer flagged
    tracker.prune(&["amsterdam".to_string()]);

    assert!(tracker.entries.contains_key("amsterdam"));
    assert!(
        !tracker.entries.contains_key("lexington"),
        "pruned orphan should be removed"
    );
}

// -------------------------------------------------------------------------
// CommentTracker — polling fallback for review comment notifications
// -------------------------------------------------------------------------

#[test]
fn comment_tracker_detects_new_comments() {
    let mut tracker = CommentTracker::new();

    // First poll: PR #42 has 2 non-owner comments
    assert!(
        tracker.has_new_comments(42, 2),
        "first poll with comments should return true"
    );
    tracker.record(42, 2);

    // Second poll: same count
    assert!(
        !tracker.has_new_comments(42, 2),
        "same count should return false"
    );

    // Third poll: count increased
    assert!(
        tracker.has_new_comments(42, 3),
        "increased count should return true"
    );
}

#[test]
fn comment_tracker_returns_false_for_new_pr_with_no_comments() {
    let tracker = CommentTracker::new();

    // New PR with 0 comments — no new activity
    assert!(
        !tracker.has_new_comments(42, 0),
        "new PR with no comments should return false"
    );
}

#[test]
fn comment_tracker_cleanup_removes_closed_prs() {
    let mut tracker = CommentTracker::new();

    tracker.record(42, 5);
    tracker.record(43, 3);
    tracker.record(44, 1);

    // Only PRs 42 and 44 are still open
    tracker.cleanup(&[42, 44]);

    assert!(tracker.comment_counts.contains_key(&42));
    assert!(!tracker.comment_counts.contains_key(&43));
    assert!(tracker.comment_counts.contains_key(&44));
}

// -------------------------------------------------------------------------
// Integration scenario: webhook fires before polling
// -------------------------------------------------------------------------

#[test]
fn webhook_before_polling_prevents_duplicate() {
    let mut tracker = PrIssueTracker::new();

    // Scenario: PR #42 gets CI failure
    // 1. Webhook fires and nudges owner
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // 2. ~30s later, polling runs and detects the same issue
    // Polling should see the cooldown and skip
    assert!(
        !tracker.should_nudge(42, PrIssueType::CiFailed),
        "polling should skip when webhook already handled the issue"
    );
}

// -------------------------------------------------------------------------
// Integration scenario: webhook degraded, polling takes over
// -------------------------------------------------------------------------

#[test]
fn polling_handles_issue_when_webhook_missing() {
    let mut tracker = PrIssueTracker::new();

    // Scenario: Webhook is degraded, polling is first to detect CI failure
    // 1. Polling detects issue (no prior webhook)
    assert!(
        tracker.should_nudge(42, PrIssueType::CiFailed),
        "polling should handle issue when webhook hasn't fired"
    );

    // 2. Polling records the nudge
    tracker.record_nudge(42, PrIssueType::CiFailed);

    // 3. Next polling cycle should be blocked
    assert!(
        !tracker.should_nudge(42, PrIssueType::CiFailed),
        "repeat polling should be blocked after first handled"
    );
}

// -------------------------------------------------------------------------
// CiNotificationBuffer — batches CI check notifications
// -------------------------------------------------------------------------

#[test]
fn ci_buffer_batches_checks_by_target() {
    let mut buffer = CiNotificationBuffer::new();

    // Add checks for two different targets
    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "main".to_string(),
        mention_prefix: "".to_string(),
    });
    buffer.add(CiCheckPassed {
        check_name: "Test".to_string(),
        target: "main".to_string(),
        mention_prefix: "".to_string(),
    });
    buffer.add(CiCheckPassed {
        check_name: "Build".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "@columbus ".to_string(),
    });

    // Buffer should not flush immediately
    assert!(
        !buffer.should_flush(),
        "buffer should not flush immediately"
    );

    // Force flush by simulating time passing
    buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));

    assert!(buffer.should_flush(), "buffer should flush after delay");

    let batched = buffer.flush();
    assert_eq!(
        batched.len(),
        2,
        "should have 2 batched notifications (one per target)"
    );

    // Find the "main" batch
    let main_batch = batched.iter().find(|b| b.target == "main").unwrap();
    assert_eq!(main_batch.check_names.len(), 2);
    assert!(main_batch.check_names.contains(&"Clippy".to_string()));
    assert!(main_batch.check_names.contains(&"Test".to_string()));
    assert_eq!(main_batch.mention_prefix, "");

    // Find the "PR #42" batch
    let pr_batch = batched.iter().find(|b| b.target == "PR #42").unwrap();
    assert_eq!(pr_batch.check_names.len(), 1);
    assert!(pr_batch.check_names.contains(&"Build".to_string()));
    assert_eq!(pr_batch.mention_prefix, "@columbus ");
}

#[test]
fn ci_buffer_clears_after_flush() {
    let mut buffer = CiNotificationBuffer::new();

    buffer.add(CiCheckPassed {
        check_name: "Test".to_string(),
        target: "main".to_string(),
        mention_prefix: "".to_string(),
    });

    // Force flush
    buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));
    let _ = buffer.flush();

    assert!(buffer.is_empty(), "buffer should be empty after flush");
    assert!(
        !buffer.should_flush(),
        "should_flush should be false after flush"
    );
}

#[test]
fn ci_buffer_single_check_returns_single_result() {
    let mut buffer = CiNotificationBuffer::new();

    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "main".to_string(),
        mention_prefix: "".to_string(),
    });

    // Force flush
    buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));
    let batched = buffer.flush();

    assert_eq!(batched.len(), 1);
    assert_eq!(batched[0].check_names.len(), 1);
    assert_eq!(batched[0].check_names[0], "Clippy");
}

// -------------------------------------------------------------------------
// format_batched_ci_notification tests
// -------------------------------------------------------------------------

#[test]
fn format_batched_ci_single_check() {
    let batch = BatchedCiNotification {
        target: "main".to_string(),
        mention_prefix: "".to_string(),
        check_names: vec!["Clippy".to_string()],
    };
    let msg = format_batched_ci_notification(&batch);
    assert_eq!(msg, "Check 'Clippy' passed on main");
}

#[test]
fn format_batched_ci_single_check_with_mention() {
    let batch = BatchedCiNotification {
        target: "PR #42".to_string(),
        mention_prefix: "@columbus ".to_string(),
        check_names: vec!["Build".to_string()],
    };
    let msg = format_batched_ci_notification(&batch);
    assert_eq!(msg, "@columbus Check 'Build' passed on PR #42");
}

#[test]
fn format_batched_ci_multiple_checks() {
    let batch = BatchedCiNotification {
        target: "main".to_string(),
        mention_prefix: "".to_string(),
        check_names: vec![
            "Clippy".to_string(),
            "Test".to_string(),
            "E2E - foo".to_string(),
        ],
    };
    let msg = format_batched_ci_notification(&batch);
    assert_eq!(msg, "3 checks passed on main");
}

#[test]
fn format_batched_ci_multiple_checks_with_mention() {
    let batch = BatchedCiNotification {
        target: "PR #99".to_string(),
        mention_prefix: "@park ".to_string(),
        check_names: vec!["Build".to_string(), "Test".to_string()],
    };
    let msg = format_batched_ci_notification(&batch);
    assert_eq!(msg, "@park 2 checks passed on PR #99");
}

#[test]
fn format_batched_ci_many_checks_omits_names() {
    let batch = BatchedCiNotification {
        target: "PR #42".to_string(),
        mention_prefix: "@madison ".to_string(),
        check_names: vec![
            "Clippy".to_string(),
            "Test".to_string(),
            "E2E - chat_e2e".to_string(),
            "E2E - daemon_e2e".to_string(),
            "Format".to_string(),
        ],
    };
    let msg = format_batched_ci_notification(&batch);
    // Should NOT list individual check names — just the count
    assert_eq!(msg, "@madison 5 checks passed on PR #42");
    assert!(!msg.contains("Clippy"));
    assert!(!msg.contains("E2E"));
}

#[test]
fn ci_buffer_deduplicates_check_names() {
    let mut buffer = CiNotificationBuffer::new();

    // Add the same check name twice (e.g., from multiple workflow runs)
    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "@columbus ".to_string(),
    });
    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "@columbus ".to_string(),
    });
    buffer.add(CiCheckPassed {
        check_name: "Test".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "@columbus ".to_string(),
    });

    // Force flush
    buffer.oldest_entry = Some(Instant::now() - Duration::from_secs(20));
    let batched = buffer.flush();

    assert_eq!(batched.len(), 1);
    // Should have 2 unique checks, not 3
    assert_eq!(batched[0].check_names.len(), 2);
    assert!(batched[0].check_names.contains(&"Clippy".to_string()));
    assert!(batched[0].check_names.contains(&"Test".to_string()));
}

#[test]
fn ci_buffer_oldest_entry_only_set_on_actual_add() {
    // Defensive: oldest_entry should only be set when an entry is actually
    // added, not when a duplicate is skipped.
    let mut buffer = CiNotificationBuffer::new();

    // Add a check — oldest_entry should be set
    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "".to_string(),
    });
    assert!(buffer.oldest_entry.is_some());

    // Manually clear oldest_entry to simulate an edge case
    buffer.oldest_entry = None;

    // Add duplicate — oldest_entry should NOT be set since no entry was added
    buffer.add(CiCheckPassed {
        check_name: "Clippy".to_string(),
        target: "PR #42".to_string(),
        mention_prefix: "".to_string(),
    });
    assert!(
        buffer.oldest_entry.is_none(),
        "oldest_entry should not be set when duplicate is skipped"
    );
}
