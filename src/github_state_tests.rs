use super::*;
use tempfile::tempdir;

#[test]
fn test_state_default() {
    let state = GitHubState::default();
    assert!(state.pr_reviewers.is_empty());
}

#[test]
fn test_assign_reviewer() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

    assert!(state.is_assigned(42));
    assert_eq!(state.get_reviewer(42), Some("lexington"));
    assert!(!state.is_assigned(43));
}

#[test]
fn test_remove_assignment() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

    let removed = state.remove_assignment(42);
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().reviewer, "lexington");
    assert!(!state.is_assigned(42));
}

#[test]
fn test_assigned_reviewers() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    let reviewers: Vec<_> = state.assigned_reviewers().collect();
    assert_eq!(reviewers.len(), 2);
    assert!(reviewers.contains(&"lexington"));
    assert!(reviewers.contains(&"park"));
}

#[test]
fn test_pr_for_reviewer() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    assert_eq!(state.pr_for_reviewer("lexington"), Some(42));
    assert_eq!(state.pr_for_reviewer("park"), Some(43));
    assert_eq!(state.pr_for_reviewer("york"), None);

    // After removal, should return None
    state.remove_assignment(42);
    assert_eq!(state.pr_for_reviewer("lexington"), None);
}

#[test]
fn test_remove_assignment_by_reviewer() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    // Remove lexington's assignment by name
    let removed = state.remove_assignment_by_reviewer("lexington");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().pr_number, 42);

    // Verify it's gone
    assert!(state.pr_for_reviewer("lexington").is_none());
    assert!(state.get_reviewer(42).is_none());

    // park's assignment should be unaffected
    assert_eq!(state.pr_for_reviewer("park"), Some(43));

    // Removing non-existent reviewer returns None
    assert!(state.remove_assignment_by_reviewer("york").is_none());
}

#[test]
fn test_cleanup_closed_prs() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
    state.assign_reviewer(44, "york", AssignmentSource::PollingFallback);

    // Only PR 42 and 44 are still open
    state.cleanup_closed_prs(&[42, 44]);

    assert!(state.is_assigned(42));
    assert!(!state.is_assigned(43)); // cleaned up
    assert!(state.is_assigned(44));
}

#[test]
fn test_save_and_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(loaded.pr_reviewers.len(), 2);
    assert_eq!(loaded.get_reviewer(42), Some("lexington"));
    assert_eq!(loaded.get_reviewer(43), Some("park"));
}

#[test]
fn test_load_nonexistent_returns_default() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");

    let state = GitHubState::load(&path).unwrap();
    assert!(state.pr_reviewers.is_empty());
}

#[test]
fn test_is_assigned_expires_after_timeout() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);

    // Fresh assignment should be considered assigned
    assert!(state.is_assigned(42));

    // Manually backdate the assignment to exceed the timeout
    if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
        assignment.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // Expired assignment should NOT be considered assigned
    assert!(
        !state.is_assigned(42),
        "Expired persistent assignment should not be considered assigned"
    );
}

#[test]
fn test_cleanup_expired_assignments() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    // Backdate PR 42's assignment past the timeout
    if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
        assignment.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    state.cleanup_expired_assignments();

    // PR 42 should be removed (expired), PR 43 should remain (fresh)
    assert!(!state.pr_reviewers.contains_key(&42));
    assert!(state.pr_reviewers.contains_key(&43));
}

#[test]
fn test_cleanup_expired_preserves_active_coworkers() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    // Backdate broadway's assignment past the timeout
    if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
        assignment.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // broadway is still active
    let active: std::collections::HashSet<String> = ["broadway".to_string()].into_iter().collect();

    state.cleanup_expired_preserving(&active, None);

    // broadway's expired assignment should be preserved (still active coworker)
    assert!(state.pr_reviewers.contains_key(&42));
    // park's fresh assignment should also be there
    assert!(state.pr_reviewers.contains_key(&43));
}

#[test]
fn test_cleanup_expired_removes_inactive_expired() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);

    // Backdate assignment past timeout
    if let Some(assignment) = state.pr_reviewers.get_mut(&42) {
        assignment.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // broadway is NOT active
    let active: std::collections::HashSet<String> = std::collections::HashSet::new();

    state.cleanup_expired_preserving(&active, None);

    // Should be removed (expired + inactive)
    assert!(!state.pr_reviewers.contains_key(&42));
}

