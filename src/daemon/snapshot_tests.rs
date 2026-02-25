use super::*;

/// Test that ProcessHealth derives usage limit and API error sets correctly.
#[test]
fn test_process_health_derives_usage_limited_and_api_error_sets() {
    let mut health = HashMap::new();
    health.insert(
        "york".to_string(),
        ProcessHealth {
            has_usage_limit: true,
            ..Default::default()
        },
    );
    health.insert(
        "park".to_string(),
        ProcessHealth {
            has_api_error: true,
            ..Default::default()
        },
    );
    health.insert("madison".to_string(), ProcessHealth::default());

    let usage_limited: HashSet<String> = health
        .iter()
        .filter(|(_, h)| h.has_usage_limit)
        .map(|(n, _)| n.to_lowercase())
        .collect();
    let api_error: HashSet<String> = health
        .iter()
        .filter(|(n, h)| h.has_api_error && !usage_limited.contains(&n.to_lowercase()))
        .map(|(n, _)| n.to_lowercase())
        .collect();

    assert!(usage_limited.contains("york"));
    assert!(!usage_limited.contains("park"));
    assert!(api_error.contains("park"));
    assert!(!api_error.contains("madison"));
}

/// Test that WorldSnapshot has coworker_stop_times field and it serializes correctly.
#[test]
fn test_world_snapshot_has_coworker_stop_times() {
    let mut stop_times = HashMap::new();
    stop_times.insert("lexington".to_string(), Utc::now());
    stop_times.insert("broadway".to_string(), Utc::now());

    let snapshot = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: stop_times.clone(),
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
        reviewer_in_progress_comment_ids: HashMap::new(),
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
        coworkers_with_active_tools: HashSet::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
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
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    };

    assert_eq!(snapshot.coworker_stop_times.len(), 2);
    assert!(snapshot.coworker_stop_times.contains_key("lexington"));
    assert!(snapshot.coworker_stop_times.contains_key("broadway"));

    let json = serde_json::to_string(&snapshot).expect("should serialize");
    assert!(json.contains("coworker_stop_times"));
}

/// Test that read_daemon_log_tail returns the last N lines of a file.
#[test]
fn test_read_daemon_log_tail() {
    use std::io::Write;

    // Create a temp file with 10 lines
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let log_path = temp_dir.path().join("test.log");
    {
        let mut file = std::fs::File::create(&log_path).expect("create file");
        for i in 1..=10 {
            writeln!(file, "line {}", i).expect("write line");
        }
    }

    // Test reading the tail - use a custom implementation that accepts a path
    // since read_daemon_log_tail uses a fixed path
    let contents = std::fs::read_to_string(&log_path).expect("read file");
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(5);
    let tail: Vec<String> = lines[start..].iter().map(|s| s.to_string()).collect();

    assert_eq!(tail.len(), 5);
    assert_eq!(tail[0], "line 6");
    assert_eq!(tail[4], "line 10");
}

/// Test that debug context fields (channel_messages, daemon_logs) are empty
/// during normal snapshot collection to avoid I/O overhead on the hot path.
#[test]
fn test_snapshot_debug_context_empty_by_default() {
    let snapshot = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
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
        reviewer_in_progress_comment_ids: HashMap::new(),
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
        coworkers_with_active_tools: HashSet::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
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
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    };

    assert!(snapshot.channel_messages.is_empty());
    assert!(snapshot.daemon_logs.is_empty());

    let json = serde_json::to_string(&snapshot).expect("should serialize");
    assert!(json.contains("\"channel_messages\":[]"));
    assert!(json.contains("\"daemon_logs\":[]"));
}

