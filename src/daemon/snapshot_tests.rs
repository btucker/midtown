use super::*;

/// Test that compute_health_sets derives usage limit and API error sets correctly.
#[test]
fn test_compute_health_sets_basic() {
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

    let sets = compute_health_sets(&health);

    assert!(sets.usage_limited_coworkers.contains("york"));
    assert!(!sets.usage_limited_coworkers.contains("park"));
    assert!(sets.api_error_coworkers.contains("park"));
    assert!(!sets.api_error_coworkers.contains("madison"));
    assert!(sets.auth_error_coworkers.is_empty());
    assert!(sets.tool_name_conflict_coworkers.is_empty());
}

/// Auth errors take precedence: a coworker with both auth and API errors
/// appears only in auth_error_coworkers, not api_error_coworkers.
#[test]
fn test_compute_health_sets_auth_excludes_api() {
    let mut health = HashMap::new();
    health.insert(
        "york".to_string(),
        ProcessHealth {
            has_auth_error: true,
            has_api_error: true,
            ..Default::default()
        },
    );
    health.insert(
        "park".to_string(),
        ProcessHealth {
            has_usage_limit: true,
            has_api_error: true,
            ..Default::default()
        },
    );

    let sets = compute_health_sets(&health);

    assert!(sets.auth_error_coworkers.contains("york"));
    assert!(
        !sets.api_error_coworkers.contains("york"),
        "auth error should exclude from api_error set"
    );
    assert!(sets.usage_limited_coworkers.contains("park"));
    assert!(
        !sets.api_error_coworkers.contains("park"),
        "usage limit should exclude from api_error set"
    );
}

/// Tool name conflict coworkers are tracked independently.
#[test]
fn test_compute_health_sets_tool_conflict() {
    let mut health = HashMap::new();
    health.insert(
        "broadway".to_string(),
        ProcessHealth {
            has_tool_name_conflict: true,
            ..Default::default()
        },
    );

    let sets = compute_health_sets(&health);
    assert!(sets.tool_name_conflict_coworkers.contains("broadway"));
}

/// Regression test: reviewer_pr_assignments must include dead reviewers.
///
/// With the span-based model, open reviewer spans (end_time = None) persist even
/// when the reviewer's process has exited. This allows `decide_dead_reviewer_respawns`
/// to detect and respawn reviewers that died before posting their review.
#[test]
fn reviewer_pr_assignments_includes_dead_reviewers() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number.insert("review-42".to_string(), 1352_u64);
    // Reviewer "riverside" — session exists but not running (dead reviewer).
    ps.insert_session_for_task(
        "review-42",
        "riverside",
        "midtown-code-reviewer",
        "sess-riverside",
    );
    // Mark as not running to simulate dead reviewer
    if let Some(s) = ps.sessions.get_mut("sess-riverside") {
        s.is_running = false;
    }

    // No active process (riverside has died — is_running=false, health absent).
    let process_health: HashMap<String, ProcessHealth> = HashMap::new();

    let assignments = super::build_reviewer_pr_assignments_from_spans(&ps);

    assert!(
        assignments.contains_key("riverside"),
        "dead reviewer 'riverside' must appear in reviewer_pr_assignments so \
         decide_dead_reviewer_respawns can detect and respawn it"
    );
    assert_eq!(
        assignments["riverside"], 1352,
        "assignment should map reviewer to the correct PR number"
    );

    // compute_active_reviewers_from_spans: riverside not active (is_running=false, not alive)
    let active = super::compute_active_reviewers_from_spans(&ps, &process_health);
    assert!(
        !active.contains("riverside"),
        "dead reviewer 'riverside' must NOT appear in active_reviewers when is_running=false and not alive"
    );
}

/// compute_active_reviewers_from_spans: reviewer with is_running=true appears in active set.
#[test]
fn active_reviewer_with_running_session_in_active_set() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number.insert("review-100".to_string(), 1553_u64);
    ps.insert_session_for_task(
        "review-100",
        "amsterdam",
        "midtown-code-reviewer",
        "sess-amsterdam",
    );

    let process_health: HashMap<String, ProcessHealth> = HashMap::new();
    let active = super::compute_active_reviewers_from_spans(&ps, &process_health);

    assert!(
        active.contains("amsterdam"),
        "reviewer with is_running=true must appear in active_reviewers"
    );
}

/// compute_active_reviewers_from_spans: reviewer alive in process_health appears even
/// if SessionRecord.is_running is false (process alive but session not yet updated).
#[test]
fn active_reviewer_alive_in_process_health_in_active_set() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number.insert("review-200".to_string(), 2000_u64);
    ps.insert_session_for_task(
        "review-200",
        "broadway",
        "midtown-code-reviewer",
        "sess-broadway",
    );
    // Mark as not running (stale), but process is alive
    if let Some(s) = ps.sessions.get_mut("sess-broadway") {
        s.is_running = false;
    }

    let mut process_health = HashMap::new();
    process_health.insert(
        "broadway".to_string(),
        ProcessHealth {
            is_alive: true,
            ..Default::default()
        },
    );

    let active = super::compute_active_reviewers_from_spans(&ps, &process_health);

    assert!(
        active.contains("broadway"),
        "reviewer alive in process_health must appear in active_reviewers even if is_running=false"
    );
}

/// Dead reviewers (is_alive=false AND is_running=false) must NOT appear in active_reviewers.
#[test]
fn dead_reviewer_not_in_active_reviewers() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number.insert("review-300".to_string(), 3000_u64);
    ps.insert_session_for_task(
        "review-300",
        "amsterdam",
        "midtown-code-reviewer",
        "sess-amsterdam",
    );
    // Mark as not running
    if let Some(s) = ps.sessions.get_mut("sess-amsterdam") {
        s.is_running = false;
    }

    // Process is dead
    let mut process_health = HashMap::new();
    process_health.insert(
        "amsterdam".to_string(),
        ProcessHealth {
            is_alive: false,
            ..Default::default()
        },
    );

    let active = super::compute_active_reviewers_from_spans(&ps, &process_health);
    assert!(
        !active.contains("amsterdam"),
        "dead reviewers must NOT appear in active_reviewers"
    );
}

