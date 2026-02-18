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
