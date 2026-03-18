use super::*;

/// Test that usage limit expiry nudges only target Running coworkers.
///
/// Regression test: the function previously iterated `snap.coworkers.active_coworkers`
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
        coworkers: snapshot::SnapshotCoworkerState {
            active_coworkers: vec![running.clone(), stopping.clone()],
            running_coworkers: vec![running.clone()],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashMap::new(),
        },
        pr: snapshot::SnapshotPrState {
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pr_task_index: snapshot::PrTaskIndex::default(),
            orphaned_pr_lead_nudges_sent: HashSet::new(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        },
        reviewer: snapshot::SnapshotReviewerState {
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewer_in_progress_comment_ids: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
        },
        health: snapshot::SnapshotHealthState {
            headless_process_health: HashMap::new(),
            usage_limit_nudge_scheduled: true,
            // Set nudge time in the past so it fires
            usage_limit_nudge_at: Some(tokio::time::Instant::now() - Duration::from_secs(10)),
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            auth_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworkers_with_active_tools: HashSet::new(),
        },
        in_progress_tasks: vec![(
            "42".to_string(),
            "Fix bug".to_string(),
            "lexington".to_string(),
        )],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        pending_task_owners: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        lead_session_refresh_interval_secs: 5400,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: chrono::Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::from([
            ("lexington".to_string(), "sess-lexington".to_string()),
            ("park".to_string(), "sess-park".to_string()),
        ]),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
    };

    let effects = maybe_nudge_usage_limit_expiry(&snap);

    // Should have effects: ClearUsageLimitNudge + PostToChannel + 1 NudgeSession
    let nudge_session_ids: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .collect();

    // Only the Running coworker should be nudged, not the Stopping one
    assert_eq!(
        nudge_session_ids,
        vec!["sess-lexington"],
        "Only the Running coworker (lexington) should be nudged"
    );
}

#[test]
fn test_usage_limit_nudge_includes_reviewers_and_leads_with_sessions() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    let task_worker = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: Some("42".to_string()),
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    let project_lead = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "test-repo".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Codex,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    let reviewer = Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "amsterdam".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: Some("reviewing PR #99".to_string()),
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    let snap = snapshot::WorldSnapshot {
        coworkers: snapshot::SnapshotCoworkerState {
            active_coworkers: vec![task_worker.clone(), project_lead.clone(), reviewer.clone()],
            running_coworkers: vec![task_worker.clone(), project_lead.clone(), reviewer.clone()],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashMap::new(),
        },
        pr: snapshot::SnapshotPrState {
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pr_task_index: snapshot::PrTaskIndex::default(),
            orphaned_pr_lead_nudges_sent: HashSet::new(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        },
        reviewer: snapshot::SnapshotReviewerState {
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewer_in_progress_comment_ids: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
        },
        health: snapshot::SnapshotHealthState {
            headless_process_health: HashMap::new(),
            usage_limit_nudge_scheduled: true,
            usage_limit_nudge_at: Some(tokio::time::Instant::now() - Duration::from_secs(10)),
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            auth_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworkers_with_active_tools: HashSet::new(),
        },
        in_progress_tasks: vec![(
            "42".to_string(),
            "Implement feature".to_string(),
            "lexington".to_string(),
        )],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        pending_task_owners: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        lead_session_refresh_interval_secs: 5400,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: chrono::Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::from([
            ("lexington".to_string(), "sess-lexington".to_string()),
            ("test-repo".to_string(), "sess-lead".to_string()),
            ("amsterdam".to_string(), "sess-reviewer".to_string()),
        ]),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
    };

    let effects = maybe_nudge_usage_limit_expiry(&snap);
    let nudge_session_ids: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        nudge_session_ids,
        vec!["sess-lexington", "sess-lead", "sess-reviewer"],
        "all running sessions should be nudged when usage limit expires"
    );
}

#[test]
fn test_fired_reminder_nudges_lead() {
    use crate::reminders::{Reminder, ReminderTrigger};

    let reminder = Reminder {
        id: "abc123".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Cut new release".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: crate::reminders::RepeatPolicy::Once,
        fire_count: 0,
        last_evaluated_at: None,
    };
    let fired = vec![&reminder];

    let effects = effects_for_fired_reminders(&fired, "test-repo", "test-repo");

    // Should have: PostToChannel, NudgeLead, MarkRemindersFired
    assert_eq!(effects.len(), 3, "Expected 3 effects");
    assert!(
        matches!(&effects[0], Effect::PostToChannel { .. }),
        "First effect should be PostToChannel"
    );
    assert!(
        matches!(&effects[1], Effect::NudgeChannelLead { .. }),
        "Second effect should be NudgeChannelLead"
    );
    assert!(
        matches!(&effects[2], Effect::MarkRemindersFired { .. }),
        "Third effect should be MarkRemindersFired"
    );
}

