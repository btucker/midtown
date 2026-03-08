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

    // Insert an entry past the longest cooldown (orphaned PR = 30 min)
    tracker.nudged.insert(
        (42, PrIssueType::CiFailed),
        Instant::now() - Duration::from_secs(ORPHANED_PR_NUDGE_COOLDOWN_SECS + 1),
    );

    tracker.cleanup();

    assert!(
        tracker.nudged.is_empty(),
        "expired entries should be removed by cleanup"
    );
}

#[test]
fn tracker_cleanup_retains_entries_within_orphaned_cooldown() {
    let mut tracker = PrIssueTracker::new();

    // Insert an entry past standard cooldown but within orphaned cooldown
    tracker.nudged.insert(
        (42, PrIssueType::MergeConflict),
        Instant::now() - Duration::from_secs(PR_NUDGE_COOLDOWN_SECS + 60),
    );

    tracker.cleanup();

    assert!(
        !tracker.nudged.is_empty(),
        "entries within orphaned cooldown should be retained by cleanup"
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

    assert!(
        buffer.pending.is_empty(),
        "buffer should be empty after flush"
    );
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

// -------------------------------------------------------------------------
// PrIssueTracker — permanent nudge persistence across restarts
// -------------------------------------------------------------------------

#[test]
fn permanent_nudge_survives_simulated_restart() {
    // Step 1: Record a permanent nudge (simulates daemon session)
    let mut tracker = PrIssueTracker::new();
    tracker.record_permanent_nudge(1838, PrIssueType::ReviewComplete);

    assert!(
        tracker.has_nudge(1838, PrIssueType::ReviewComplete),
        "permanent nudge should be recorded"
    );

    // Step 2: Persist to DaemonPersistentState (simulates save_for_repo)
    let persisted: Vec<(u64, PrIssueType)> = tracker.permanent_nudges().iter().cloned().collect();
    assert_eq!(
        persisted.len(),
        1,
        "should have 1 permanent nudge to persist"
    );

    // Step 3: Simulate restart — create new tracker from persisted data
    let restored_tracker = PrIssueTracker::with_permanent_nudges(persisted.into_iter().collect());

    assert!(
        restored_tracker.has_nudge(1838, PrIssueType::ReviewComplete),
        "permanent nudge should survive restart via persistence"
    );

    // The restored tracker should NOT allow re-nudging (has_nudge blocks it)
    // This is the key invariant: the daemon won't re-send the notification
}

#[test]
fn permanent_nudge_serialization_roundtrip() {
    // Verify PrIssueType serializes/deserializes correctly for persistence
    let nudges = vec![
        (42u64, PrIssueType::ReviewComplete),
        (99u64, PrIssueType::ReviewComplete),
    ];

    let json = serde_json::to_string(&nudges).expect("serialize");
    let deserialized: Vec<(u64, PrIssueType)> = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.len(), 2);
    assert_eq!(deserialized[0], (42, PrIssueType::ReviewComplete));
    assert_eq!(deserialized[1], (99, PrIssueType::ReviewComplete));
}

#[test]
fn permanent_nudge_clear_removes_from_set() {
    let mut tracker = PrIssueTracker::new();
    tracker.record_permanent_nudge(42, PrIssueType::ReviewComplete);

    assert!(tracker.has_nudge(42, PrIssueType::ReviewComplete));
    assert_eq!(tracker.permanent_nudges().len(), 1);

    tracker.clear_nudge(42, PrIssueType::ReviewComplete);

    assert!(!tracker.has_nudge(42, PrIssueType::ReviewComplete));
    assert!(tracker.permanent_nudges().is_empty());
}

#[test]
fn with_permanent_nudges_blocks_should_nudge() {
    // Regression: with_permanent_nudges() must populate the nudged map so that
    // should_nudge() returns false for restored permanent entries. Without this,
    // permanent nudges would re-fire after daemon restart because should_nudge()
    // only checks the nudged map.
    let mut nudges = HashSet::new();
    nudges.insert((1838, PrIssueType::ReviewComplete));

    let tracker = PrIssueTracker::with_permanent_nudges(nudges);

    assert!(
        !tracker.should_nudge(1838, PrIssueType::ReviewComplete),
        "should_nudge() must return false for restored permanent nudges"
    );
}

// -------------------------------------------------------------------------
// StuckConditionType::AutoMerge — deduplication for auto-merge attempts
// -------------------------------------------------------------------------

#[test]
fn auto_merge_condition_allows_first_attempt() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::AutoMerge);
    assert!(
        tracker.should_nudge("42", StuckConditionType::AutoMerge),
        "first auto-merge attempt should be allowed"
    );
}