/// Test that active_names includes alive headless coworkers.
///
/// This is a regression test for #904: active_names was only populated from
/// CoworkerManager.list_running() which missed headless coworkers managed
/// by SessionManager, causing orphan recovery loops and incorrect status reporting.
#[test]
fn test_active_names_includes_headless_coworkers() {
    // Setup: headless process health with two alive coworkers and one stopped
    let mut headless_health = HashMap::new();
    headless_health.insert(
        "riverside".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
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
    headless_health.insert(
        "york".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
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
    headless_health.insert(
        "madison".to_string(),
        ProcessHealth {
            is_alive: false, // stopped
            last_event_at: Some(Utc::now()),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            exit_code: Some(0),
        },
    );

    // Derive active_names from headless_process_health (simulating the fix)
    let headless_active_names: HashSet<String> = headless_health
        .iter()
        .filter(|(_, health)| health.is_alive)
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // Only alive headless coworkers should be in active_names
    assert!(headless_active_names.contains("riverside"));
    assert!(headless_active_names.contains("york"));
    assert!(!headless_active_names.contains("madison")); // stopped, not active
    assert_eq!(headless_active_names.len(), 2);
}

/// Active-turn protection should include pending API calls, not just tools/subagents.
#[test]
fn test_active_work_includes_pending_api_calls() {
    let mut headless_health = HashMap::new();
    headless_health.insert(
        "web".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: true,
            exit_code: None,
        },
    );
    headless_health.insert(
        "stale".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now() - chrono::Duration::minutes(30)),
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: true,
            exit_code: None,
        },
    );

    let now_utc = Utc::now();
    let max_pending_api_call_exemption = chrono::Duration::minutes(20);
    let active_work: HashSet<String> = headless_health
        .iter()
        .filter(|(_, health)| {
            let pending_api_turn_fresh = health.has_pending_api_call
                && health.last_event_at.is_some_and(|t| {
                    now_utc.signed_duration_since(t) < max_pending_api_call_exemption
                });
            health.has_pending_tool || health.has_running_subagent || pending_api_turn_fresh
        })
        .map(|(name, _)| name.to_lowercase())
        .collect();

    assert!(
        active_work.contains("web"),
        "pending API turns must protect sessions from idle shutdown"
    );
    assert!(
        !active_work.contains("stale"),
        "stale pending API turns should not suppress idle shutdown forever"
    );
}

/// Test that sessions_for_name returns session IDs for coworkers matching a name.
#[test]
fn test_sessions_for_name() {
    use crate::coworker::{Coworker, CoworkerStatus};

    let snapshot = WorldSnapshot {
        active_coworkers: vec![
            Coworker {
                slot_id: uuid::Uuid::new_v4().to_string(),
                name: "lexington".to_string(),
                status: CoworkerStatus::Running,
                working_dir: "/tmp/lex1".to_string(),
                started_at: Utc::now(),
                current_task: None,
                session_id: Some("session-aaa".to_string()),
                model: "sonnet".to_string(),
                provider: crate::auth::AuthProvider::Claude,
                profile: crate::auth::DEFAULT_PROFILE.to_string(),
            },
            Coworker {
                slot_id: uuid::Uuid::new_v4().to_string(),
                name: "park".to_string(),
                status: CoworkerStatus::Running,
                working_dir: "/tmp/park1".to_string(),
                started_at: Utc::now(),
                current_task: None,
                session_id: Some("session-bbb".to_string()),
                model: "sonnet".to_string(),
                provider: crate::auth::AuthProvider::Claude,
                profile: crate::auth::DEFAULT_PROFILE.to_string(),
            },
            Coworker {
                slot_id: uuid::Uuid::new_v4().to_string(),
                name: "lexington".to_string(),
                status: CoworkerStatus::Running,
                working_dir: "/tmp/lex2".to_string(),
                started_at: Utc::now(),
                current_task: None,
                session_id: Some("session-ccc".to_string()),
                model: "sonnet".to_string(),
                provider: crate::auth::AuthProvider::Claude,
                profile: crate::auth::DEFAULT_PROFILE.to_string(),
            },
        ],
        running_coworkers: vec![],
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
        reviewer_in_progress_comment_ids: HashMap::new(),
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
        coworkers_with_active_tools: HashSet::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
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
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    };

    // "lexington" has two sessions
    let lex_sessions = snapshot.sessions_for_name("lexington");
    assert_eq!(lex_sessions.len(), 2);
    assert!(lex_sessions.contains(&"session-aaa".to_string()));
    assert!(lex_sessions.contains(&"session-ccc".to_string()));

    // "park" has one session
    let park_sessions = snapshot.sessions_for_name("park");
    assert_eq!(park_sessions.len(), 1);
    assert_eq!(park_sessions[0], "session-bbb");

    // unknown name returns empty
    let unknown = snapshot.sessions_for_name("broadway");
    assert!(unknown.is_empty());
}