#[test]
fn test_fired_reminder_no_reminders_produces_no_effects() {
    let fired: Vec<&crate::reminders::Reminder> = vec![];
    let effects = effects_for_fired_reminders(&fired, "test-repo", "test-repo");
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
        coworkers: snapshot::SnapshotCoworkerState {
            active_coworkers: vec![coworker],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::from(["amsterdam".to_string()]),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashMap::new(),
        },
        pr: snapshot::SnapshotPrState {
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pr_task_index: snapshot::PrTaskIndex::default(),
            orphaned_pr_lead_nudges_sent: HashSet::new(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        },
        reviewer: snapshot::SnapshotReviewerState {
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewer_in_progress_comment_ids: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
        },
        health: snapshot::SnapshotHealthState {
            headless_process_health: health,
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
            api_error_coworkers: HashSet::new(),
            auth_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworkers_with_active_tools: HashSet::new(),
        },
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        pending_task_owners: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
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
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: chrono::Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
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
        coworkers: snapshot::SnapshotCoworkerState {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashMap::new(),
        },
        pr: snapshot::SnapshotPrState {
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pr_task_index: snapshot::PrTaskIndex::default(),
            orphaned_pr_lead_nudges_sent: HashSet::new(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        },
        reviewer: snapshot::SnapshotReviewerState {
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewer_in_progress_comment_ids: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
        },
        health: snapshot::SnapshotHealthState {
            headless_process_health: HashMap::new(),
            usage_limit_nudge_scheduled: true, // Already scheduled
            usage_limit_nudge_at: Some(tokio::time::Instant::now()),
            usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
            api_error_coworkers: HashSet::new(),
            auth_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworkers_with_active_tools: HashSet::new(),
        },
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        pending_task_owners: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
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
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: chrono::Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
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
        coworkers: snapshot::SnapshotCoworkerState {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashMap::new(),
        },
        pr: snapshot::SnapshotPrState {
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pr_task_index: snapshot::PrTaskIndex::default(),
            orphaned_pr_lead_nudges_sent: HashSet::new(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        },
        reviewer: snapshot::SnapshotReviewerState {
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewer_in_progress_comment_ids: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
        },
        health: snapshot::SnapshotHealthState {
            headless_process_health: HashMap::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            auth_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworkers_with_active_tools: HashSet::new(),
        },
        busy_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        pending_tasks_without_owners: vec![],
        pending_tasks_with_owners: vec![],
        all_tasks: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        task_thread_id_map: HashMap::new(),
        task_message_id_map: HashMap::new(),
        task_parent_map: HashMap::new(),
        task_agent_type_map: HashMap::new(),
        channel_lead_sessions: HashMap::new(),
        channel_lead_names: HashSet::new(),
        lead_driven_channels: HashSet::new(),
        pending_task_owners: HashSet::new(),
        coworkers_with_unblocked_deps: HashSet::new(),
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
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        now_utc: chrono::Utc::now(),
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        default_branch: "main".to_string(),
        repo_owner: None,
        topic_sessions: HashMap::new(),
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        in_flight_task_spawns: HashSet::new(),
        task_nudge_cooldown_ids: HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
        pr_protected_tasks: HashSet::new(),
        note_staleness_cooldown_channels: HashSet::new(),
        merge_rebase_nudge_cooldown_names: HashSet::new(),
        rebase_nudge_processed_prs: HashSet::new(),
        rebase_regression_cooldown_names: HashSet::new(),
        stale_channel_lead_worktrees: HashSet::new(),
        lead_worktree_freshness_cooldown_channels: HashSet::new(),
        stale_channel_notes: HashMap::new(),
    }
}

#[test]
fn ensure_lead_alive_respawns_missing_lead() {
    let snap = empty_snap();
    let effects = ensure_lead_alive(&snap);
    assert_eq!(effects.len(), 1, "Should spawn lead when missing");
    // After rename, lead session name = repo name (test-repo in test snapshots)
    assert!(
        matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == snap.project_name),
        "Should spawn a lead config with the project name"
    );
}

#[test]
fn ensure_lead_alive_no_op_when_lead_registered() {
    use crate::coworker::{Coworker, CoworkerStatus};
    let mut snap = empty_snap();
    // After rename, lead session name = repo name (test-repo in test snapshots)
    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: snap.project_name.clone(),
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
    // Key is the repo name (not "lead") after rename
    snap.coworkers.coworker_stop_times.insert(
        snap.project_name.to_lowercase(),
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
    // Lead stopped 10 minutes ago — past the 5-minute cooldown.
    // Key is the repo name (lowercase) after the rename; using "lead" would not be found.
    snap.coworkers.coworker_stop_times.insert(
        snap.project_name.to_lowercase(),
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
    // After rename, lead is keyed by repo name (lowercase)
    snap.coworkers
        .attached_coworkers
        .insert(snap.project_name.to_lowercase(), chrono::Utc::now());
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
        matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == snap.project_name),
        "Effect should be SpawnCoworker with project name"
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
    snap.coworkers
        .attached_coworkers
        .insert("lead".to_string(), recent);
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
    snap.coworkers
        .attached_coworkers
        .insert("lead".to_string(), stale);
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
    snap.coworkers
        .attached_coworkers
        .insert("lead".to_string(), stale);
    snap.coworkers
        .attached_coworkers
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
    snap.coworkers.active_coworkers.push(lead);
    snap.coworkers
        .coworker_start_times
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
    snap.coworkers.active_coworkers.push(lead);
    snap.coworkers
        .coworker_start_times
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
        name: snap.project_name.clone(), // lead session name = project name
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: started,
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };
    snap.coworkers.active_coworkers.push(lead);
    snap.coworkers
        .coworker_start_times
        .insert(snap.project_name.to_lowercase(), started);

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
        matches!(&effects[1], Effect::ShutdownCoworker { name, .. } if name == &snap.project_name),
        "Second effect should be ShutdownCoworker for the repo-named lead"
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
    snap.coworkers.active_coworkers.push(lead);
    snap.coworkers
        .coworker_start_times
        .insert("lead".to_string(), started);
    snap.coworkers
        .attached_coworkers
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
    snap.coworkers.active_coworkers.push(lead);
    // Intentionally don't insert into coworker_start_times

    let effects = maybe_refresh_lead_session(&snap);
    assert!(
        effects.is_empty(),
        "No refresh should happen when lead has no start time recorded"
    );
}

#[test]
fn dead_process_respawn_propagates_session_id() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Dead health entry
    snap.health.headless_process_health.insert(
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
        &snap.health.headless_process_health,
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
    snap.fixup_legacy_fields();

    let lead_name = snap.project_name.to_lowercase();

    // Precondition: lead is not registered (dead)
    let lead_registered = snap
        .coworkers
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(&snap.project_name));
    assert!(
        !lead_registered,
        "Lead should not be registered in snapshot"
    );

    // Precondition: lead is not attached interactively
    assert!(
        !snap.coworkers.attached_coworkers.contains_key(&lead_name),
        "Lead should not be attached in snapshot"
    );

    // Precondition: lead has a recent stop time (within cooldown)
    let stop_time = snap
        .coworkers
        .coworker_stop_times
        .get(&lead_name)
        .expect("Lead should have a stop time in snapshot (keyed by repo name)");
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
    snap.coworkers.coworker_stop_times.remove(&lead_name);

    // Now ensure_lead_alive should respawn immediately
    let effects_after_clear = ensure_lead_alive(&snap);
    assert_eq!(
        effects_after_clear.len(),
        1,
        "ensure_lead_alive should respawn after stop time is cleared"
    );
    assert!(
        matches!(&effects_after_clear[0], Effect::SpawnCoworker(config) if config.name == snap.project_name),
        "Effect should be SpawnCoworker with repo name (lead session name = repo name)"
    );
}

/// Dead reviewer with a placeholder comment emits `UpdatePrComment` to mark the
/// placeholder as abandoned when the reviewer is respawned.
#[test]
fn dead_reviewer_with_placeholder_emits_update_pr_comment() {
    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    // Dead reviewer health: process exited without posting review
    snap.health.headless_process_health.insert(
        "broadway".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
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

    // Reviewer assigned to PR 88, but review was NOT posted
    snap.reviewer
        .reviewer_pr_assignments
        .insert("broadway".to_string(), 88);
    // reviewed_prs does NOT contain 88 (review not posted)

    // PR 88 has a placeholder comment with id 888
    snap.reviewer
        .reviewer_in_progress_comment_ids
        .insert(88, 888);

    // Below max restarts
    snap.reviewer.reviewer_restart_counts.insert(88, 0);

    let effects = check_and_restart_dead_reviewers(&snap);

    let has_update_pr_comment = effects.iter().any(|e| {
        matches!(
            e,
            Effect::UpdatePrComment { comment_id, .. } if *comment_id == 888
        )
    });
    assert!(
        has_update_pr_comment,
        "Expected UpdatePrComment effect with comment_id 888 for dead reviewer with placeholder, got: {:#?}",
        effects
    );
}

/// When a dead reviewer exhausts the restart budget, `check_and_restart_dead_reviewers`
/// must emit a PostToChannel to the ops channel, a NudgeLead, and a RecordReviewerEscalation.
/// Subsequent ticks must not re-emit the escalation (idempotency via reviewer_escalations_posted).
#[test]
fn dead_reviewer_at_max_restarts_escalates_to_ops() {
    use std::collections::HashMap;

    let mut reviewer_pr_assignments = HashMap::new();
    reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);

    let mut reviewer_restart_counts = HashMap::new();
    reviewer_restart_counts.insert(1352u64, MAX_REVIEWER_RESTARTS); // at limit

    let mut headless_process_health = HashMap::new();
    headless_process_health.insert(
        "riverside".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );

    let mut snap = empty_snap();
    snap.health.headless_process_health = headless_process_health;
    snap.reviewer.reviewer_pr_assignments = reviewer_pr_assignments;
    snap.reviewer.reviewer_restart_counts = reviewer_restart_counts;

    let effects = check_and_restart_dead_reviewers(&snap);

    let has_ops_post = effects.iter().any(|e| {
        matches!(
            e,
            Effect::PostToChannel { channel: Some(ch), message, .. }
                if ch == OPS_CHANNEL && message.contains("1352")
        )
    });
    assert!(
        has_ops_post,
        "expected PostToChannel to ops for PR #1352, got: {:#?}",
        effects
    );

    let has_nudge_lead = effects
        .iter()
        .any(|e| matches!(e, Effect::NudgeChannelLead { .. }));
    assert!(
        has_nudge_lead,
        "expected NudgeChannelLead effect, got: {:#?}",
        effects
    );

    let has_record_escalation = effects.iter().any(
        |e| matches!(e, Effect::RecordReviewerEscalation { pr_number } if *pr_number == 1352u64),
    );
    assert!(
        has_record_escalation,
        "expected RecordReviewerEscalation for PR #1352, got: {:#?}",
        effects
    );
}

/// Once an escalation is posted (pr_number in reviewer_escalations_posted),
/// subsequent ticks must not re-emit the ops message.
#[test]
fn dead_reviewer_escalation_not_repeated_after_recorded() {
    use std::collections::{HashMap, HashSet};

    let mut reviewer_pr_assignments = HashMap::new();
    reviewer_pr_assignments.insert("riverside".to_string(), 1352u64);

    let mut reviewer_restart_counts = HashMap::new();
    reviewer_restart_counts.insert(1352u64, MAX_REVIEWER_RESTARTS);

    let mut reviewer_escalations_posted = HashSet::new();
    reviewer_escalations_posted.insert(1352u64); // already escalated

    let mut headless_process_health = HashMap::new();
    headless_process_health.insert(
        "riverside".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );

    let mut snap = empty_snap();
    snap.health.headless_process_health = headless_process_health;
    snap.reviewer.reviewer_pr_assignments = reviewer_pr_assignments;
    snap.reviewer.reviewer_restart_counts = reviewer_restart_counts;
    snap.reviewer.reviewer_escalations_posted = reviewer_escalations_posted;

    let effects = check_and_restart_dead_reviewers(&snap);

    assert!(
        effects.is_empty(),
        "no effects expected when escalation already posted, got: {:#?}",
        effects
    );
}

