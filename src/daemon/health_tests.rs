use super::*;

/// Test that usage limit expiry nudges only target Running coworkers.
///
/// Regression test: the function previously iterated `snap.active_coworkers`
/// (all statuses) to generate NudgeCoworker effects. Nudges should only
/// target Running coworkers with active sessions.
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
        attached_coworkers: HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
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
            has_pending_api_call: false,
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
        attached_coworkers: HashMap::new(),
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
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
        attached_coworkers: HashMap::new(),
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
    };

    let effects = check_for_usage_limits(&snap);

    // Should not schedule another nudge
    assert!(effects.is_empty(), "Should not schedule duplicate nudge");
}

#[test]
fn check_for_stale_worktrees_generates_only_cleanup_effect() {
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

    // Only the 48h-old worktree should be cleaned up.
    // Decision function should only return CleanupStaleWorktree effect.
    // The message posting is handled by effects.rs when executing the cleanup.
    assert_eq!(
        effects.len(),
        1,
        "should generate only 1 CleanupStaleWorktree effect (message posting happens in effects.rs)"
    );
    assert!(
        matches!(&effects[0], Effect::CleanupStaleWorktree { worktree_id } if worktree_id == "task-99-fix-bug"),
        "first effect should be CleanupStaleWorktree"
    );
}

/// Helper to build a minimal empty WorldSnapshot for ensure_lead_alive tests.
fn empty_snap() -> snapshot::WorldSnapshot {
    use std::collections::{HashMap, HashSet};
    snapshot::WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashMap::new(),
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
    }
}

#[test]
fn ensure_lead_alive_respawns_missing_lead() {
    let snap = empty_snap();
    let effects = ensure_lead_alive(&snap);
    assert_eq!(effects.len(), 1, "Should spawn lead when missing");
    assert!(
        matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == "lead"),
        "Should spawn a lead config"
    );
}

#[test]
fn ensure_lead_alive_no_op_when_lead_registered() {
    use crate::coworker::{Coworker, CoworkerStatus};
    let mut snap = empty_snap();
    snap.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });
    let effects = ensure_lead_alive(&snap);
    assert!(effects.is_empty(), "Should not respawn when lead is alive");
}

#[test]
fn ensure_lead_alive_cooldown_prevents_respawn_loop() {
    let mut snap = empty_snap();
    // Lead stopped 1 minute ago — within the 5-minute cooldown
    snap.coworker_stop_times.insert(
        "lead".to_string(),
        chrono::Utc::now() - chrono::Duration::minutes(1),
    );
    let effects = ensure_lead_alive(&snap);
    assert!(
        effects.is_empty(),
        "Should not respawn during cooldown period"
    );
}

#[test]
fn ensure_lead_alive_respawns_after_cooldown() {
    let mut snap = empty_snap();
    // Lead stopped 10 minutes ago — past the 5-minute cooldown
    snap.coworker_stop_times.insert(
        "lead".to_string(),
        chrono::Utc::now() - chrono::Duration::minutes(10),
    );
    let effects = ensure_lead_alive(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Should respawn lead after cooldown expires"
    );
}

#[test]
fn ensure_lead_alive_skips_when_attached() {
    let mut snap = empty_snap();
    snap.attached_coworkers
        .insert("lead".to_string(), chrono::Utc::now());
    let effects = ensure_lead_alive(&snap);
    assert!(
        effects.is_empty(),
        "Should not spawn headless lead when attached interactively"
    );
}

/// Documents the interaction between `clear_lead_respawn_cooldown()` and
/// `ensure_lead_alive()`: when there is NO stop time entry for "lead",
/// `ensure_lead_alive()` respawns immediately (no cooldown delay).
///
/// This is the key mechanism used by `expedite_lead_respawn_on_user_message()`:
/// it removes the stop time, and on the very next tick `ensure_lead_alive()`
/// sees no stop time and respawns without waiting for the 5-minute cooldown.
#[test]
fn ensure_lead_alive_respawns_immediately_when_stop_time_cleared() {
    // No stop time at all (simulates what clear_lead_respawn_cooldown() does)
    let snap = empty_snap();
    let effects = ensure_lead_alive(&snap);
    assert_eq!(
        effects.len(),
        1,
        "ensure_lead_alive should respawn immediately when no stop time exists"
    );
    assert!(
        matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == "lead"),
        "Effect should be SpawnCoworker for lead"
    );
}