#[test]
fn test_cleanup_expired_preserving_with_session_ids() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "broadway", AssignmentSource::Webhook);

    // Set session IDs: PR 42 has session "sess-review", PR 43 has session "sess-dev"
    state.pr_reviewers.get_mut(&42).unwrap().reviewer_session_id = Some("sess-review".to_string());
    state.pr_reviewers.get_mut(&43).unwrap().reviewer_session_id = Some("sess-dev".to_string());

    // Backdate both past timeout
    for pr in [42, 43] {
        state.pr_reviewers.get_mut(&pr).unwrap().assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // broadway is in running_coworkers by name, but only sess-review is a running session
    let running_names: std::collections::HashSet<String> =
        ["broadway".to_string()].into_iter().collect();
    let running_sessions: std::collections::HashSet<String> =
        ["sess-review".to_string()].into_iter().collect();

    state.cleanup_expired_preserving(&running_names, Some(&running_sessions));

    // PR 42 (sess-review) should be preserved — session is running
    assert!(
        state.pr_reviewers.contains_key(&42),
        "Assignment with running session ID should be preserved"
    );
    // PR 43 (sess-dev) should be removed — session is NOT running
    assert!(
        !state.pr_reviewers.contains_key(&43),
        "Assignment with non-running session ID should be cleaned up"
    );
}

#[test]
fn test_cleanup_expired_preserving_falls_back_to_name_without_session_id() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "broadway", AssignmentSource::PollingFallback);
    // No reviewer_session_id set (legacy assignment)

    // Backdate past timeout
    state.pr_reviewers.get_mut(&42).unwrap().assigned_at =
        Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);

    // broadway is running by name; session IDs provided but assignment has none
    let running_names: std::collections::HashSet<String> =
        ["broadway".to_string()].into_iter().collect();
    let running_sessions: std::collections::HashSet<String> =
        ["some-other-session".to_string()].into_iter().collect();

    state.cleanup_expired_preserving(&running_names, Some(&running_sessions));

    // Should be preserved via name fallback (no session_id on assignment)
    assert!(
        state.pr_reviewers.contains_key(&42),
        "Legacy assignment without session_id should fall back to name matching"
    );
}

#[test]
fn test_reviewer_session_id_persists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);
    state.pr_reviewers.get_mut(&42).unwrap().reviewer_session_id = Some("sess-abc-123".to_string());

    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    let assignment = loaded.pr_reviewers.get(&42).unwrap();
    assert_eq!(
        assignment.reviewer_session_id.as_deref(),
        Some("sess-abc-123")
    );
}

#[test]
fn test_active_count() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    assert_eq!(state.active_count(), 2);

    // Expire one assignment
    if let Some(a) = state.pr_reviewers.get_mut(&42) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    assert_eq!(state.active_count(), 1);
}

#[test]
fn test_active_reviewers() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
    state.assign_reviewer(44, "lexington", AssignmentSource::Webhook); // duplicate reviewer name

    let reviewers = state.active_reviewers();
    assert!(reviewers.contains("lexington"));
    assert!(reviewers.contains("park"));
    assert_eq!(reviewers.len(), 2); // deduped

    // Expire lexington's assignment on PR 44
    if let Some(a) = state.pr_reviewers.get_mut(&44) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    // lexington still has PR 42 (fresh)
    let reviewers = state.active_reviewers();
    assert!(reviewers.contains("lexington"));
    assert!(reviewers.contains("park"));
}

#[test]
fn test_active_assignments() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    let assignments = state.active_assignments();
    assert_eq!(assignments.len(), 2);
    assert_eq!(assignments[&42].reviewer, "lexington");
    assert_eq!(assignments[&43].reviewer, "park");

    // Expire one
    if let Some(a) = state.pr_reviewers.get_mut(&42) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    let assignments = state.active_assignments();
    assert_eq!(assignments.len(), 1);
    assert!(!assignments.contains_key(&42));
    assert!(assignments.contains_key(&43));
}