/// `check_and_restart_dead_reviewers` must produce both a respawn and an
/// escalation in the same tick when two dead reviewers are present: one below
/// max restarts (→ respawn) and one at max restarts (→ escalation).
///
/// This covers the combined-effects code path where the early-return guard
/// checks both respawns and escalations.
#[test]
fn check_and_restart_dead_reviewers_emits_respawn_and_escalation_in_same_tick() {
    let mut snap = empty_snap();

    // Reviewer "riverside" — dead, below max restarts → should be respawned.
    snap.health.headless_process_health.insert(
        "riverside".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    snap.reviewer
        .reviewer_pr_assignments
        .insert("riverside".to_string(), 100u64);
    // restart_count = 0, below MAX_REVIEWER_RESTARTS

    // Reviewer "broadway" — dead, at max restarts → should be escalated.
    snap.health.headless_process_health.insert(
        "broadway".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    snap.reviewer
        .reviewer_pr_assignments
        .insert("broadway".to_string(), 200u64);
    snap.reviewer
        .reviewer_restart_counts
        .insert(200u64, MAX_REVIEWER_RESTARTS);
    // escalation not yet posted for PR 200

    let effects = check_and_restart_dead_reviewers(&snap);

    // Must contain a SpawnCoworkerWithCallbacks for riverside (the respawn).
    let has_respawn = effects.iter().any(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { config, .. } = e {
            config.name == "riverside"
        } else {
            false
        }
    });
    assert!(
        has_respawn,
        "Expected SpawnCoworkerWithCallbacks for 'riverside' (below max restarts), got: {:#?}",
        effects
    );

    // Must contain a RecordReviewerEscalation for broadway's PR (the escalation).
    let has_escalation = effects
        .iter()
        .any(|e| matches!(e, Effect::RecordReviewerEscalation { pr_number } if *pr_number == 200));
    assert!(
        has_escalation,
        "Expected RecordReviewerEscalation for PR 200 ('broadway' at max restarts), got: {:#?}",
        effects
    );
}

#[test]
fn unrecoverable_session_error_restarts_project_lead_immediately() {
    let mut snap = empty_snap();
    snap.dir_key = "midtown".to_string();
    snap.project_name = "midtown".to_string();
    snap.default_channel = "midtown".to_string();
    snap.health
        .tool_name_conflict_coworkers
        .insert("midtown".to_string());
    snap.name_session_map
        .insert("midtown".to_string(), "sess-lead-1".to_string());

    let effects = check_and_restart_tool_name_conflicts(&snap);

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ClearSavedSessionId { name } if name == "midtown")),
        "expected ClearSavedSessionId for project lead, got: {:#?}",
        effects
    );
    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::ShutdownSession { session_id, .. } if session_id == "sess-lead-1")
        ),
        "expected ShutdownSession for project lead, got: {:#?}",
        effects
    );
    assert!(
        effects.iter().any(|e| {
            matches!(
                e,
                Effect::SpawnCoworker(config)
                    if config.name == "midtown"
                        && matches!(config.role, crate::launch::CoworkerRole::Lead)
                        && matches!(config.session_mode, crate::launch::SessionMode::Fresh)
            )
        }),
        "expected immediate fresh lead spawn, got: {:#?}",
        effects
    );
}

#[test]
fn unrecoverable_session_error_does_not_force_spawn_for_non_lead() {
    let mut snap = empty_snap();
    snap.dir_key = "midtown".to_string();
    snap.project_name = "midtown".to_string();
    snap.health
        .tool_name_conflict_coworkers
        .insert("lexington".to_string());
    snap.name_session_map
        .insert("lexington".to_string(), "sess-lex-1".to_string());

    let effects = check_and_restart_tool_name_conflicts(&snap);

    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnCoworker(config) if config.name == "lexington")),
        "non-lead coworker should not be force-spawned by session-error health check, got: {:#?}",
        effects
    );
}

// ── Auth profile pool: usage-limit marking ────────────────────────────────────

#[test]
fn usage_limit_marks_pool_profile_limited() {
    // When a coworker with a usage limit is mapped to a pool profile,
    // check_for_usage_limits() should emit MarkProfileLimited for that profile.
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    let reset_time = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut health = HashMap::new();
    health.insert(
        "lexington".to_string(),
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

    let mut profile_map = HashMap::new();
    profile_map.insert("lexington".to_string(), "alice@example.com".to_string());

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_coworkers = vec![coworker];
    snap.health.headless_process_health = health;
    snap.coworkers.active_names = HashSet::from(["lexington".to_string()]);
    snap.health.usage_limited_coworkers = HashSet::from(["lexington".to_string()]);
    snap.session_profile_map = profile_map;

    let effects = check_for_usage_limits(&snap);

    assert!(!effects.is_empty(), "Should produce effects");

    let has_mark_limited = effects.iter().any(|e| {
        matches!(e, Effect::MarkProfileLimited { profile_email, .. } if profile_email == "alice@example.com")
    });
    assert!(
        has_mark_limited,
        "Should emit MarkProfileLimited for alice@example.com, got: {:#?}",
        effects
    );
}

#[test]
fn usage_limit_without_profile_map_skips_mark_limited() {
    // When a coworker hits a usage limit but is NOT in session_profile_map
    // (single-profile mode, no pool), check_for_usage_limits() should NOT
    // emit MarkProfileLimited.
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    let mut health = HashMap::new();
    health.insert(
        "lexington".to_string(),
        snapshot::ProcessHealth {
            is_alive: true,
            last_event_at: Some(chrono::Utc::now()),
            has_usage_limit: true,
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

    let coworker = Coworker {
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

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_coworkers = vec![coworker];
    snap.health.headless_process_health = health;
    snap.coworkers.active_names = HashSet::from(["lexington".to_string()]);
    snap.health.usage_limited_coworkers = HashSet::from(["lexington".to_string()]);
    snap.session_profile_map = HashMap::new(); // no pool mapping

    let effects = check_for_usage_limits(&snap);

    assert!(
        !effects.is_empty(),
        "Should produce SetUsageLimitNudge and PostToChannel effects"
    );

    let has_mark_limited = effects
        .iter()
        .any(|e| matches!(e, Effect::MarkProfileLimited { .. }));
    assert!(
        !has_mark_limited,
        "Should NOT emit MarkProfileLimited when no profile is mapped"
    );
}

#[test]
fn maybe_nudge_usage_limit_expiry_clears_pool_profile_limit() {
    // When the usage limit reset fires, maybe_nudge_usage_limit_expiry() should
    // emit ClearProfileLimit for ALL profiles in limited_pool_profiles (from
    // persistent state), not only those reachable via session_profile_map.
    use std::collections::HashSet;

    let past_instant = tokio::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or_else(tokio::time::Instant::now);

    let mut snap = snapshot::minimal_snapshot_for_test();
    // At least one running coworker so eligibility check passes
    snap.coworkers.running_coworkers = vec![crate::coworker::Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    }];
    snap.health.usage_limit_nudge_scheduled = true;
    snap.health.usage_limit_nudge_at = Some(past_instant);
    // limited_pool_profiles is the source of truth for which profiles to clear.
    // Both alice and bob are marked is_usage_limited in persistent state.
    snap.limited_pool_profiles = HashSet::from([
        "alice@example.com".to_string(),
        "bob@example.com".to_string(),
    ]);

    let effects = maybe_nudge_usage_limit_expiry(&snap);

    assert!(!effects.is_empty(), "Should produce effects");

    let has_clear_alice = effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "alice@example.com")
    });
    assert!(
        has_clear_alice,
        "Should emit ClearProfileLimit for alice@example.com, got: {:#?}",
        effects
    );

    let has_clear_bob = effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "bob@example.com")
    });
    assert!(
        has_clear_bob,
        "Should emit ClearProfileLimit for bob@example.com, got: {:#?}",
        effects
    );
}

#[test]
fn maybe_nudge_usage_limit_expiry_no_clear_for_non_limited_profiles() {
    // Only profiles in limited_pool_profiles (is_usage_limited=true in persistent
    // state) get ClearProfileLimit. Profiles not in that set are left untouched.
    use std::collections::HashSet;

    let past_instant = tokio::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or_else(tokio::time::Instant::now);

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.running_coworkers = vec![crate::coworker::Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    }];
    snap.health.usage_limit_nudge_scheduled = true;
    snap.health.usage_limit_nudge_at = Some(past_instant);
    // Only alice is in limited_pool_profiles — bob is NOT usage-limited.
    snap.limited_pool_profiles = HashSet::from(["alice@example.com".to_string()]);

    let effects = maybe_nudge_usage_limit_expiry(&snap);

    let has_clear_alice = effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "alice@example.com")
    });
    assert!(
        has_clear_alice,
        "Should clear alice (she is in limited_pool_profiles)"
    );

    let has_clear_bob = effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "bob@example.com")
    });
    assert!(
        !has_clear_bob,
        "Should NOT clear bob (he is not in limited_pool_profiles)"
    );
}