// -----------------------------------------------------------------------
// detect_stale_attached_sessions tests
// -----------------------------------------------------------------------

#[test]
fn detect_stale_attached_sessions_no_op_when_recent() {
    // Attached 5 minutes ago — well within the 10-minute timeout
    let mut snap = empty_snap();
    let recent = snap.now_utc - chrono::Duration::minutes(5);
    snap.attached_coworkers.insert("lead".to_string(), recent);
    let effects = detect_stale_attached_sessions(&snap);
    assert!(
        effects.is_empty(),
        "Session attached 5 min ago should not be auto-detached (timeout is 10 min)"
    );
}

#[test]
fn detect_stale_attached_sessions_auto_detaches_after_timeout() {
    // Attached 15 minutes ago — past the 10-minute timeout
    let mut snap = empty_snap();
    let stale = snap.now_utc - chrono::Duration::minutes(15);
    snap.attached_coworkers.insert("lead".to_string(), stale);
    let effects = detect_stale_attached_sessions(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Session attached 15 min ago should be auto-detached"
    );
    assert!(
        matches!(&effects[0], Effect::AutoDetachCoworker { name } if name == "lead"),
        "Effect should be AutoDetachCoworker for lead"
    );
}

#[test]
fn detect_stale_attached_sessions_handles_multiple() {
    // One stale (15 min), one fresh (5 min) — only stale gets detached
    let mut snap = empty_snap();
    let stale = snap.now_utc - chrono::Duration::minutes(15);
    let fresh = snap.now_utc - chrono::Duration::minutes(5);
    snap.attached_coworkers.insert("lead".to_string(), stale);
    snap.attached_coworkers
        .insert("amsterdam".to_string(), fresh);
    let effects = detect_stale_attached_sessions(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Only the stale session should be auto-detached"
    );
    assert!(
        matches!(&effects[0], Effect::AutoDetachCoworker { name } if name == "lead"),
        "Effect should be AutoDetachCoworker for lead (the stale one)"
    );
}

#[test]
fn test_maybe_refresh_lead_session_no_refresh_when_disabled() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // interval_secs = 0 → no effects even if lead has been running for a long time
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 0;
    let started = snap.now_utc - chrono::Duration::minutes(120);
    let lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.active_coworkers.push(lead);
    snap.coworker_start_times
        .insert("lead".to_string(), started);

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when interval is 0 (disabled)"
    );
}

#[test]
fn test_maybe_refresh_lead_session_no_refresh_when_young() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // lead started 30 min ago, interval = 90 min → no effects
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 90 * 60;
    let started = snap.now_utc - chrono::Duration::minutes(30);
    let lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.active_coworkers.push(lead);
    snap.coworker_start_times
        .insert("lead".to_string(), started);

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when lead is younger than the interval"
    );
}

#[test]
fn test_maybe_refresh_lead_session_triggers_when_old() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // lead started 91 min ago, interval = 90 min → PostToChannel + ShutdownCoworker
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 90 * 60;
    let started = snap.now_utc - chrono::Duration::minutes(91);
    let lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.active_coworkers.push(lead);
    snap.coworker_start_times
        .insert("lead".to_string(), started);

    let effects = maybe_refresh_lead_session(&snap);
    assert_eq!(
        effects.len(),
        2,
        "Should produce PostToChannel + ShutdownCoworker when lead is past the interval"
    );
    assert!(
        matches!(&effects[0], Effect::PostToChannel { sender, .. } if sender == "midtown"),
        "First effect should be PostToChannel from midtown"
    );
    assert!(
        matches!(&effects[1], Effect::ShutdownCoworker { name, .. } if name == "lead"),
        "Second effect should be ShutdownCoworker for lead"
    );
}

