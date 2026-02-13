use super::*;

#[test]
fn test_determine_lead_working_pane_changed() {
    let now = Instant::now();
    let grace = Duration::from_secs(30);
    assert!(determine_lead_working(true, None, now, grace));
    assert!(determine_lead_working(true, Some(now), now, grace));
}

#[test]
fn test_determine_lead_working_within_grace_period() {
    let now = Instant::now();
    let grace = Duration::from_secs(30);
    let last_activity = now - Duration::from_secs(10);
    assert!(determine_lead_working(
        false,
        Some(last_activity),
        now,
        grace
    ));
}

#[test]
fn test_determine_lead_working_grace_period_expired() {
    let now = Instant::now();
    let grace = Duration::from_secs(30);
    let last_activity = now - Duration::from_secs(31);
    assert!(!determine_lead_working(
        false,
        Some(last_activity),
        now,
        grace
    ));
}

#[test]
fn test_determine_lead_working_no_activity_ever() {
    let now = Instant::now();
    let grace = Duration::from_secs(30);
    assert!(!determine_lead_working(false, None, now, grace));
}

#[test]
fn test_determine_lead_working_exactly_at_grace_boundary() {
    let now = Instant::now();
    let grace = Duration::from_secs(30);
    let last_activity = now - Duration::from_secs(30);
    assert!(!determine_lead_working(
        false,
        Some(last_activity),
        now,
        grace
    ));
}

/// Test that usage limit expiry nudges only target Running coworkers.
///
/// Regression test: the function previously iterated `snap.active_coworkers`
/// (all statuses) to generate NudgeCoworker effects. Nudges target tmux
/// windows via send-keys, so Stopping/Starting coworkers (no window) would
/// cause "can't find window" errors.
#[test]
fn test_usage_limit_nudge_only_targets_running_coworkers() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    let running = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    let stopping = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "park".to_string(),
        status: CoworkerStatus::Stopping,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    // Build a snapshot where the nudge should fire
    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![running.clone(), stopping.clone()],
        running_coworkers: vec![running.clone()],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: true,
        // Set nudge time in the past so it fires
        usage_limit_nudge_at: Some(tokio::time::Instant::now() - Duration::from_secs(10)),
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let effects = maybe_nudge_usage_limit_expiry(&snap);

    // Should have effects: ClearUsageLimitNudge + PostToChannel + 1 NudgeCoworker
    let nudge_names: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeCoworker { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();

    // Only the Running coworker should be nudged
    assert!(
        nudge_names.contains(&"lexington"),
        "Running coworker should be nudged"
    );
    assert!(
        !nudge_names.contains(&"park"),
        "Stopping coworker must NOT be nudged"
    );
    assert_eq!(nudge_names.len(), 1, "Only 1 coworker should be nudged");
}

#[test]
fn test_fired_reminder_nudges_lead() {
    use crate::reminders::{Reminder, ReminderTrigger};

    let reminder = Reminder {
        id: "abc123".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Cut new release".to_string(),
        created_at: chrono::Utc::now(),
        fired: false,
    };
    let fired = vec![&reminder];

    let effects = effects_for_fired_reminders(&fired, "test-repo");

    // Should have: PostToChannel, NudgeLead, MarkRemindersFired
    assert_eq!(effects.len(), 3, "Expected 3 effects");
    assert!(
        matches!(&effects[0], Effect::PostToChannel { .. }),
        "First effect should be PostToChannel"
    );
    assert!(
        matches!(&effects[1], Effect::NudgeLead { .. }),
        "Second effect should be NudgeLead"
    );
    assert!(
        matches!(&effects[2], Effect::MarkRemindersFired { .. }),
        "Third effect should be MarkRemindersFired"
    );
}

#[test]
fn test_fired_reminder_no_reminders_produces_no_effects() {
    let fired: Vec<&crate::reminders::Reminder> = vec![];
    let effects = effects_for_fired_reminders(&fired, "test-repo");
    assert!(
        effects.is_empty(),
        "No fired reminders should produce no effects"
    );
}

#[test]
fn test_check_for_usage_limits_with_reset_time() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    // Create a ProcessHealth with usage limit and a specific reset time
    let reset_time = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut health = HashMap::new();
    health.insert(
        "amsterdam".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(chrono::Utc::now()),
            has_usage_limit: true,
            usage_limit_reset_at: Some(reset_time),
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            exit_code: None,
        },
    );

    let coworker = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "amsterdam".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    // Create a minimal snapshot
    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![coworker],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::from(["amsterdam".to_string()]),
        active_session_ids: HashSet::new(),
        session_name: "test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: health,
        attached_coworkers: HashSet::new(),
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let effects = check_for_usage_limits(&snap);

    // Should have SetUsageLimitNudge and PostToChannel effects
    assert!(!effects.is_empty(), "Should produce effects");

    // Check that a nudge is scheduled
    let has_set_nudge = effects
        .iter()
        .any(|e| matches!(e, Effect::SetUsageLimitNudge { .. }));
    assert!(has_set_nudge, "Should schedule a usage limit nudge");

    // Check that a message is posted
    let has_post = effects
        .iter()
        .any(|e| matches!(e, Effect::PostToChannel { .. }));
    assert!(has_post, "Should post a channel message");
}

#[test]
fn test_check_for_usage_limits_already_scheduled() {
    use std::collections::{HashMap, HashSet};

    // Create a snapshot with usage_limit_nudge_scheduled = true
    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: true, // Already scheduled
        usage_limit_nudge_at: Some(tokio::time::Instant::now()),
        usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let effects = check_for_usage_limits(&snap);

    // Should not schedule another nudge
    assert!(effects.is_empty(), "Should not schedule duplicate nudge");
}

#[test]
fn check_for_stale_worktrees_generates_cleanup_and_channel_message() {
    use std::collections::HashSet;

    let mut registry = crate::worktree_registry::WorktreeRegistry::new();
    // Stale worktree with task ID, completed 48 hours ago
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-99-fix-bug".to_string(),
            branch_name: "task-99-fix-bug".to_string(),
            task_id: Some("99".to_string()),
            current_coworker: None,
            pr_number: Some(200),
            created_at: chrono::Utc::now() - chrono::Duration::hours(72),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(48)),
        })
        .unwrap();
    // Non-stale worktree (within retention period)
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-100-add-test".to_string(),
            branch_name: "task-100-add-test".to_string(),
            task_id: Some("100".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: chrono::Utc::now() - chrono::Duration::hours(2),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        })
        .unwrap();

    let active_coworkers = HashSet::new();
    let retention = chrono::Duration::hours(24);

    let effects = check_for_stale_worktrees(&registry, &active_coworkers, retention);

    // Only the 48h-old worktree should be cleaned up (2 effects: cleanup + message)
    assert_eq!(
        effects.len(),
        2,
        "should generate 1 cleanup + 1 channel message effect"
    );
    assert!(
        matches!(&effects[0], Effect::CleanupStaleWorktree { worktree_id } if worktree_id == "task-99-fix-bug"),
        "first effect should be CleanupStaleWorktree"
    );
    assert!(
        matches!(&effects[1], Effect::PostSystemMessage { message } if message.contains("task-99-fix-bug") && message.contains("task !99") && message.contains('🧹')),
        "second effect should be PostSystemMessage with task ID"
    );
}