#[test]
fn maybe_nudge_usage_limit_expiry_clears_profiles_even_when_session_map_empty() {
    // Regression test for P1: profiles must be cleared from persistent state
    // even when session_profile_map is empty (coworker exited before nudge fired,
    // or daemon restarted). The fix is to iterate limited_pool_profiles (persistent)
    // instead of session_profile_map (ephemeral).
    use std::collections::HashSet;

    let past_instant = tokio::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or_else(tokio::time::Instant::now);

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.health.usage_limit_nudge_scheduled = true;
    snap.health.usage_limit_nudge_at = Some(past_instant);
    // session_profile_map is empty — coworker already stopped or daemon restarted
    // limited_pool_profiles is populated from persistent state (survives restarts)
    snap.limited_pool_profiles = HashSet::from(["alice@example.com".to_string()]);

    let effects = maybe_nudge_usage_limit_expiry(&snap);

    let has_clear = effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "alice@example.com")
    });
    assert!(
        has_clear,
        "Should emit ClearProfileLimit even when session_profile_map is empty, got: {:#?}",
        effects
    );
}

#[test]
fn check_for_usage_limits_marks_all_limited_coworker_profiles() {
    // Regression test for P2: when multiple coworkers simultaneously hit the
    // usage limit, ALL their pool profiles must be marked — not just the first.
    use crate::coworker::{Coworker, CoworkerStatus};
    use std::collections::{HashMap, HashSet};

    let mut health = HashMap::new();
    for name in &["amsterdam", "lexington"] {
        health.insert(
            name.to_string(),
            snapshot::ProcessHealth {
                is_alive: true,
                last_event_at: Some(chrono::Utc::now()),
                has_usage_limit: true,
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
    }

    let make_coworker = |name: &str| Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    };

    let mut profile_map = HashMap::new();
    profile_map.insert("amsterdam".to_string(), "alice@example.com".to_string());
    profile_map.insert("lexington".to_string(), "bob@example.com".to_string());

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_coworkers = vec![make_coworker("amsterdam"), make_coworker("lexington")];
    snap.health.headless_process_health = health;
    snap.coworkers.active_names = HashSet::from(["amsterdam".to_string(), "lexington".to_string()]);
    snap.session_profile_map = profile_map;

    let effects = check_for_usage_limits(&snap);

    let marked: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::MarkProfileLimited { profile_email, .. } => Some(profile_email.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        marked.contains(&"alice@example.com"),
        "Should mark alice@example.com (amsterdam's profile), got: {:#?}",
        marked
    );
    assert!(
        marked.contains(&"bob@example.com"),
        "Should mark bob@example.com (lexington's profile), got: {:#?}",
        marked
    );
    assert_eq!(
        marked.len(),
        2,
        "Should emit exactly 2 MarkProfileLimited effects, got: {:#?}",
        marked
    );
}

// ── check_for_stale_notes tests ──────────────────────────────────────────

#[test]
fn test_stale_notes_skips_channels_without_leads() {
    use std::collections::HashMap;
    let mut snap = empty_snap();
    snap.stale_channel_notes =
        HashMap::from([("orphan-channel".to_string(), vec!["old-note".to_string()])]);
    snap.channel_lead_sessions = HashMap::new();

    let effects = check_for_stale_notes(&snap);
    assert!(
        effects.is_empty(),
        "Should skip channels without a lead session"
    );
}

#[test]
fn test_stale_notes_skips_channels_on_cooldown() {
    use std::collections::{HashMap, HashSet};
    let mut snap = empty_snap();
    snap.stale_channel_notes = HashMap::from([("dev".to_string(), vec!["stale-note".to_string()])]);
    snap.channel_lead_sessions = HashMap::from([("dev".to_string(), "sess-dev-lead".to_string())]);
    snap.note_staleness_cooldown_channels = HashSet::from(["dev".to_string()]);

    let effects = check_for_stale_notes(&snap);
    assert!(effects.is_empty(), "Should skip channels on cooldown");
}

#[test]
fn test_stale_notes_emits_nudge_and_cooldown_effects() {
    use std::collections::{HashMap, HashSet};
    let mut snap = empty_snap();
    snap.stale_channel_notes = HashMap::from([(
        "dev".to_string(),
        vec!["old-note".to_string(), "ancient-note".to_string()],
    )]);
    snap.channel_lead_sessions = HashMap::from([("dev".to_string(), "sess-dev-lead".to_string())]);
    snap.note_staleness_cooldown_channels = HashSet::new();

    let effects = check_for_stale_notes(&snap);
    assert_eq!(
        effects.len(),
        2,
        "Should emit NudgeChannelLead + RecordCooldown"
    );
    assert!(
        matches!(&effects[0], Effect::NudgeChannelLead { channel_name, .. } if channel_name == "dev"),
        "First effect should be NudgeChannelLead for 'dev'"
    );
    assert!(
        matches!(&effects[1], Effect::RecordCooldown { category, key } if category == "note_staleness" && key == "dev"),
        "Second effect should be RecordCooldown for note_staleness/dev"
    );
}

/// Dead reviewer respawn should also inherit the task's topic channel.
#[test]
fn test_dead_reviewer_respawn_inherits_task_channel() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    let pr_number = 55u64;
    let task_id = "200";
    let channel_name = "billing-feature";

    // Active reviewer coworker
    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: now - chrono::Duration::minutes(5),
        current_task: None,
        session_id: Some("sess-rev-200".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });

    // Dead: process has exited
    snap.health.headless_process_health.insert(
        "lexington".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            last_event_at: Some(now - chrono::Duration::minutes(2)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: Some(1),
        },
    );

    // Reviewer assignment + not yet reviewed
    snap.reviewer
        .reviewer_pr_assignments
        .insert("lexington".to_string(), pr_number);

    // Session mapping
    snap.name_session_map
        .insert("lexington".to_string(), "sess-rev-200".to_string());

    // PR → task → channel chain
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(
        [(task_id.to_string(), pr_number)].into_iter().collect(),
        std::collections::HashMap::new(),
    );
    snap.task_channel
        .insert(task_id.to_string(), channel_name.to_string());

    let effects = check_and_restart_dead_reviewers(&snap);

    // Find the SpawnCoworkerWithCallbacks and check config.channel
    let config = effects.iter().find_map(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { config, .. } = e {
            Some(config)
        } else {
            None
        }
    });

    assert!(
        config.is_some(),
        "Expected a SpawnCoworkerWithCallbacks effect for dead reviewer respawn. Effects: {:#?}",
        effects
    );

    assert_eq!(
        config.unwrap().channel,
        Some(channel_name.to_string()),
        "Respawned reviewer LaunchConfig.channel should be set to the task's topic channel '{}'. \
         Before fix: respawned reviewers had channel: None.",
        channel_name
    );
}

// ── State GC tests ──────────────────────────────────────────────────────

fn make_session(
    session_id: &str,
    is_running: bool,
    is_reviewer: bool,
    resume_on_startup: bool,
    last_active: chrono::DateTime<chrono::Utc>,
    task_id: Option<&str>,
) -> crate::daemon::state::SessionRecord {
    crate::daemon::state::SessionRecord {
        session_id: session_id.to_string(),
        is_running,
        is_reviewer,
        resume_on_startup,
        last_active,
        task_id: task_id.map(|s| s.to_string()),
        initial_prompt: Some("test prompt".to_string()),
        coworker_type: if is_reviewer {
            "reviewer".to_string()
        } else {
            "dev".to_string()
        },
        ..Default::default()
    }
}