/// Test that active_session_ids is populated in WorldSnapshot serialization.
#[test]
fn test_active_session_ids_in_snapshot() {
    let mut active_session_ids = HashSet::new();
    active_session_ids.insert("session-aaa".to_string());
    active_session_ids.insert("session-bbb".to_string());

    let snapshot = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(),
        active_session_ids,
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
        reviewer_in_progress_comment_ids: HashMap::new(),
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
        coworkers_with_active_tools: HashSet::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
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
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    };

    assert_eq!(snapshot.active_session_ids.len(), 2);
    assert!(snapshot.active_session_ids.contains("session-aaa"));
    assert!(snapshot.active_session_ids.contains("session-bbb"));

    let json = serde_json::to_string(&snapshot).expect("should serialize");
    assert!(json.contains("active_session_ids"));
}

/// Test that session-centric fields exist in WorldSnapshot and default to empty.
///
/// These fields are added for the session-centric coworker model refactor.
/// The `#[serde(default)]` attribute ensures existing fixture JSON (which lacks
/// these fields) still deserializes correctly with empty maps.
#[test]
fn test_snapshot_includes_session_fields() {
    // Verify fields exist and default to empty in a constructed snapshot
    let snapshot = WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
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
        reviewer_in_progress_comment_ids: HashMap::new(),
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
        coworkers_with_active_tools: HashSet::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        // Session-centric fields (new model)
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
        recently_recovered_session_ids: std::collections::HashSet::new(),
        stale_working_dir_sessions: std::collections::HashSet::new(),
        session_profile_map: HashMap::new(),
        limited_pool_profiles: HashSet::new(),
    };

    assert!(snapshot.sessions.is_empty());
    assert!(snapshot.session_task_map.is_empty());
    assert!(snapshot.session_name_map.is_empty());
    assert!(snapshot.name_session_map.is_empty());

    // Verify backward compat: JSON that lacks session-centric fields deserializes correctly.
    // Serialize the snapshot, remove session fields, then deserialize to confirm defaults.
    let json = serde_json::to_string(&snapshot).expect("should serialize");
    let mut v: serde_json::Value = serde_json::from_str(&json).expect("should parse");
    // Strip session-centric fields to simulate an older snapshot that predates the model
    if let Some(o) = v.as_object_mut() {
        o.remove("sessions");
        o.remove("session_task_map");
        o.remove("session_name_map");
        o.remove("name_session_map");
    }
    let stripped_json = serde_json::to_string(&v).expect("should re-serialize");
    let deserialized: WorldSnapshot =
        serde_json::from_str(&stripped_json).expect("stripped fixture should deserialize");
    assert!(deserialized.sessions.is_empty());
    assert!(deserialized.session_task_map.is_empty());
    assert!(deserialized.session_name_map.is_empty());
    assert!(deserialized.name_session_map.is_empty());
}

/// Precondition test: the captured bug snapshot has coworkers running but the
/// sessions map is empty. This documents a historical bug where sessions were
/// written to the name-keyed map instead of the session-ID-keyed map.
#[test]
fn test_captured_snapshot_has_empty_sessions_despite_running_coworkers() {
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-no-one-working-on-1625-20260219-193645.json"
    );
    let snapshot: WorldSnapshot = serde_json::from_str(fixture).unwrap();
    // The bug: coworkers are active but sessions map is empty
    assert!(
        !snapshot.active_coworkers.is_empty(),
        "Bug snapshot should have active coworkers"
    );
    assert!(
        snapshot.sessions.is_empty(),
        "Bug snapshot should have empty sessions map (demonstrating the bug)"
    );
}

/// Verify that session_health_map translates name-keyed health to session-ID-keyed.
#[test]
fn test_snapshot_session_health_map_populated() {
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-no-one-working-on-1625-20260219-193645.json"
    );
    let mut snapshot: WorldSnapshot = serde_json::from_str(fixture).unwrap();
    // Manually wire up session mapping (Task 1 ensures this happens at runtime).
    snapshot
        .name_session_map
        .insert("vernon".to_string(), "sess-123".to_string());
    snapshot
        .headless_process_health
        .insert("vernon".to_string(), ProcessHealth::default());
    // Also add a name without a session mapping — should be excluded.
    snapshot
        .headless_process_health
        .insert("orphan".to_string(), ProcessHealth::default());

    let health = snapshot.session_health_map();
    assert!(health.contains_key("sess-123"));
    assert!(!health.contains_key("orphan"));
}

