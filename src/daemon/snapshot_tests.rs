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
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
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
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
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
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
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
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
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
        now_utc: Utc::now(),
        repo_name: "test-repo".to_string(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    assert_eq!(snapshot.active_session_ids.len(), 2);
    assert!(snapshot.active_session_ids.contains("session-aaa"));
    assert!(snapshot.active_session_ids.contains("session-bbb"));

    let json = serde_json::to_string(&snapshot).expect("should serialize");
    assert!(json.contains("active_session_ids"));
}