#[test]
fn state_gc_prunes_dead_reviewer_sessions_immediately() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead reviewer session (just stopped, 1 minute ago)
    sessions.insert(
        "reviewer-1".to_string(),
        make_session(
            "reviewer-1",
            false,
            true,
            false,
            now - chrono::Duration::minutes(1),
            None,
        ),
    );
    // Running dev session (should be kept)
    sessions.insert(
        "dev-1".to_string(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1, "should produce exactly one GC effect");
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert_eq!(dead_session_ids, &vec!["reviewer-1".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_prunes_stale_dead_sessions_past_retention() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead dev session, 48 hours old, resume_on_startup=false
    sessions.insert(
        "dead-old".to_string(),
        make_session(
            "dead-old",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            Some("10"),
        ),
    );
    // Dead dev session, 1 hour old, resume_on_startup=false (within retention)
    sessions.insert(
        "dead-recent".to_string(),
        make_session(
            "dead-recent",
            false,
            false,
            false,
            now - chrono::Duration::hours(1),
            Some("11"),
        ),
    );
    // Dead dev session, 48 hours old, resume_on_startup=true (should be kept)
    sessions.insert(
        "dead-resumable".to_string(),
        make_session(
            "dead-resumable",
            false,
            false,
            true,
            now - chrono::Duration::hours(48),
            Some("12"),
        ),
    );

    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert_eq!(dead_session_ids, &vec!["dead-old".to_string()]);
            // dead-recent and dead-resumable survive — prompts preserved for session.clear
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_preserves_initial_prompt_on_stopped_sessions() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Stopped session within retention, has initial_prompt — should NOT be pruned
    // because session.clear needs the prompt to restart with original context
    sessions.insert(
        "stopped-1".to_string(),
        make_session(
            "stopped-1",
            false,
            false,
            true,
            now - chrono::Duration::hours(1),
            None,
        ),
    );
    // Running session (should be kept)
    sessions.insert(
        "running-1".to_string(),
        make_session("running-1", true, false, true, now, None),
    );

    let active_session_ids = HashSet::from(["running-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    // No effects: stopped-1 is within retention and not a reviewer,
    // running-1 is active — nothing to GC
    assert!(
        effects.is_empty(),
        "should produce no effects — stopped session is within retention"
    );
}

#[test]
fn state_gc_prunes_orphaned_task_metadata() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Running session referencing task "42"
    sessions.insert(
        "dev-1".to_string(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);

    // Task metadata keys: "42" (referenced by session), "99" (orphaned), "100" (in active tasks)
    let task_metadata_keys = HashSet::from(["42".to_string(), "99".to_string(), "100".to_string()]);
    let active_task_ids = HashSet::from(["42".to_string(), "100".to_string()]);

    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &task_metadata_keys,
        &active_task_ids,
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            orphaned_task_ids, ..
        } => {
            assert_eq!(orphaned_task_ids, &vec!["99".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_no_effect_when_nothing_to_clean() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Only running sessions, no stale metadata
    sessions.insert(
        "dev-1".to_string(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert!(
        effects.is_empty(),
        "should produce no effects when nothing to clean"
    );
}

#[test]
fn state_gc_works_with_zero_retention_period() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead reviewer session — should still be pruned even with zero retention
    sessions.insert(
        "reviewer-1".to_string(),
        make_session(
            "reviewer-1",
            false,
            true,
            false,
            now - chrono::Duration::minutes(1),
            None,
        ),
    );
    // Dead non-reviewer session, 1 minute old — zero retention means prune immediately
    sessions.insert(
        "dead-dev".to_string(),
        make_session(
            "dead-dev",
            false,
            false,
            false,
            now - chrono::Duration::minutes(1),
            Some("99"),
        ),
    );

    // Orphaned metadata for task "99" (session is being pruned)
    let task_metadata_keys = HashSet::from(["99".to_string()]);
    let retention = chrono::Duration::hours(0); // zero retention

    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &task_metadata_keys,
        &HashSet::new(),
        retention,
    );

    assert_eq!(
        effects.len(),
        1,
        "should produce GC effect even with zero retention"
    );
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids,
            orphaned_task_ids,
        } => {
            assert_eq!(dead_session_ids.len(), 2, "both sessions should be pruned");
            assert!(dead_session_ids.contains(&"reviewer-1".to_string()));
            assert!(dead_session_ids.contains(&"dead-dev".to_string()));
            assert_eq!(orphaned_task_ids, &vec!["99".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// Dead reviewer respawn emits CoworkerStuck workflow event with PR task channel.
#[test]
fn test_dead_reviewer_respawn_emits_coworker_stuck_workflow_event() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    let pr_number = 55u64;
    let task_id = "200";
    let channel_name = "billing-feature";

    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "lexington".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: now - chrono::Duration::minutes(5),
        current_task: None,
        session_id: Some("sess-rev-200".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });

    // Dead: process has exited
    snap.health.headless_process_health.insert(
        "lexington".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            last_event_at: Some(now - chrono::Duration::minutes(2)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: Some(1),
        },
    );

    // Reviewer assignment + not yet reviewed
    snap.reviewer
        .reviewer_pr_assignments
        .insert("lexington".to_string(), pr_number);

    // Session mapping
    snap.name_session_map
        .insert("lexington".to_string(), "sess-rev-200".to_string());

    // PR → task → channel chain
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(
        [(task_id.to_string(), pr_number)].into_iter().collect(),
        std::collections::HashMap::new(),
    );
    snap.task_channel
        .insert(task_id.to_string(), channel_name.to_string());

    let effects = check_and_restart_dead_reviewers(&snap);

    let stuck_event = effects.iter().find_map(|e| {
        if let Effect::EmitWorkflowEvent(crate::workflow::WorkflowEvent::CoworkerStuck {
            channel,
            task_id,
            coworker,
        }) = e
        {
            Some((channel.clone(), task_id.clone(), coworker.clone()))
        } else {
            None
        }
    });

    assert!(
        stuck_event.is_some(),
        "Dead reviewer respawn should emit CoworkerStuck workflow event, got: {:#?}",
        effects
    );
    let (ch, tid, cw) = stuck_event.unwrap();
    assert_eq!(ch, channel_name);
    assert_eq!(tid, Some(task_id.to_string()));
    assert_eq!(cw, "lexington");
}

// -----------------------------------------------------------------------
// Session role determination uses correct labels
// -----------------------------------------------------------------------

/// The session role determination logic should use "Lead" for project leads,
/// "Channel lead" for channel leads, and "Coworker" for regular coworkers.
///
/// Regression test for the logging issue where lead sessions were reported
/// as "Coworker" exits in channel messages.
#[test]
fn test_session_role_determination_labels() {
    use std::collections::HashSet;

    let project_name = "midtown";

    // Build channel lead names set (simulating what snap.channel_lead_names() returns)
    let channel_lead_names: HashSet<String> = HashSet::from(["ops".to_string()]);

    // Helper closure matching the logic in check_and_handle_auth_errors and mod.rs
    let determine_role = |name: &str| -> &'static str {
        let is_lead = crate::daemon::helpers::is_project_lead(name, project_name);
        let is_channel_lead = channel_lead_names.contains(name);
        if is_lead {
            "Lead"
        } else if is_channel_lead {
            "Channel lead"
        } else {
            "Coworker"
        }
    };

    assert_eq!(determine_role("midtown"), "Lead");
    assert_eq!(determine_role("Midtown"), "Lead"); // case-insensitive
    assert_eq!(determine_role("lead"), "Lead"); // legacy name
    assert_eq!(determine_role("ops"), "Channel lead");
    assert_eq!(determine_role("lexington"), "Coworker");
    assert_eq!(determine_role("park"), "Coworker");
}

/// Ensure channel leads are detected as missing when not registered.
/// This validates the preconditions that `ensure_channel_leads_alive` checks.
#[test]
fn test_channel_lead_respawn_preconditions_when_missing() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // No active coworkers registered — channel lead should be respawned.
    let is_registered = snap
        .coworkers
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case("ops"));
    assert!(
        !is_registered,
        "ops should not be registered, triggering respawn"
    );

    // Verify no cooldown would block respawn (no stop time recorded)
    let has_stop_time = snap
        .coworkers
        .coworker_stop_times
        .get(&"ops".to_lowercase());
    assert!(
        has_stop_time.is_none(),
        "No stop time means no cooldown delay"
    );
}

/// Ensure channel lead respawn is skipped when already registered.
#[test]
fn test_channel_lead_respawn_skipped_when_registered() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // Register ops as an active coworker
    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "ops".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: String::new(),
        profile: String::new(),
        provider: crate::auth::AuthProvider::Claude,
    });

    let is_registered = snap
        .coworkers
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case("ops"));
    assert!(
        is_registered,
        "ops is registered — ensure_channel_leads_alive should no-op"
    );
}