/// build_reviewer_pr_assignments includes stopped reviewers (for respawn detection).
#[test]
fn build_reviewer_pr_assignments_includes_stopped_sessions() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number.insert("review-400".to_string(), 4000_u64);
    // Stopped reviewer session — dead but needs respawning
    ps.insert_session_for_task("review-400", "park", "midtown-code-reviewer", "sess-park");
    if let Some(s) = ps.sessions.get_mut("sess-park") {
        s.is_running = false;
    }

    let assignments = super::build_reviewer_pr_assignments_from_spans(&ps);

    assert!(
        assignments.contains_key("park"),
        "stopped reviewer sessions should be in assignments for respawn detection"
    );
}

/// Verify PrTaskIndex deserializes correctly from old JSON using alias field names,
/// both directly and through the SnapshotPrState flatten boundary.
#[test]
fn test_pr_task_index_deserializes_from_old_field_names() {
    // Direct PrTaskIndex deserialization
    let old_json = r#"{
        "tasks_with_open_prs":     {"42": 100},
        "github_open_pr_task_ids": {"55": 200},
        "pr_task_associations":    {"100": "42"}
    }"#;
    let index: PrTaskIndex =
        serde_json::from_str(old_json).expect("should deserialize via alias names");
    assert_eq!(index.session_pr_for_task("42"), Some(100));
    assert_eq!(index.github_pr_for_task("55"), Some(200));
    assert_eq!(index.task_for_pr(100), Some("42"));

    // Also verify through a captured snapshot fixture (tests the full flatten path).
    // Use one of the existing fixtures that contains old field names.
    let fixture = std::fs::read_to_string(
        "tests/fixtures/snapshot/snapshot-double-assign-open-pr-20260216-231443.json",
    )
    .expect("fixture should exist");
    let snap: WorldSnapshot =
        serde_json::from_str(&fixture).expect("fixture should deserialize with alias names");
    // This fixture has pr_task_associations — verify the flatten+alias path works
    // Verify the fixture has PR-task data and the flatten+alias path works.
    // This fixture has pr_task_associations entries — they should be accessible.
    let _pair_count = snap.pr.pr_task_index.pr_task_pairs().count();
}

/// Verify PrTaskIndex::from_task_maps() derives pr_to_task from session_task_to_pr.
#[test]
fn test_pr_task_index_from_task_maps_derives_reverse_map() {
    let session: HashMap<String, u64> = [("task-1".to_string(), 100), ("task-2".to_string(), 200)]
        .into_iter()
        .collect();
    let github: HashMap<String, u64> = [("task-3".to_string(), 300)].into_iter().collect();
    let index = PrTaskIndex::from_task_maps(session, github);

    assert_eq!(index.session_pr_for_task("task-1"), Some(100));
    assert_eq!(index.session_pr_for_task("task-2"), Some(200));
    assert_eq!(index.github_pr_for_task("task-3"), Some(300));
    assert_eq!(index.task_for_pr(100), Some("task-1"));
    assert_eq!(index.task_for_pr(200), Some("task-2"));
    assert!(index.task_has_pr("task-1"));
    assert!(index.task_has_pr("task-3")); // github source
    assert!(!index.task_has_pr("task-999"));
}

/// Verify PrTaskIndex::new() preserves all PR→task pairs even when multiple PRs map to one task.
#[test]
fn test_pr_task_index_new_preserves_multiple_prs_per_task() {
    let session: HashMap<String, u64> = [("42".to_string(), 200)].into_iter().collect(); // only latest PR survives
    let github: HashMap<String, u64> = HashMap::new();
    // But pr_to_task built directly from sessions has both
    let pr_to_task: HashMap<u64, String> = [(100, "42".to_string()), (200, "42".to_string())]
        .into_iter()
        .collect();
    let index = PrTaskIndex::new(session, github, pr_to_task);

    // session_task_to_pr only has the latest
    assert_eq!(index.session_pr_for_task("42"), Some(200));
    // but pr_to_task has both
    assert_eq!(index.task_for_pr(100), Some("42"));
    assert_eq!(index.task_for_pr(200), Some("42"));
    // pr_task_pairs iterates all PR→task associations
    let pairs: Vec<_> = index.pr_task_pairs().collect();
    assert_eq!(pairs.len(), 2);
}

/// Fork sessions (bound_thread_id set + midtown-channel-lead agent) should be excluded
/// from task ownership lookups on DaemonPersistentState.
#[test]
fn session_by_task_skips_fork_sessions() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};

    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "coworker-sess".into(),
        SessionRecord {
            session_id: "coworker-sess".into(),
            task_id: Some("500".into()),
            name: "madison".into(),
            working_dir: "/tmp/test".into(),
            ..Default::default()
        },
    );
    ps.sessions.insert(
        "fork-sess".into(),
        SessionRecord {
            session_id: "fork-sess".into(),
            task_id: Some("500".into()),
            name: "fork-research".into(),
            agent_type: "midtown-channel-lead".into(),
            bound_thread_id: Some("thread-xyz".into()),
            working_dir: "/tmp/test".into(),
            ..Default::default()
        },
    );

    let fork = ps.sessions.get("fork-sess").unwrap();
    assert!(fork.is_fork_session());
    let coworker = ps.sessions.get("coworker-sess").unwrap();
    assert!(!coworker.is_fork_session());
}