#[test]
fn test_maybe_refresh_lead_session_skips_attached() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // lead is attached interactively → no effects even if old
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 90 * 60;
    let started = snap.now_utc - chrono::Duration::minutes(120);
    let lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.active_coworkers.push(lead);
    snap.coworker_start_times
        .insert("lead".to_string(), started);
    snap.attached_coworkers
        .insert("lead".to_string(), snap.now_utc);

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when lead is attached interactively"
    );
}

#[test]
fn test_maybe_refresh_lead_session_no_lead_in_active_coworkers() {
    // No lead in active_coworkers → no effects
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 90 * 60;
    // Don't add any coworkers — lead is missing from active_coworkers

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when lead is not in active_coworkers"
    );
}

#[test]
fn test_maybe_refresh_lead_session_no_start_time() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // Lead is in active_coworkers but missing from coworker_start_times → no effects
    let mut snap = empty_snap();
    snap.lead_session_refresh_interval_secs = 90 * 60;
    let started = snap.now_utc - chrono::Duration::minutes(120);
    let lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lead".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.active_coworkers.push(lead);
    // Intentionally don't insert into coworker_start_times

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when lead has no start time recorded"
    );
}

// -----------------------------------------------------------------------
// Session ID propagation tests (health → effects)
// -----------------------------------------------------------------------

#[test]
fn stuck_coworker_restart_propagates_session_id_to_shutdown_effect() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Active coworker with a stuck health entry
    snap.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "riverside".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: now - chrono::Duration::minutes(30),
        current_task: Some("42".to_string()),
        session_id: Some("session-stuck-abc".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });

    // Stuck health: no events for 10 minutes
    snap.headless_process_health.insert(
        "riverside".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: None,
        },
    );

    // In-progress task owned by riverside
    snap.in_progress_tasks.push((
        "42".to_string(),
        "Fix bug".to_string(),
        "riverside".to_string(),
    ));

    // Session mapping: riverside → session-stuck-abc
    snap.name_session_map
        .insert("riverside".to_string(), "session-stuck-abc".to_string());

    // We can't call check_and_restart_stuck_coworkers directly because it needs DaemonState,
    // but we can verify the pure decision layer populates session_id correctly.
    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_coworker_restarts(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &exemptions,
        snap.now_utc,
        Duration::from_secs(180),
        &snap.name_session_map,
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id,
        Some("session-stuck-abc".to_string()),
        "stuck coworker restart should carry session_id from name_session_map"
    );
}

#[test]
fn dead_process_respawn_propagates_session_id() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Dead health entry
    snap.headless_process_health.insert(
        "york".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(137),
            last_event_at: Some(now - chrono::Duration::seconds(60)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
        },
    );

    // In-progress task
    snap.in_progress_tasks.push((
        "99".to_string(),
        "Add feature".to_string(),
        "york".to_string(),
    ));

    // Session mapping
    snap.name_session_map
        .insert("york".to_string(), "session-dead-xyz".to_string());

    let respawns = crate::rules::decide_dead_process_respawns(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &snap.name_session_map,
    );

    assert_eq!(respawns.len(), 1);
    assert_eq!(
        respawns[0].session_id,
        Some("session-dead-xyz".to_string()),
        "dead process respawn should carry session_id from name_session_map"
    );
}

#[test]
fn stuck_reviewer_restart_propagates_session_id() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Stuck reviewer health
    snap.headless_process_health.insert(
        "amsterdam".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: None,
        },
    );

    // Reviewer assignment
    snap.reviewer_pr_assignments
        .insert("amsterdam".to_string(), 77);

    // Session mapping
    snap.name_session_map
        .insert("amsterdam".to_string(), "session-rev-999".to_string());

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_reviewer_restarts(
        &snap.headless_process_health,
        &snap.reviewer_pr_assignments,
        &snap.reviewer_restart_counts,
        &exemptions,
        snap.now_utc,
        Duration::from_secs(300),
        2,
        &snap.name_session_map,
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id,
        Some("session-rev-999".to_string()),
        "stuck reviewer restart should carry session_id from name_session_map"
    );
}