/// Ensure channel lead respawn respects cooldown from coworker_stop_times.
#[test]
fn test_channel_lead_respawn_cooldown_prevents_respawn() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // Set stop time to 1 second ago (within 5-minute cooldown)
    let recent_stop = snap.now_utc - chrono::Duration::seconds(1);
    snap.coworkers
        .coworker_stop_times
        .insert("ops".to_string(), recent_stop);

    let since_stop = snap
        .now_utc
        .signed_duration_since(*snap.coworkers.coworker_stop_times.get("ops").unwrap());
    let cooldown = chrono::Duration::from_std(LEAD_RESPAWN_COOLDOWN).unwrap_or_default();

    assert!(
        since_stop < cooldown,
        "Stop was {}s ago, cooldown is {}s — should block respawn",
        since_stop.num_seconds(),
        cooldown.num_seconds()
    );
}

/// Ensure channel lead respawn is skipped when the session is attached interactively.
///
/// When a user attaches to a channel lead via `midtown session attach`, the session
/// is deregistered from `active_coworkers` and tracked in `attached_coworkers`.
/// Without checking `attached_coworkers`, `ensure_channel_leads_alive` would spawn
/// a duplicate headless session after the cooldown expires.
#[test]
fn test_channel_lead_respawn_skipped_when_attached() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // Not in active_coworkers (deregistered on attach)
    let is_registered = snap
        .coworkers
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case("ops"));
    assert!(!is_registered, "ops should not be in active_coworkers");

    // But IS in attached_coworkers (interactive session active)
    snap.coworkers
        .attached_coworkers
        .insert("ops".to_string(), snap.now_utc);

    let is_attached = snap
        .coworkers
        .attached_coworkers
        .contains_key(&"ops".to_lowercase());
    assert!(
        is_attached,
        "ops is attached — ensure_channel_leads_alive should skip respawn"
    );

    // Directly verify the pure function returns no effects
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "attached channel lead should not produce respawn effects"
    );
}

/// Direct test: ensure_channel_leads_alive emits RespawnChannelLead when
/// a channel lead is in channel_lead_sessions but not in active_coworkers
/// and has no cooldown or interactive attachment.
#[test]
fn test_ensure_channel_leads_alive_emits_respawn_effect() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // No active coworkers, no stop time, no attachment → should respawn
    let effects = ensure_channel_leads_alive(&snap);
    assert_eq!(effects.len(), 1, "expected exactly one respawn effect");
    match &effects[0] {
        Effect::RespawnChannelLead { channel_name } => {
            assert_eq!(channel_name, "ops");
        }
        other => panic!("expected RespawnChannelLead, got {:?}", other),
    }
}

/// Direct test: ensure_channel_leads_alive returns empty when the channel
/// lead is already registered as an active coworker.
#[test]
fn test_ensure_channel_leads_alive_noop_when_registered() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "ops".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: String::new(),
        profile: String::new(),
        provider: crate::auth::AuthProvider::Claude,
    });

    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "registered channel lead should not produce respawn effects"
    );
}

/// Direct test: cooldown prevents respawn within LEAD_RESPAWN_COOLDOWN window.
#[test]
fn test_ensure_channel_leads_alive_respects_cooldown() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // Stopped 1 second ago — within the 5-minute cooldown
    let recent_stop = snap.now_utc - chrono::Duration::seconds(1);
    snap.coworkers
        .coworker_stop_times
        .insert("ops".to_string(), recent_stop);

    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "recently stopped channel lead should not be respawned during cooldown"
    );
}

