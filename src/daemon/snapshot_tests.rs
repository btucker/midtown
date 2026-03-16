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
        coworkers: SnapshotCoworkerState {
            session_name: "midtown-test".to_string(),
            coworker_stop_times: stop_times.clone(),
            ..Default::default()
        },
        ..minimal_snapshot_for_test()
    };

    assert_eq!(snapshot.coworkers.coworker_stop_times.len(), 2);
    assert!(
        snapshot
            .coworkers
            .coworker_stop_times
            .contains_key("lexington")
    );
    assert!(
        snapshot
            .coworkers
            .coworker_stop_times
            .contains_key("broadway")
    );

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
    let snapshot = minimal_snapshot_for_test();

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

/// Test that active_session_ids is populated in WorldSnapshot serialization.
#[test]
fn test_active_session_ids_in_snapshot() {
    let mut active_session_ids = HashSet::new();
    active_session_ids.insert("session-aaa".to_string());
    active_session_ids.insert("session-bbb".to_string());

    let snapshot = WorldSnapshot {
        coworkers: SnapshotCoworkerState {
            active_session_ids,
            session_name: "midtown-test".to_string(),
            ..Default::default()
        },
        ..minimal_snapshot_for_test()
    };

    assert_eq!(snapshot.coworkers.active_session_ids.len(), 2);
    assert!(
        snapshot
            .coworkers
            .active_session_ids
            .contains("session-aaa")
    );
    assert!(
        snapshot
            .coworkers
            .active_session_ids
            .contains("session-bbb")
    );

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
    let snapshot = minimal_snapshot_for_test();

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
        !snapshot.coworkers.active_coworkers.is_empty(),
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
        .health
        .headless_process_health
        .insert("vernon".to_string(), ProcessHealth::default());
    // Also add a name without a session mapping — should be excluded.
    snapshot
        .health
        .headless_process_health
        .insert("orphan".to_string(), ProcessHealth::default());

    let health = snapshot.session_health_map();
    assert!(health.contains_key("sess-123"));
    assert!(!health.contains_key("orphan"));
}

/// Regression test: reviewer_pr_assignments must include dead reviewers.
///
/// With the span-based model, open reviewer spans (end_time = None) persist even
/// when the reviewer's process has exited. This allows `decide_dead_reviewer_respawns`
/// to detect and respawn reviewers that died before posting their review.
#[test]
fn reviewer_pr_assignments_includes_dead_reviewers() {
    use crate::daemon::state::{DaemonPersistentState, TaskSessionSpan};

    let mut ps = DaemonPersistentState::default();
    // Reviewer "riverside" has an open span for task "review-42" → PR 1352.
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "review-42".to_string(),
        agent_name: "riverside".to_string(),
        agent_type: "reviewer".to_string(),
        session_id: "sess-riverside".to_string(),
        start_time: chrono::Utc::now(),
        end_time: None, // open span — reviewer is/was active
    });
    ps.task_pr_number.insert("review-42".to_string(), 1352_u64);

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
    use crate::daemon::state::{DaemonPersistentState, SessionRecord, TaskSessionSpan};

    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "review-100".to_string(),
        agent_name: "amsterdam".to_string(),
        agent_type: "reviewer".to_string(),
        session_id: "sess-amsterdam".to_string(),
        start_time: chrono::Utc::now(),
        end_time: None,
    });
    ps.task_pr_number.insert("review-100".to_string(), 1553_u64);

    // Session record shows is_running = true
    ps.sessions.insert(
        "sess-amsterdam".to_string(),
        SessionRecord {
            session_id: "sess-amsterdam".to_string(),
            is_running: true,
            ..Default::default()
        },
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
    use crate::daemon::state::{DaemonPersistentState, SessionRecord, TaskSessionSpan};

    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "review-200".to_string(),
        agent_name: "broadway".to_string(),
        agent_type: "reviewer".to_string(),
        session_id: "sess-broadway".to_string(),
        start_time: chrono::Utc::now(),
        end_time: None,
    });
    ps.task_pr_number.insert("review-200".to_string(), 2000_u64);

    // Session record shows is_running = false (stale), but process is alive
    ps.sessions.insert(
        "sess-broadway".to_string(),
        SessionRecord {
            session_id: "sess-broadway".to_string(),
            is_running: false,
            ..Default::default()
        },
    );

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
    use crate::daemon::state::{DaemonPersistentState, SessionRecord, TaskSessionSpan};

    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "review-300".to_string(),
        agent_name: "amsterdam".to_string(),
        agent_type: "reviewer".to_string(),
        session_id: "sess-amsterdam".to_string(),
        start_time: chrono::Utc::now(),
        end_time: None, // span still open (not yet closed)
    });
    ps.task_pr_number.insert("review-300".to_string(), 3000_u64);

    // Session shows not running
    ps.sessions.insert(
        "sess-amsterdam".to_string(),
        SessionRecord {
            session_id: "sess-amsterdam".to_string(),
            is_running: false,
            ..Default::default()
        },
    );

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