#[test]
fn session_id_is_none_when_no_session_mapping_exists() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Stuck health with no session mapping
    snap.headless_process_health.insert(
        "broadway".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: None,
        },
    );

    snap.in_progress_tasks.push((
        "55".to_string(),
        "Refactor module".to_string(),
        "broadway".to_string(),
    ));

    // No name_session_map entry for broadway

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_coworker_restarts(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &exemptions,
        snap.now_utc,
        Duration::from_secs(180),
        &snap.name_session_map,
    );

    assert_eq!(restarts.len(), 1);
    assert_eq!(
        restarts[0].session_id, None,
        "session_id should be None when no mapping exists in name_session_map"
    );
}

// -----------------------------------------------------------------------
// ensure_lead_alive snapshot tests — lead not responding scenario
// -----------------------------------------------------------------------

/// Verify that the captured snapshot (lead dead within cooldown) has the
/// cooldown active, so `ensure_lead_alive` would NOT respawn immediately.
/// After clearing the stop time (as `clear_lead_respawn_cooldown()` does),
/// `ensure_lead_alive` should respawn on the next tick.
///
/// This documents the exact scenario that `expedite_lead_respawn_on_user_message`
/// solves: a user message arrives while the lead is in the cooldown window.
#[test]
fn snapshot_lead_not_responding_cooldown_blocks_respawn_until_cleared() {
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-lead-not-responding-20260219-183818.json"
    );
    let mut snap: super::snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("Failed to deserialize WorldSnapshot from fixture");

    // Precondition: lead is not registered (dead)
    let lead_registered = snap
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case("lead"));
    assert!(
        !lead_registered,
        "Lead should not be registered in snapshot"
    );

    // Precondition: lead is not attached interactively
    assert!(
        !snap.attached_coworkers.contains_key("lead"),
        "Lead should not be attached in snapshot"
    );

    // Precondition: lead has a recent stop time (within cooldown)
    let stop_time = snap
        .coworker_stop_times
        .get("lead")
        .expect("Lead should have a stop time in snapshot");
    let since_stop = snap.now_utc.signed_duration_since(*stop_time);
    assert!(
        since_stop < chrono::Duration::minutes(5),
        "Lead stop time should be within 5-min cooldown (was {}s ago)",
        since_stop.num_seconds()
    );

    // With cooldown active, ensure_lead_alive should NOT respawn
    let effects = ensure_lead_alive(&snap);
    assert!(
        effects.is_empty(),
        "ensure_lead_alive should NOT respawn while cooldown is active (stop time set {}s ago)",
        since_stop.num_seconds()
    );

    // Simulate what clear_lead_respawn_cooldown() does: remove the stop time
    snap.coworker_stop_times.remove("lead");

    // Now ensure_lead_alive should respawn immediately
    let effects_after_clear = ensure_lead_alive(&snap);
    assert_eq!(
        effects_after_clear.len(),
        1,
        "ensure_lead_alive should respawn after stop time is cleared"
    );
    assert!(
        matches!(&effects_after_clear[0], Effect::SpawnCoworker(config) if config.name == "lead"),
        "Effect should be SpawnCoworker for lead"
    );
}

// -----------------------------------------------------------------------
// ensure_channel_leads_alive tests
// -----------------------------------------------------------------------

#[test]
fn ensure_channel_leads_alive_empty_sessions_no_effects() {
    // No channel_lead_sessions registered → no effects
    let snap = empty_snap();
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "Empty channel_lead_sessions should produce no effects"
    );
}

#[test]
fn ensure_channel_leads_alive_already_running_no_effects() {
    use crate::coworker::{Coworker, CoworkerStatus};
    // Channel lead is already registered as an active coworker → no effects
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("web".to_string(), "session-web-123".to_string());
    snap.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "web".to_string(), // channel_lead_session_name("web") == "web"
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: Some("session-web-123".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "Channel lead already running should produce no effects"
    );
}

#[test]
fn ensure_channel_leads_alive_in_flight_spawn_no_effects() {
    // In-flight spawn: session_id is empty, no previous stop recorded → skip to avoid double-spawning
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "".to_string());
    // No entry in coworker_stop_times for "ops"
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "In-flight spawn (empty session_id, no previous stop) should produce no effects"
    );
}