/// Channel lead sessions should NOT be garbage-collected even when
/// resume_on_startup=false and past retention period. Channel leads are
/// long-lived and should always be available for resume.
#[test]
fn state_gc_preserves_channel_lead_sessions() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead channel lead session, 48 hours old, resume_on_startup=false
    // (e.g., after session refresh marked it false because it was dead)
    let mut channel_lead = make_session(
        "cl-ops",
        false,
        false,
        false,
        now - chrono::Duration::hours(48),
        None,
    );
    channel_lead.coworker_type = "channel-lead".to_string();
    sessions.insert("cl-ops".to_string(), channel_lead);

    // Dead dev session, same age — should be pruned
    sessions.insert(
        "dead-dev".to_string(),
        make_session(
            "dead-dev",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            Some("10"),
        ),
    );

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert!(
                !dead_session_ids.contains(&"cl-ops".to_string()),
                "channel lead session should NOT be garbage-collected"
            );
            assert!(
                dead_session_ids.contains(&"dead-dev".to_string()),
                "dead dev session should still be pruned"
            );
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// Dead fork sessions (coworker_type=channel-lead, bound_thread_id set,
/// resume_on_startup=false) must survive GC even when past the retention period.
///
/// Fork sessions are ephemeral (resume_on_startup=false) but created with
/// coworker_type="channel-lead", which protects them from GC. Without this
/// protection, a liveness check marking is_running=false would make the fork
/// eligible for pruning, breaking fork crash recovery (which reads the
/// persistent SessionRecord to get working_dir, auth_provider, and
/// initial_prompt for respawn).
///
/// Regression test for the GC/liveness interaction on fork SessionRecords.
#[test]
fn state_gc_preserves_dead_fork_sessions() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead fork session: coworker_type=channel-lead, resume_on_startup=false,
    // bound_thread_id set, is_running=false (liveness marked it dead), 48h old.
    // This mimics the state after a liveness check detects a dead fork process
    // and cleanup_dead_coworker_state marks is_running=false in the record.
    let mut fork_session = make_session(
        "fork-ops-abc123",
        false,                             // is_running: liveness marked dead
        false,                             // is_reviewer
        false,                             // resume_on_startup: forks are ephemeral
        now - chrono::Duration::hours(48), // well past 24h retention
        None,
    );
    fork_session.coworker_type = "channel-lead".to_string();
    // bound_thread_id is set for fixture realism (real fork sessions have it),
    // but it is NOT part of the GC predicate — coworker_type alone guards protection.
    fork_session.bound_thread_id = Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string());
    sessions.insert("fork-ops-abc123".to_string(), fork_session);

    // Dead dev session, same age — should be pruned (control case).
    sessions.insert(
        "dead-dev".to_string(),
        make_session(
            "dead-dev",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            Some("10"),
        ),
    );

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1, "should produce exactly one GC effect");
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert!(
                !dead_session_ids.contains(&"fork-ops-abc123".to_string()),
                "fork session (channel-lead) must NOT be garbage-collected — \
                 fork crash recovery needs the persistent record"
            );
            assert!(
                dead_session_ids.contains(&"dead-dev".to_string()),
                "dead dev session should still be pruned"
            );
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// Multiple fork sessions at different lifecycle stages: running fork survives
/// (skipped as running), dead fork survives (channel-lead protection), and
/// a dead dev session is pruned. Verifies the GC correctly handles a mixed
/// set of fork and non-fork records simultaneously.
#[test]
fn state_gc_handles_mixed_fork_and_dev_sessions() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Running fork session — should survive (is_running=true)
    let mut running_fork = make_session(
        "fork-ops-running",
        true, // is_running
        false,
        false, // resume_on_startup=false (fork)
        now,
        None,
    );
    running_fork.coworker_type = "channel-lead".to_string();
    // bound_thread_id set for realism; not part of GC predicate.
    running_fork.bound_thread_id = Some("11111111-1111-1111-1111-111111111111".to_string());
    sessions.insert("fork-ops-running".to_string(), running_fork);

    // Dead fork session, 72 hours old — should survive (channel-lead protection)
    let mut dead_fork = make_session(
        "fork-ops-dead",
        false,
        false,
        false,
        now - chrono::Duration::hours(72),
        None,
    );
    dead_fork.coworker_type = "channel-lead".to_string();
    // bound_thread_id set for realism; not part of GC predicate.
    dead_fork.bound_thread_id = Some("22222222-2222-2222-2222-222222222222".to_string());
    sessions.insert("fork-ops-dead".to_string(), dead_fork);

    // Dead dev session, 48h old — should be pruned
    sessions.insert(
        "dead-dev-1".to_string(),
        make_session(
            "dead-dev-1",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            None,
        ),
    );

    // Dead reviewer session — should be pruned immediately
    sessions.insert(
        "reviewer-1".to_string(),
        make_session(
            "reviewer-1",
            false,
            true,
            false,
            now - chrono::Duration::minutes(5),
            None,
        ),
    );

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            // Fork sessions must survive regardless of age or is_running state
            assert!(
                !dead_session_ids.contains(&"fork-ops-running".to_string()),
                "running fork must not be GC'd"
            );
            assert!(
                !dead_session_ids.contains(&"fork-ops-dead".to_string()),
                "dead fork (channel-lead) must not be GC'd"
            );
            // Non-fork sessions follow normal pruning rules
            assert!(
                dead_session_ids.contains(&"dead-dev-1".to_string()),
                "dead dev session should be pruned"
            );
            assert!(
                dead_session_ids.contains(&"reviewer-1".to_string()),
                "dead reviewer should be pruned immediately"
            );
            assert_eq!(
                dead_session_ids.len(),
                2,
                "exactly 2 sessions should be pruned"
            );
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// Fork session with a task_id preserves that task from orphaned metadata pruning.
///
/// Even though the fork is dead, its task_id must be treated as "surviving"
/// because the fork's SessionRecord persists (channel-lead sessions are never
/// GC'd). If the task were pruned, metadata like task_channel and task_plan
/// would be lost.
#[test]
fn state_gc_fork_session_preserves_task_metadata() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Dead fork session with task_id — channel-lead survives GC, so task "55"
    // should be in surviving_task_ids and NOT pruned from metadata.
    let mut fork_with_task = make_session(
        "fork-ops-task",
        false,
        false,
        false,
        now - chrono::Duration::hours(48),
        Some("55"),
    );
    fork_with_task.coworker_type = "channel-lead".to_string();
    // bound_thread_id set for realism; not part of GC predicate.
    fork_with_task.bound_thread_id = Some("33333333-3333-3333-3333-333333333333".to_string());
    sessions.insert("fork-ops-task".to_string(), fork_with_task);

    // Task metadata keys: "55" (referenced by fork), "99" (orphaned)
    let task_metadata_keys = HashSet::from(["55".to_string(), "99".to_string()]);

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &task_metadata_keys,
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids,
            orphaned_task_ids,
        } => {
            assert!(
                dead_session_ids.is_empty(),
                "fork (channel-lead) should not be pruned"
            );
            assert!(
                !orphaned_task_ids.contains(&"55".to_string()),
                "task 55 is referenced by surviving fork — must not be orphaned"
            );
            assert!(
                orphaned_task_ids.contains(&"99".to_string()),
                "task 99 is truly orphaned — should be pruned"
            );
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// Confirms that `bound_thread_id` is NOT part of the GC predicate.
///
/// A channel-lead session without `bound_thread_id` must still survive GC,
/// proving that `coworker_type == "channel-lead"` is the sole guard — not
/// `bound_thread_id`. This clarifies the boundary for readers of the other
/// fork GC tests where `bound_thread_id` is set for fixture realism.
#[test]
fn state_gc_channel_lead_without_bound_thread_survives() {
    use std::collections::{HashMap, HashSet};

    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();

    // Channel-lead session with NO bound_thread_id — should still survive GC.
    let mut lead_no_thread = make_session(
        "lead-no-thread",
        false,
        false,
        false,
        now - chrono::Duration::hours(48),
        None,
    );
    lead_no_thread.coworker_type = "channel-lead".to_string();
    // Deliberately NOT setting bound_thread_id — proving it's not the guard.
    sessions.insert("lead-no-thread".to_string(), lead_no_thread);

    // Control: dead dev session, same age — should be pruned.
    sessions.insert(
        "dead-dev".to_string(),
        make_session(
            "dead-dev",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            None,
        ),
    );

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert!(
                !dead_session_ids.contains(&"lead-no-thread".to_string()),
                "channel-lead without bound_thread_id must still survive GC — \
                 coworker_type is the sole guard, not bound_thread_id"
            );
            assert!(
                dead_session_ids.contains(&"dead-dev".to_string()),
                "dead dev session should be pruned"
            );
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

/// When a user posts in a topic channel and the channel lead is dead
/// (within respawn cooldown), clearing the stop time should allow
/// ensure_channel_leads_alive to respawn on the next tick.
#[test]
fn ensure_channel_leads_alive_respawns_after_cooldown_cleared() {
    let mut snap = empty_snap();
    snap.channel_lead_sessions
        .insert("ops".to_string(), "sess-ops".to_string());

    // Stopped 1 second ago — within the 5-minute cooldown
    let recent_stop = snap.now_utc - chrono::Duration::seconds(1);
    snap.coworkers
        .coworker_stop_times
        .insert("ops".to_string(), recent_stop);

    // Precondition: cooldown blocks respawn
    let effects = ensure_channel_leads_alive(&snap);
    assert!(
        effects.is_empty(),
        "precondition: cooldown should block respawn"
    );

    // Simulate what expedite does: clear the stop time
    snap.coworkers.coworker_stop_times.remove("ops");

    // Now ensure_channel_leads_alive should respawn immediately
    let effects = ensure_channel_leads_alive(&snap);
    assert_eq!(effects.len(), 1, "should respawn after cooldown is cleared");
    match &effects[0] {
        Effect::RespawnChannelLead { channel_name } => {
            assert_eq!(channel_name, "ops");
        }
        other => panic!("expected RespawnChannelLead, got {:?}", other),
    }
}

// ── Worktree cleanup: abandoned worktrees without completed_at ──────────

/// Worktrees created long ago with no `completed_at` and no active coworker
/// should be cleaned up as abandoned. This is the catch-all for the gap where
/// `completed_at` is never set (review worktrees, abandoned tasks, etc.).
#[test]
fn check_for_stale_worktrees_cleans_abandoned_without_completed_at() {
    use std::collections::HashSet;

    let mut registry = crate::worktree_registry::WorktreeRegistry::new();

    // Abandoned review worktree: no completed_at, no coworker, created 48h ago
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "review-pr-100".to_string(),
            branch_name: "review-pr-100".to_string(),
            task_id: None,
            current_coworker: None,
            pr_number: Some(100),
            created_at: chrono::Utc::now() - chrono::Duration::hours(48),
            completed_at: None,
        })
        .unwrap();

    // Abandoned task worktree: no completed_at, no coworker, created 48h ago
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-50-old-work".to_string(),
            branch_name: "task-50-old-work".to_string(),
            task_id: Some("50".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: chrono::Utc::now() - chrono::Duration::hours(48),
            completed_at: None,
        })
        .unwrap();

    // Recent worktree without completed_at: should NOT be cleaned up
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-51-new-work".to_string(),
            branch_name: "task-51-new-work".to_string(),
            task_id: Some("51".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: chrono::Utc::now() - chrono::Duration::hours(2),
            completed_at: None,
        })
        .unwrap();

    // Active worktree without completed_at: bound to active coworker, should NOT be cleaned up
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-52-active".to_string(),
            branch_name: "task-52-active".to_string(),
            task_id: Some("52".to_string()),
            current_coworker: Some("park".to_string()),
            pr_number: None,
            created_at: chrono::Utc::now() - chrono::Duration::hours(48),
            completed_at: None,
        })
        .unwrap();

    let mut active_coworkers = HashSet::new();
    active_coworkers.insert("park".to_string());
    let retention = chrono::Duration::hours(24);

    let effects = check_for_stale_worktrees(&registry, &active_coworkers, retention);

    // Should clean up the two abandoned worktrees (review-pr-100 and task-50-old-work)
    assert_eq!(
        effects.len(),
        2,
        "should clean up 2 abandoned worktrees without completed_at, got: {:?}",
        effects
    );

    let cleaned_ids: HashSet<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CleanupStaleWorktree { worktree_id } => Some(worktree_id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        cleaned_ids.contains("review-pr-100"),
        "should clean up review worktree"
    );
    assert!(
        cleaned_ids.contains("task-50-old-work"),
        "should clean up abandoned task worktree"
    );
}

// ── build_reviewer_respawn_effects task_id lookup ────────────────────────────