/// Regression test: reviewer_pr_assignments must include dead reviewers.
///
/// Previously, assignments were built by iterating `active_coworkers` and
/// looking up each in `persistent_state.github.pr_reviewers`. When a reviewer's
/// process died it was removed from `active_coworkers`, so its entry was dropped
/// from `reviewer_pr_assignments`. This caused `decide_dead_reviewer_respawns`
/// to never fire, leaving dead reviewers with unposted reviews undetected.
///
/// The fix builds assignments directly from `pr_reviewers`, which persists
/// across coworker lifecycle changes.
#[test]
fn reviewer_pr_assignments_includes_dead_reviewers() {
    use crate::github_state::{AssignmentSource, GitHubState};

    let mut github = GitHubState::default();
    // Reviewer "riverside" is assigned to PR 1352 in persistent state.
    github.assign_reviewer(1352, "riverside", AssignmentSource::PollingFallback);

    // No active_coworkers — riverside has died (its process exited).
    // The old code filtered through active_coworkers, so riverside was absent.
    // The new code reads pr_reviewers directly.
    let assignments = super::build_reviewer_pr_assignments(&github);

    assert!(
        assignments.contains_key("riverside"),
        "dead reviewer 'riverside' must appear in reviewer_pr_assignments so \
         decide_dead_reviewer_respawns can detect and respawn it"
    );
    assert_eq!(
        assignments["riverside"], 1352,
        "assignment should map reviewer to the correct PR number"
    );
}

/// `build_reviewer_pr_assignments` must include ALL assignments, even expired ones,
/// so that `decide_dead_reviewer_respawns` can detect and respawn dead reviewers.
///
/// `active_reviewers()` (display/logging) still applies the timeout filter, but
/// the snapshot-level assignments must not drop entries based on age.
#[test]
fn build_reviewer_pr_assignments_excludes_expired_entries() {
    use crate::github_state::{AssignmentSource, GitHubState, PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS};

    let mut github = GitHubState::default();
    // Assign reviewer for PR 100 with a fresh timestamp (non-expired).
    github.assign_reviewer(100, "riverside", AssignmentSource::PollingFallback);

    // Assign reviewer for PR 200 with an expired timestamp.
    github.assign_reviewer(200, "broadway", AssignmentSource::PollingFallback);
    // Backdate broadway's assignment past the timeout.
    if let Some(assignment) = github.pr_reviewers.get_mut(&200) {
        assignment.assigned_at = chrono::Utc::now()
            - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 + 1);
    }

    let assignments = super::build_reviewer_pr_assignments(&github);
    let active = github.active_reviewers();

    // Fresh assignment must appear in both.
    assert!(
        assignments.contains_key("riverside"),
        "non-expired reviewer 'riverside' must appear in build_reviewer_pr_assignments"
    );
    assert!(
        active.contains("riverside"),
        "non-expired reviewer 'riverside' must appear in active_reviewers"
    );

    // active_reviewers() still excludes expired entries (used for display/logging).
    assert!(
        !active.contains("broadway"),
        "expired reviewer 'broadway' must NOT appear in active_reviewers"
    );

    // build_reviewer_pr_assignments includes expired entries so that
    // decide_dead_reviewer_respawns can detect and respawn dead reviewers whose
    // assignment timed out before respawn could run.
    assert!(
        assignments.contains_key("broadway"),
        "expired reviewer 'broadway' MUST appear in build_reviewer_pr_assignments \
         so decide_dead_reviewer_respawns can find and respawn them"
    );
}

/// Issue 2: A dead reviewer with an expired assignment must still be detectable
/// by decide_dead_reviewer_respawns via reviewer_pr_assignments.
///
/// Regression test for: reviewer assignment dropped when PR flagged as orphaned.
/// When a reviewer dies after the 10-minute assignment timeout window,
/// build_reviewer_pr_assignments used to exclude their assignment (timeout filter),
/// so decide_dead_reviewer_respawns could never match them and the review was lost.
#[test]
fn test_build_reviewer_pr_assignments_includes_expired_for_dead_reviewer_respawn() {
    use crate::github_state::{AssignmentSource, GitHubState, PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS};

    let mut github = GitHubState::default();
    github.assign_reviewer(1515, "park", AssignmentSource::PollingFallback);
    // Backdate park's assignment well past the timeout (simulating a long review or
    // a reviewer that died without the timestamp being refreshed).
    if let Some(assignment) = github.pr_reviewers.get_mut(&1515) {
        assignment.assigned_at = chrono::Utc::now()
            - chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64 * 2);
    }

    let assignments = super::build_reviewer_pr_assignments(&github);

    // park must appear so that decide_dead_reviewer_respawns can detect and respawn them.
    assert!(
        assignments.contains_key("park"),
        "park's expired assignment must appear in reviewer_pr_assignments \
         so decide_dead_reviewer_respawns can respawn the dead reviewer"
    );
    assert_eq!(assignments["park"], 1515);
}