#[test]
fn ensure_channel_leads_alive_crash_recovery_after_cooldown_spawns_fresh() {
    // Crash recovery: empty session_id, stop_time exists, cooldown elapsed → SpawnCoworker(Fresh)
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "".to_string());
    // Stop time well beyond the cooldown
    snap.coworker_stop_times.insert(
        "ops".to_string(),
        snap.now_utc - chrono::Duration::minutes(10),
    );
    let effects = ensure_channel_leads_alive(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Crash recovery after cooldown should produce one SpawnCoworker effect"
    );
    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(
                config.name, "ops",
                "Should spawn a channel lead named 'ops'"
            );
            assert!(
                matches!(config.session_mode, crate::launch::SessionMode::Fresh),
                "Crash recovery with empty session_id should use Fresh mode"
            );
        }
        other => panic!("Expected SpawnCoworker, got {:?}", other),
    }
}

#[test]
fn ensure_channel_leads_alive_crash_recovery_within_cooldown_no_effects() {
    // Crash recovery: empty session_id, stop_time exists, still within cooldown → no effects
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "".to_string());
    // Stop time within the cooldown window (1 minute ago)
    snap.coworker_stop_times.insert(
        "ops".to_string(),
        snap.now_utc - chrono::Duration::minutes(1),
    );
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "Crash recovery within cooldown should produce no effects"
    );
}

#[test]
fn ensure_channel_leads_alive_normal_recovery_after_cooldown_resumes_session() {
    // Normal recovery: non-empty session_id, not running, cooldown elapsed → SpawnCoworker(ResumeSession)
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "session-ops-abc".to_string());
    // Stop time well beyond the cooldown
    snap.coworker_stop_times.insert(
        "ops".to_string(),
        snap.now_utc - chrono::Duration::minutes(10),
    );
    let effects = ensure_channel_leads_alive(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Normal recovery after cooldown should produce one SpawnCoworker effect"
    );
    match &effects[0] {
        Effect::SpawnCoworker(config) => {
            assert_eq!(
                config.name, "ops",
                "Should spawn a channel lead named 'ops'"
            );
            assert!(
                matches!(
                    &config.session_mode,
                    crate::launch::SessionMode::ResumeSession(id) if id == "session-ops-abc"
                ),
                "Normal recovery should use ResumeSession with stored session_id"
            );
        }
        other => panic!("Expected SpawnCoworker, got {:?}", other),
    }
}

#[test]
fn ensure_channel_leads_alive_normal_recovery_within_cooldown_no_effects() {
    // Normal recovery: non-empty session_id, not running, within cooldown → no effects
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "session-ops-abc".to_string());
    // Stop time within the cooldown window (1 minute ago)
    snap.coworker_stop_times.insert(
        "ops".to_string(),
        snap.now_utc - chrono::Duration::minutes(1),
    );
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "Normal recovery within cooldown should produce no effects"
    );
}

// -----------------------------------------------------------------------
// has_pending_api_call exemption tests
// -----------------------------------------------------------------------

/// Regression test: coworkers waiting for an API response after a tool_result
/// (extended thinking phase) must NOT be killed by stuck detection.
///
/// The bug: after tool_result arrives, has_pending_tool is cleared but the model
/// then enters extended thinking before emitting the next assistant event. During
/// this silent window, stuck detection incorrectly fired and killed the session.
#[test]
fn pending_api_call_exempts_coworker_from_stuck_detection() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    snap.headless_process_health.insert(
        "lexington".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_pending_api_call: true,
            has_tool_name_conflict: false,
            exit_code: None,
        },
    );

    snap.in_progress_tasks.push((
        "77".to_string(),
        "Review PR".to_string(),
        "lexington".to_string(),
    ));

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };

    let restarts = crate::rules::decide_stuck_coworker_restarts(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &exemptions,
        snap.now_utc,
        Duration::from_secs(180),
        &snap.name_session_map,
    );

    assert_eq!(
        restarts.len(),
        0,
        "coworker waiting for API response (extended thinking) must NOT be restarted"
    );
}