#[test]
fn test_backfill_reviewer_session_ids() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);

    // Initially, no session IDs
    assert!(state.pr_reviewers[&42].reviewer_session_id.is_none());
    assert!(state.pr_reviewers[&43].reviewer_session_id.is_none());

    // Backfill with session IDs from running coworkers
    let mut session_map = std::collections::HashMap::new();
    session_map.insert("lexington".to_string(), "sess-abc".to_string());
    // park is not in the map (session not yet initialized)

    state.backfill_reviewer_session_ids(&session_map);

    // lexington should have session_id backfilled
    assert_eq!(
        state.pr_reviewers[&42].reviewer_session_id,
        Some("sess-abc".to_string())
    );
    // park should still be None
    assert!(state.pr_reviewers[&43].reviewer_session_id.is_none());

    // Second backfill with park's session
    let mut session_map2 = std::collections::HashMap::new();
    session_map2.insert("lexington".to_string(), "sess-abc".to_string());
    session_map2.insert("park".to_string(), "sess-def".to_string());

    state.backfill_reviewer_session_ids(&session_map2);

    // Both should now have session IDs
    assert_eq!(
        state.pr_reviewers[&42].reviewer_session_id,
        Some("sess-abc".to_string())
    );
    assert_eq!(
        state.pr_reviewers[&43].reviewer_session_id,
        Some("sess-def".to_string())
    );

    // Already-set session IDs should not be overwritten
    let mut session_map3 = std::collections::HashMap::new();
    session_map3.insert("lexington".to_string(), "sess-NEW".to_string());

    state.backfill_reviewer_session_ids(&session_map3);

    // lexington keeps original session_id (not overwritten)
    assert_eq!(
        state.pr_reviewers[&42].reviewer_session_id,
        Some("sess-abc".to_string())
    );
}

#[test]
fn test_add_pending_review_spawn() {
    let mut state = GitHubState::default();
    let future = Utc::now() + chrono::Duration::seconds(60);

    state.add_pending_review_spawn(42, future);
    assert_eq!(state.pending_review_spawns.len(), 1);
    assert_eq!(state.pending_review_spawns[0].pr_number, 42);

    // Duplicate should be ignored
    state.add_pending_review_spawn(42, future);
    assert_eq!(state.pending_review_spawns.len(), 1);

    // Different PR should be added
    state.add_pending_review_spawn(43, future);
    assert_eq!(state.pending_review_spawns.len(), 2);
}

#[test]
fn test_drain_ready_review_spawns() {
    let mut state = GitHubState::default();
    let past = Utc::now() - chrono::Duration::seconds(10);
    let future = Utc::now() + chrono::Duration::seconds(60);

    state.add_pending_review_spawn(42, past);
    state.add_pending_review_spawn(43, future);
    state.add_pending_review_spawn(44, past);

    let ready = state.drain_ready_review_spawns();
    assert_eq!(ready.len(), 2);
    assert!(ready.contains(&42));
    assert!(ready.contains(&44));

    // Only the future spawn should remain
    assert_eq!(state.pending_review_spawns.len(), 1);
    assert_eq!(state.pending_review_spawns[0].pr_number, 43);
}

#[test]
fn test_pending_review_spawns_persist() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    let future = Utc::now() + chrono::Duration::seconds(60);
    state.add_pending_review_spawn(42, future);
    state.add_pending_review_spawn(43, future);
    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(loaded.pending_review_spawns.len(), 2);
    assert_eq!(loaded.pending_review_spawns[0].pr_number, 42);
    assert_eq!(loaded.pending_review_spawns[1].pr_number, 43);
}

#[test]
fn test_assignment_source_persists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(
        loaded.get_assignment_source(42),
        Some(AssignmentSource::Webhook)
    );
    assert_eq!(
        loaded.get_assignment_source(43),
        Some(AssignmentSource::PollingFallback)
    );
}

#[test]
fn test_webhook_recently_handled() {
    let mut state = GitHubState::default();

    // No event recorded yet
    assert!(!state.webhook_recently_handled(42, 120));

    // Record an event
    state.record_webhook_event(42);
    assert!(state.webhook_recently_handled(42, 120));
    assert!(!state.webhook_recently_handled(43, 120));
}