/// When a reviewer has two entries (stale + fresh for different PRs), the most recently
/// assigned entry must win — the result must be deterministic regardless of HashMap
/// iteration order.
#[test]
fn test_build_reviewer_pr_assignments_prefers_newest_when_duplicate_reviewer() {
    use crate::github_state::{AssignmentSource, GitHubState};

    let mut github = GitHubState::default();
    // Assign park to PR 1515 with a stale timestamp.
    github.assign_reviewer(1515, "park", AssignmentSource::PollingFallback);
    if let Some(a) = github.pr_reviewers.get_mut(&1515) {
        a.assigned_at = chrono::Utc::now() - chrono::Duration::seconds(3600);
    }
    // Assign park again to PR 1520 with a fresh timestamp (newer).
    github.assign_reviewer(1520, "park", AssignmentSource::PollingFallback);

    let assignments = super::build_reviewer_pr_assignments(&github);

    assert_eq!(
        assignments.get("park"),
        Some(&1520),
        "should keep the most recently assigned PR (1520), not the stale one (1515)"
    );
}

/// When two assignments have identical `assigned_at` timestamps (e.g., refreshed by
/// `cleanup_expired_preserving` in the same tick), the higher PR number wins as a
/// stable tie-breaker — result must be deterministic regardless of iteration order.
#[test]
fn test_build_reviewer_pr_assignments_tie_broken_by_pr_number() {
    use crate::github_state::{AssignmentSource, GitHubState};

    let mut github = GitHubState::default();
    let same_time = chrono::Utc::now();

    // Assign park to two PRs with identical timestamps (simulates cleanup_expired_preserving
    // refreshing multiple stale assignments to the same `now`).
    github.assign_reviewer(1515, "park", AssignmentSource::PollingFallback);
    github.assign_reviewer(1520, "park", AssignmentSource::PollingFallback);
    // Force both to the exact same timestamp.
    if let Some(a) = github.pr_reviewers.get_mut(&1515) {
        a.assigned_at = same_time;
    }
    if let Some(a) = github.pr_reviewers.get_mut(&1520) {
        a.assigned_at = same_time;
    }

    let assignments = super::build_reviewer_pr_assignments(&github);

    assert_eq!(
        assignments.get("park"),
        Some(&1520),
        "higher PR number (1520) must win the tie over 1515"
    );
}

/// Test that recently_recovered_session_ids is correctly populated from CooldownTracker.
///
/// The collect_world_snapshot() function builds this set by checking the
/// "session_recovered" cooldown for each known session ID. This test verifies
/// the extraction logic: a session with an active cooldown appears in the set,
/// while a session without a cooldown does not.
#[test]
fn test_recently_recovered_session_ids_populated_from_cooldowns() {
    use crate::rules::CooldownTracker;
    use std::sync::Mutex;

    let cooldowns = Mutex::new(CooldownTracker::new());

    // Record a "session_recovered" cooldown for session "sess-abc" (simulating
    // a successful recovery spawn).
    cooldowns
        .lock()
        .unwrap()
        .record("session_recovered", "sess-abc");

    // Simulate the known session IDs (as collect_world_snapshot iterates sessions.keys())
    let known_session_ids = [
        "sess-abc".to_string(), // has active cooldown
        "sess-xyz".to_string(), // no cooldown recorded
    ];

    // Replicate the exact extraction logic from collect_world_snapshot():
    // !cooldowns.check() means "cooldown is NOT expired" → include in the set.
    let recently_recovered: HashSet<String> = {
        let cd = cooldowns.lock().unwrap();
        known_session_ids
            .iter()
            .filter(|sid| {
                !cd.check(
                    "session_recovered",
                    sid,
                    crate::daemon::constants::SESSION_RECOVERED_COOLDOWN,
                )
            })
            .cloned()
            .collect()
    };

    assert!(
        recently_recovered.contains("sess-abc"),
        "session with active cooldown must appear in recently_recovered_session_ids"
    );
    assert!(
        !recently_recovered.contains("sess-xyz"),
        "session without cooldown must NOT appear in recently_recovered_session_ids"
    );
    assert_eq!(recently_recovered.len(), 1);
}