/// Corollary: same scenario for reviewers.
#[test]
fn pending_api_call_exempts_reviewer_from_stuck_detection() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    snap.headless_process_health.insert(
        "amsterdam".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(now - chrono::Duration::minutes(10)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_pending_api_call: true,
            has_tool_name_conflict: false,
            exit_code: None,
        },
    );

    snap.reviewer_pr_assignments
        .insert("amsterdam".to_string(), 1329);

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };

    let restarts = crate::rules::decide_stuck_reviewer_restarts(
        &snap.headless_process_health,
        &snap.reviewer_pr_assignments,
        &snap.reviewer_restart_counts,
        &exemptions,
        snap.now_utc,
        Duration::from_secs(300),
        2,
        &snap.name_session_map,
    );

    assert_eq!(
        restarts.len(),
        0,
        "reviewer waiting for API response (extended thinking) must NOT be restarted"
    );
}

/// When a session mapping exists for an idle coworker, the idle-shutdown
/// path should emit `ShutdownSession` instead of `ShutdownCoworker`.
#[test]
fn test_idle_shutdown_emits_shutdown_session_when_mapping_exists() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use crate::rules::CoworkerSnapshot;
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    // started_at well before now so MINIMUM_COWORKER_LIFETIME (60s) is satisfied
    let started_at = now - chrono::Duration::minutes(10);

    let coworker = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "amsterdam".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at,
        current_task: None,
        session_id: Some("sess-abc-123".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    let cw_snap = CoworkerSnapshot {
        name: "amsterdam".to_string(),
        started_at,
        session_id: Some("sess-abc-123".to_string()),
    };

    let mut name_session_map = HashMap::new();
    name_session_map.insert("amsterdam".to_string(), "sess-abc-123".to_string());

    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![coworker.clone()],
        running_coworkers: vec![coworker.clone()],
        coworker_snapshots: vec![cw_snap],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: now,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map,
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
    };

    let effects = check_and_shutdown_idle_coworkers(&snap);

    // Should contain ShutdownSession (not ShutdownCoworker)
    let has_shutdown_session = effects.iter().any(|e| {
        matches!(
            e,
            Effect::ShutdownSession {
                session_id,
                reason,
            } if session_id == "sess-abc-123" && reason.contains("idle shutdown")
        )
    });
    assert!(
        has_shutdown_session,
        "Expected ShutdownSession effect with session_id 'sess-abc-123', got: {:?}",
        effects
    );

    // Must NOT contain ShutdownCoworker for "amsterdam"
    let has_shutdown_coworker = effects.iter().any(|e| {
        matches!(
            e,
            Effect::ShutdownCoworker { name, .. } if name == "amsterdam"
        )
    });
    assert!(
        !has_shutdown_coworker,
        "Should NOT have ShutdownCoworker when session mapping exists"
    );
}

/// When NO session mapping exists for an idle coworker, the idle-shutdown
/// path should fall back to the legacy `ShutdownCoworker` effect.
#[test]
fn test_idle_shutdown_falls_back_to_shutdown_coworker_without_mapping() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use crate::rules::CoworkerSnapshot;
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let started_at = now - chrono::Duration::minutes(10);

    let coworker = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "broadway".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    let cw_snap = CoworkerSnapshot {
        name: "broadway".to_string(),
        started_at,
        session_id: None,
    };

    // No session mapping for "broadway"
    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![coworker.clone()],
        running_coworkers: vec![coworker.clone()],
        coworker_snapshots: vec![cw_snap],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
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
        orphaned_pr_lead_nudges_sent: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
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
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: now,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
    };

    let effects = check_and_shutdown_idle_coworkers(&snap);

    // Should contain ShutdownCoworker (legacy path)
    let has_shutdown_coworker = effects.iter().any(|e| {
        matches!(
            e,
            Effect::ShutdownCoworker { name, .. } if name == "broadway"
        )
    });
    assert!(
        has_shutdown_coworker,
        "Expected ShutdownCoworker effect for 'broadway', got: {:?}",
        effects
    );

    // Must NOT contain ShutdownSession
    let has_shutdown_session = effects
        .iter()
        .any(|e| matches!(e, Effect::ShutdownSession { .. }));
    assert!(
        !has_shutdown_session,
        "Should NOT have ShutdownSession when no session mapping exists"
    );
}