#[test]
fn test_webhook_recently_handled_expired() {
    let mut state = GitHubState::default();

    // Manually backdate the event
    state
        .pr_last_webhook_event
        .insert(42, Utc::now() - chrono::Duration::seconds(300));

    // 120s window should not match a 300s-old event
    assert!(!state.webhook_recently_handled(42, 120));
    // 600s window should match
    assert!(state.webhook_recently_handled(42, 600));
}

#[test]
fn test_count_by_source() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);
    state.assign_reviewer(43, "park", AssignmentSource::PollingFallback);
    state.assign_reviewer(44, "york", AssignmentSource::Webhook);

    let counts = state.count_by_source();
    assert_eq!(counts.get("webhook"), Some(&2));
    assert_eq!(counts.get("polling"), Some(&1));
}

#[test]
fn test_cleanup_stale_webhook_events() {
    let mut state = GitHubState::default();

    // Fresh event
    state.record_webhook_event(42);
    // Stale event (2 hours ago)
    state
        .pr_last_webhook_event
        .insert(43, Utc::now() - chrono::Duration::seconds(7200));

    state.cleanup_stale_webhook_events();

    assert!(state.pr_last_webhook_event.contains_key(&42));
    assert!(!state.pr_last_webhook_event.contains_key(&43));
}

#[test]
fn test_cleanup_closed_prs_cleans_webhook_events() {
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::PollingFallback);
    state.record_webhook_event(42);
    state.record_webhook_event(43);

    // Only PR 42 is still open
    state.cleanup_closed_prs(&[42]);

    assert!(state.pr_last_webhook_event.contains_key(&42));
    assert!(!state.pr_last_webhook_event.contains_key(&43));
}

#[test]
fn test_assign_reviewer_with_event_id() {
    let mut state = GitHubState::default();
    state.assign_reviewer_with_event_id(
        42,
        "lexington",
        AssignmentSource::Webhook,
        Some("delivery-abc123".to_string()),
    );

    let assignment = state.pr_reviewers.get(&42).unwrap();
    assert_eq!(assignment.source, AssignmentSource::Webhook);
    assert_eq!(
        assignment.webhook_event_id.as_deref(),
        Some("delivery-abc123")
    );
}

#[test]
fn test_default_source_for_legacy_data() {
    // Simulate loading legacy data without the source field
    let json = r#"{
        "pr_reviewers": {
            "42": {
                "pr_number": 42,
                "reviewer": "lexington",
                "assigned_at": "2025-01-01T00:00:00Z"
            }
        }
    }"#;

    let state: GitHubState = serde_json::from_str(json).unwrap();
    let assignment = state.pr_reviewers.get(&42).unwrap();
    // Legacy data defaults to PollingFallback
    assert_eq!(assignment.source, AssignmentSource::PollingFallback);
    assert!(assignment.webhook_event_id.is_none());
}

#[test]
fn test_store_pr_author_session() {
    let mut state = GitHubState::default();
    state.store_pr_author_session(
        42,
        "session-abc-123",
        "lexington/feature",
        "lexington",
        "feat: Add feature [Midtown !42]",
    );

    let session = state.get_pr_author_session(42).unwrap();
    assert_eq!(session.session_id, "session-abc-123");
    assert_eq!(session.branch, "lexington/feature");
    assert_eq!(session.original_author, "lexington");
    assert_eq!(session.task_id, Some("42".to_string()));
}

#[test]
fn test_get_pr_author_session_none() {
    let state = GitHubState::default();
    assert!(state.get_pr_author_session(99).is_none());
}

#[test]
fn test_remove_pr_author_session() {
    let mut state = GitHubState::default();
    state.store_pr_author_session(
        42,
        "session-abc-123",
        "lexington/feature",
        "lexington",
        "feat: Add feature",
    );

    let removed = state.remove_pr_author_session(42);
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().session_id, "session-abc-123");
    assert!(state.get_pr_author_session(42).is_none());
}

#[test]
fn test_cleanup_closed_prs_removes_author_sessions() {
    let mut state = GitHubState::default();
    state.store_pr_author_session(
        42,
        "session-1",
        "lexington/feature",
        "lexington",
        "feat: Feature 1",
    );
    state.store_pr_author_session(43, "session-2", "park/feature", "park", "feat: Feature 2");
    state.store_pr_author_session(44, "session-3", "york/feature", "york", "feat: Feature 3");

    // Only PR 42 and 44 are still open
    state.cleanup_closed_prs(&[42, 44]);

    assert!(state.get_pr_author_session(42).is_some());
    assert!(state.get_pr_author_session(43).is_none()); // cleaned up
    assert!(state.get_pr_author_session(44).is_some());
}