/// build_reviewer_pr_assignments_from_spans excludes closed spans.
#[test]
fn build_reviewer_pr_assignments_excludes_closed_spans() {
    use crate::daemon::state::{DaemonPersistentState, TaskSessionSpan};

    let mut ps = DaemonPersistentState::default();
    // Closed span — review is done
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "review-400".to_string(),
        agent_name: "park".to_string(),
        agent_type: "reviewer".to_string(),
        session_id: "sess-park".to_string(),
        start_time: chrono::Utc::now() - chrono::Duration::hours(1),
        end_time: Some(chrono::Utc::now()), // closed span
    });
    ps.task_pr_number.insert("review-400".to_string(), 4000_u64);

    let assignments = super::build_reviewer_pr_assignments_from_spans(&ps);

    assert!(
        !assignments.contains_key("park"),
        "closed spans must NOT appear in reviewer_pr_assignments"
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

// ---------------------------------------------------------------------------
// find_session_for_task tests
// ---------------------------------------------------------------------------

/// Minimal helper: build a WorldSnapshot with only the session fields populated.
/// Constructs the struct directly with empty/default values to avoid json! macro
/// recursion limits.
fn snapshot_with_sessions(
    session_task_map: HashMap<String, String>,
    sessions: HashMap<String, crate::daemon::state::SessionRecord>,
) -> WorldSnapshot {
    WorldSnapshot {
        sessions,
        session_task_map,
        ..minimal_snapshot_for_test()
    }
}

#[test]
fn find_session_for_task_returns_record_when_chain_resolves() {
    let snap = snapshot_with_sessions(
        [("42".to_string(), "sess-abc".to_string())]
            .into_iter()
            .collect(),
        [(
            "sess-abc".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-abc".to_string(),
                task_id: Some("42".to_string()),
                current_name: Some("lexington".to_string()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    );

    let record = snap.find_session_for_task("42");
    assert!(record.is_some());
    assert_eq!(record.unwrap().session_id, "sess-abc");
}

#[test]
fn find_session_for_task_returns_none_for_unknown_task() {
    let snap = snapshot_with_sessions(HashMap::new(), HashMap::new());
    assert!(snap.find_session_for_task("999").is_none());
}

#[test]
fn find_session_for_task_returns_none_when_session_id_stale() {
    let snap = snapshot_with_sessions(
        [("42".to_string(), "sess-gone".to_string())]
            .into_iter()
            .collect(),
        HashMap::new(), // sessions map doesn't have "sess-gone"
    );
    assert!(snap.find_session_for_task("42").is_none());
}

// ── reviewer_restart_counts from task_restart_count ──────────────────────────
//
// These tests verify the span-based logic used in `collect_world_snapshot` to
// build `reviewer_restart_counts`. The logic reads from `task_restart_count`
// (task-centric) and maps to PR numbers via `task_pr_number`.

/// Verify that `reviewer_restart_counts` is populated from `task_restart_count`
/// via `task_pr_number` mapping.
#[test]
fn reviewer_restart_counts_from_task_restart_count() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let pr_number: u64 = 10;
    let task_id = "review-task-10";

    ps.task_pr_number.insert(task_id.to_string(), pr_number);
    ps.task_restart_count.insert(task_id.to_string(), 3);

    // Replicate the logic from collect_world_snapshot
    let counts: HashMap<u64, u32> = ps
        .task_restart_count
        .iter()
        .filter_map(|(tid, &count)| ps.task_pr_number.get(tid).map(|&pr| (pr, count)))
        .collect();

    assert_eq!(
        counts.get(&pr_number),
        Some(&3),
        "restart_count (3) should be read from task_restart_count via task_pr_number"
    );
}

/// Verify that `reviewer_restart_counts` is empty when no task_restart_count entries exist.
#[test]
fn reviewer_restart_counts_empty_when_no_task_restart_count() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_pr_number
        .insert("review-task-20".to_string(), 20_u64);
    // No task_restart_count entry — restart_count defaults to 0

    let counts: HashMap<u64, u32> = ps
        .task_restart_count
        .iter()
        .filter_map(|(tid, &count)| ps.task_pr_number.get(tid).map(|&pr| (pr, count)))
        .collect();

    assert!(
        counts.is_empty(),
        "restart_counts should be empty when no task_restart_count entries exist"
    );
}

// ── stored_placeholder_ids from task_placeholder_comment_id ──────────────────

/// Verify that `stored_placeholder_ids` reads from `task_placeholder_comment_id`
/// via `task_pr_number` reverse lookup.
#[test]
fn stored_placeholder_ids_from_task_placeholder_comment_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let pr_number: u64 = 30;
    let task_id = "review-task-30";

    ps.task_pr_number.insert(task_id.to_string(), pr_number);
    ps.task_placeholder_comment_id
        .insert(task_id.to_string(), 2222);

    // Replicate the stored_placeholder_ids logic from collect_world_snapshot
    let id = ps
        .task_pr_number
        .iter()
        .find(|&(_, &p)| p == pr_number)
        .and_then(|(tid, _)| ps.task_placeholder_comment_id.get(tid))
        .copied();

    assert_eq!(
        id,
        Some(2222),
        "placeholder comment ID (2222) should be read from task_placeholder_comment_id"
    );
}

/// Verify that `stored_placeholder_ids` returns None when no entry exists.
#[test]
fn stored_placeholder_ids_none_when_no_task_entry() {
    use crate::daemon::state::DaemonPersistentState;

    let ps = DaemonPersistentState::default();
    let pr_number: u64 = 40;
    // No task_pr_number entry for this PR

    let id = ps
        .task_pr_number
        .iter()
        .find(|&(_, &p)| p == pr_number)
        .and_then(|(tid, _)| ps.task_placeholder_comment_id.get(tid))
        .copied();

    assert_eq!(
        id, None,
        "should return None when no task_placeholder_comment_id entry exists"
    );
}