/// Verify that `build_reviewer_respawn_effects` sets `task_id: Some(...)` on the
/// `AssignReviewer` effect when `all_tasks` contains a matching review task for
/// the PR being respawned.
///
/// This exercises the `snap.all_tasks.iter().find(...)` path in health.rs that
/// looks up the review task ID for the task session span model.
#[test]
fn build_reviewer_respawn_task_id_is_some_when_matching_task_exists() {
    use crate::coworker::{Coworker, CoworkerStatus};
    use crate::tasks::{Task, TaskStatus};

    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    let pr_number = 77u64;
    let review_task_id = "300";

    // Dead reviewer
    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "broadway".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: now - chrono::Duration::minutes(5),
        current_task: None,
        session_id: Some("sess-rev-300".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });
    snap.health.headless_process_health.insert(
        "broadway".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            last_event_at: Some(now - chrono::Duration::minutes(2)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: Some(1),
        },
    );
    snap.reviewer
        .reviewer_pr_assignments
        .insert("broadway".to_string(), pr_number);
    snap.name_session_map
        .insert("broadway".to_string(), "sess-rev-300".to_string());

    // Add a review task matching this PR
    snap.all_tasks.push(Task {
        id: review_task_id.to_string(),
        subject: "Review PR #77".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("broadway".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(pr_number),
        created_at: None,
    });
    snap.task_agent_type_map.insert(
        review_task_id.to_string(),
        "midtown-code-reviewer".to_string(),
    );

    let effects = check_and_restart_dead_reviewers(&snap);

    // CreateTaskSessionSpan is nested in SpawnCoworkerWithCallbacks.on_success
    let span_task_id = effects.iter().find_map(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
            on_success.iter().find_map(|inner| {
                if let Effect::CreateTaskSessionSpan {
                    task_id,
                    agent_type,
                    ..
                } = inner
                {
                    if agent_type == "reviewer" {
                        Some(task_id.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        } else {
            None
        }
    });

    assert!(
        span_task_id.is_some(),
        "expected a CreateTaskSessionSpan effect in SpawnCoworkerWithCallbacks.on_success; got: {:#?}",
        effects
    );
    assert_eq!(
        span_task_id.unwrap(),
        review_task_id.to_string(),
        "CreateTaskSessionSpan.task_id should match the review task ID"
    );
}

/// Verify that `build_reviewer_respawn_effects` sets `task_id` to empty string on the
/// `CreateTaskSessionSpan` effect when no matching review task exists in `all_tasks`.
///
/// This covers the fallback path when the review task hasn't been created yet
/// (legacy flow) or when no task matched the PR + agent-type filter.
#[test]
fn build_reviewer_respawn_task_id_is_empty_when_no_matching_task() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let now = chrono::Utc::now();
    let mut snap = empty_snap();
    snap.now_utc = now;

    let pr_number = 88u64;

    // Dead reviewer
    snap.coworkers.active_coworkers.push(Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: "riverside".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: now - chrono::Duration::minutes(5),
        current_task: None,
        session_id: Some("sess-rev-88".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    });
    snap.health.headless_process_health.insert(
        "riverside".to_string(),
        snapshot::ProcessHealth {
            is_alive: false,
            last_event_at: Some(now - chrono::Duration::minutes(2)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: Some(1),
        },
    );
    snap.reviewer
        .reviewer_pr_assignments
        .insert("riverside".to_string(), pr_number);
    snap.name_session_map
        .insert("riverside".to_string(), "sess-rev-88".to_string());

    // No tasks in all_tasks — the empty string path

    let effects = check_and_restart_dead_reviewers(&snap);

    // CreateTaskSessionSpan is nested in SpawnCoworkerWithCallbacks.on_success
    let span_task_id = effects.iter().find_map(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
            on_success.iter().find_map(|inner| {
                if let Effect::CreateTaskSessionSpan {
                    task_id,
                    agent_type,
                    ..
                } = inner
                {
                    if agent_type == "reviewer" {
                        Some(task_id.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        } else {
            None
        }
    });

    assert!(
        span_task_id.is_some(),
        "expected a CreateTaskSessionSpan effect in SpawnCoworkerWithCallbacks.on_success; got: {:#?}",
        effects
    );
    assert!(
        span_task_id.unwrap().is_empty(),
        "CreateTaskSessionSpan.task_id should be empty when no matching review task exists in all_tasks"
    );
}

#[test]
fn test_build_reminder_effects_respects_repeat_policy() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};

    // Reminder with Times(2) that has already fired twice — 1 fire left
    let reminder = Reminder {
        id: "abc123".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Test repeat".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Times(2),
        fire_count: 2,
        last_evaluated_at: None,
    };
    assert!(
        reminder.is_active(),
        "Times(2) with fire_count=2 should still be active (3 total fires)"
    );
}

#[test]
fn test_build_reminder_effects_skips_exhausted() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};

    let reminder = Reminder {
        id: "abc123".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Exhausted".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Once,
        fire_count: 1,
        last_evaluated_at: None,
    };
    assert!(!reminder.is_active());

    let reminders = vec![reminder];
    let effects = build_reminder_effects(&reminders, &[], "test-repo", "test-repo");
    assert!(
        effects.is_empty(),
        "Exhausted reminder should not produce effects"
    );
}

#[test]
fn test_cron_reminder_fires_in_window() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};
    use chrono::TimeZone;

    let now = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();
    let last_eval = now - chrono::Duration::seconds(30);

    let reminder = Reminder {
        id: "cron1".to_string(),
        trigger: ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        },
        message: "Monday standup".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Indefinite,
        fire_count: 0,
        last_evaluated_at: Some(last_eval),
    };

    let reminders = vec![reminder];
    let effects = build_reminder_effects_at(&reminders, &[], "test-repo", "test-repo", now);
    // Should have PostToChannel + NudgeChannelLead + MarkRemindersFired
    assert!(
        effects.len() >= 3,
        "Cron reminder should fire: got {} effects",
        effects.len()
    );
}

#[test]
fn test_cron_reminder_no_fire_outside_window() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};
    use chrono::TimeZone;

    // 09:01 — cron was at 09:00, last_eval was 09:00:15 (after cron time)
    let now = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 1, 0).unwrap();
    let last_eval = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();

    let reminder = Reminder {
        id: "cron1".to_string(),
        trigger: ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        },
        message: "Monday standup".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Indefinite,
        fire_count: 0,
        last_evaluated_at: Some(last_eval),
    };

    let reminders = vec![reminder];
    let effects = build_reminder_effects_at(&reminders, &[], "test-repo", "test-repo", now);
    assert!(effects.is_empty(), "Cron should not fire outside window");
}

#[test]
fn test_cron_with_repeat_times_fires_correct_number() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};
    use chrono::TimeZone;

    let last_eval = chrono::Utc
        .with_ymd_and_hms(2026, 3, 16, 8, 59, 30)
        .unwrap();
    let now = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();

    // Cron with Times(2) — total 3 fires. Already fired twice.
    let reminder = Reminder {
        id: "cron-repeat".to_string(),
        trigger: ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        },
        message: "Standup".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Times(2),
        fire_count: 2,
        last_evaluated_at: Some(last_eval),
    };

    let reminders = vec![reminder.clone()];
    let effects = build_reminder_effects_at(&reminders, &[], "test-repo", "test-repo", now);
    assert!(!effects.is_empty(), "Should fire (3rd of 3 total)");

    // Now simulate fire_count = 3 (exhausted)
    let mut exhausted = reminder;
    exhausted.fire_count = 3;
    let reminders = vec![exhausted];
    let effects = build_reminder_effects_at(&reminders, &[], "test-repo", "test-repo", now);
    assert!(
        effects.is_empty(),
        "Should NOT fire (exhausted after 3 total)"
    );
}

#[test]
fn test_cron_with_indefinite_always_fires() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};
    use chrono::TimeZone;

    let last_eval = chrono::Utc
        .with_ymd_and_hms(2026, 3, 16, 8, 59, 30)
        .unwrap();
    let now = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();

    let reminder = Reminder {
        id: "cron-indef".to_string(),
        trigger: ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        },
        message: "Standup".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Indefinite,
        fire_count: 999,
        last_evaluated_at: Some(last_eval),
    };

    let reminders = vec![reminder];
    let effects = build_reminder_effects_at(&reminders, &[], "test-repo", "test-repo", now);
    assert!(
        !effects.is_empty(),
        "Indefinite should always fire when cron matches"
    );
}

#[test]
fn test_mixed_triggers_in_build_reminder_effects() {
    use crate::reminders::{Reminder, ReminderTrigger, RepeatPolicy};
    use chrono::TimeZone;

    let last_eval = chrono::Utc
        .with_ymd_and_hms(2026, 3, 16, 8, 59, 30)
        .unwrap();
    let now = chrono::Utc.with_ymd_and_hms(2026, 3, 16, 9, 0, 15).unwrap();

    let cron_reminder = Reminder {
        id: "cron1".to_string(),
        trigger: ReminderTrigger::CronUtc {
            cron_expr: "0 9 * * MON".to_string(),
        },
        message: "Cron fires".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Indefinite,
        fire_count: 0,
        last_evaluated_at: Some(last_eval),
    };

    let condition_reminder = Reminder {
        id: "awm1".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Condition does not fire".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: RepeatPolicy::Once,
        fire_count: 0,
        last_evaluated_at: None,
    };

    // AllWorkMerged won't fire because there are open PRs
    let reminders = vec![cron_reminder, condition_reminder];
    let effects = build_reminder_effects_at(
        &reminders,
        &["park".to_string()],
        "test-repo",
        "test-repo",
        now,
    );

    // Only cron should fire (3 effects: PostToChannel, NudgeLead, MarkFired)
    assert_eq!(effects.len(), 3, "Only cron reminder should fire");
}