#[test]
fn test_pr_author_session_persists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.store_pr_author_session(
        42,
        "session-abc",
        "lexington/feature",
        "lexington",
        "feat: Add auth [Midtown !123]",
    );
    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    let session = loaded.get_pr_author_session(42).unwrap();
    assert_eq!(session.session_id, "session-abc");
    assert_eq!(session.branch, "lexington/feature");
    assert_eq!(session.original_author, "lexington");
    assert_eq!(session.task_id, Some("123".to_string()));
}

#[test]
fn test_extract_task_id_from_title() {
    assert_eq!(
        extract_task_id_from_title("feat: Add auth [Midtown !42]"),
        Some("42".to_string())
    );
    assert_eq!(
        extract_task_id_from_title("fix: Bug [MIDTOWN !123]"),
        Some("123".to_string())
    );
    assert_eq!(
        extract_task_id_from_title("feat: Thing [midtown !7]"),
        Some("7".to_string())
    );
    assert_eq!(extract_task_id_from_title("No task marker"), None);
    assert_eq!(extract_task_id_from_title("[Midtown !]"), None);
    assert_eq!(extract_task_id_from_title("[Midtown !abc]"), None);
}

/// Regression test for !1818: When a reviewer is shut down mid-review and their
/// assignment expires, `cleanup_expired_preserving` must remove the stale
/// assignment (since the reviewer is no longer running). This opens the re-spawn
/// path in `collect_reviewer_effects_with_source` — `is_assigned()` returns false,
/// so a new reviewer gets spawned on the next PR poll.
#[test]
fn test_expired_assignment_removed_when_reviewer_not_running_enables_respawn() {
    let mut state = GitHubState::default();
    state.assign_reviewer(1553, "amsterdam", AssignmentSource::Webhook);

    // Expire the assignment (simulate >10 minutes of reviewing).
    if let Some(a) = state.pr_reviewers.get_mut(&1553) {
        a.assigned_at =
            Utc::now() - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 120);
    }

    // Precondition: `is_assigned()` returns false (expired).
    assert!(
        !state.is_assigned(1553),
        "expired assignment must not appear as active"
    );

    // Amsterdam was shut down — not in running_coworkers.
    let running: std::collections::HashSet<String> = std::collections::HashSet::new();
    state.cleanup_expired_preserving(&running, None);

    // Assignment must be fully removed, making pr_number 1553 eligible
    // for a fresh reviewer spawn on the next PR poll.
    assert!(
        !state.pr_reviewers.contains_key(&1553),
        "expired assignment for shut-down reviewer must be removed by \
         cleanup_expired_preserving to open the re-spawn path"
    );
}

#[test]
fn test_add_review_comment_id() {
    let mut state = GitHubState::default();
    state.add_review_comment_id(42, 1001);
    state.add_review_comment_id(42, 1002);
    assert_eq!(state.get_review_comment_ids(42), &[1001, 1002]);
}

#[test]
fn test_add_review_comment_id_deduplicates() {
    let mut state = GitHubState::default();
    state.add_review_comment_id(42, 1001);
    state.add_review_comment_id(42, 1001); // duplicate
    assert_eq!(state.get_review_comment_ids(42), &[1001]);
}

#[test]
fn test_get_review_comment_ids_empty() {
    let state = GitHubState::default();
    assert!(state.get_review_comment_ids(99).is_empty());
}

#[test]
fn test_review_comment_ids_serialize_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");

    let mut state = GitHubState::default();
    state.add_review_comment_id(42, 1001);
    state.add_review_comment_id(42, 1002);
    state.add_review_comment_id(99, 2001);
    state.save(&path).unwrap();

    let loaded = GitHubState::load(&path).unwrap();
    assert_eq!(loaded.get_review_comment_ids(42), &[1001, 1002]);
    assert_eq!(loaded.get_review_comment_ids(99), &[2001]);
    assert!(loaded.get_review_comment_ids(1).is_empty());
}