#[test]
fn auto_merge_condition_blocks_immediate_repeat() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::AutoMerge);
    tracker.record_nudge("42", StuckConditionType::AutoMerge);
    assert!(
        !tracker.should_nudge("42", StuckConditionType::AutoMerge),
        "immediate repeat auto-merge should be blocked by cooldown"
    );
}

#[test]
fn auto_merge_condition_independent_of_merge_ready() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::AutoMerge);
    tracker.record_nudge("42", StuckConditionType::AutoMerge);

    // MergeReady should still be allowed — the conditions are independent
    tracker.track("42", StuckConditionType::MergeReady);
    assert!(
        tracker.should_nudge("42", StuckConditionType::MergeReady),
        "MergeReady nudge should be independent of AutoMerge cooldown"
    );
}

#[test]
fn auto_merge_condition_cleared_when_pr_no_longer_mergeable() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::AutoMerge);
    tracker.record_nudge("42", StuckConditionType::AutoMerge);

    // Clear (PR is no longer auto-mergeable)
    tracker.clear("42", StuckConditionType::AutoMerge);

    // Re-track — should allow again since the condition was cleared
    tracker.track("42", StuckConditionType::AutoMerge);
    assert!(
        tracker.should_nudge("42", StuckConditionType::AutoMerge),
        "auto-merge should be allowed after condition was cleared and re-tracked"
    );
}

#[test]
fn cleanup_retains_nudged_entries_past_first_detected_cutoff() {
    // Verifies that cleanup() retains entries where last_nudged is recent
    // even if first_detected has aged beyond the cutoff. This prevents
    // AutoMerge (and other one-shot conditions) from re-firing after cleanup.
    let mut tracker = StuckConditionTracker::new();

    // Manually insert an entry with an old first_detected but recent last_nudged.
    // The cutoff is STUCK_NUDGE_COOLDOWN_SECS * 2 = 60 min.
    // Simulate: first_detected 90 min ago, last_nudged 5 min ago.
    let old_first = Instant::now() - Duration::from_secs(90 * 60);
    let recent_nudge = Instant::now() - Duration::from_secs(5 * 60);
    tracker.conditions.insert(
        ("42".to_string(), StuckConditionType::AutoMerge),
        (old_first, Some(recent_nudge), 1),
    );

    tracker.cleanup();

    // Entry should be retained because last_nudged is recent
    assert!(
        !tracker.should_nudge("42", StuckConditionType::AutoMerge),
        "nudged AutoMerge entry should survive cleanup and remain on cooldown"
    );
}

#[test]
fn cleanup_evicts_entries_where_both_timestamps_are_old() {
    let mut tracker = StuckConditionTracker::new();

    // Both first_detected and last_nudged are older than the cutoff
    let old = Instant::now() - Duration::from_secs(90 * 60);
    tracker.conditions.insert(
        ("42".to_string(), StuckConditionType::MergeReady),
        (old, Some(old), 2),
    );

    tracker.cleanup();

    // Entry should be evicted — both timestamps are beyond cutoff
    assert!(
        !tracker.should_nudge("42", StuckConditionType::MergeReady),
        "fully stale entry should be evicted by cleanup (should_nudge returns false for untracked)"
    );
}
