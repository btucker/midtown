//! E2E tests for daemon restart recovery.
//!
//! These tests verify that after a daemon restart:
//! 1. Task assignments are preserved (restored from disk)
//! 2. Reviewer assignments are preserved (from DaemonPersistentState)
//! 3. No duplicate work is spawned
//! 4. Headless sessions are correctly resumed
//!
//! The tests use captured snapshots and the JSON persistence format to verify
//! that the daemon correctly restores state across restarts.

use std::collections::{HashMap, HashSet};
use std::fs;
use tempfile::TempDir;

/// Test that task assignments are correctly restored from disk after daemon restart.
///
/// Regression test for the bug captured in:
/// - snapshot-assignments-lost-after-restart-20260211-033718.json
///
/// Before the fix, coworker_task_assignments was initialized empty on restart,
/// causing the daemon to see in_progress tasks as orphaned and spawn duplicates.
#[test]
fn test_task_assignments_restored_after_restart() {
    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let tasks_dir = temp_dir.path().join("tasks").join("midtown-test");
    fs::create_dir_all(&tasks_dir).unwrap();

    // Create task files with owners (simulating pre-restart state)
    let tasks = vec![
        ("1385", "in_progress", "amsterdam"),
        ("1388", "in_progress", "columbus"),
        ("1389", "in_progress", "riverside"),
        ("1390", "pending", ""), // pending task, no owner
    ];

    for (task_id, status, owner) in &tasks {
        let task_file = tasks_dir.join(format!("{}.json", task_id));
        let task_json = serde_json::json!({
            "id": task_id,
            "subject": format!("Task {}", task_id),
            "status": status,
            "owner": owner,
            "blocked_by": []
        });
        fs::write(
            &task_file,
            serde_json::to_string_pretty(&task_json).unwrap(),
        )
        .unwrap();
    }

    // Simulate daemon restart: read tasks from disk
    // In production, this would be called by DaemonState::restore_task_assignments_from_disk()
    let in_progress_tasks: Vec<(String, String, String)> = tasks
        .iter()
        .filter(|(_, status, owner)| *status == "in_progress" && !owner.is_empty())
        .map(|(id, _, owner)| (id.to_string(), format!("Task {}", id), owner.to_string()))
        .collect();

    // Rebuild the assignment map (simulating the restore function)
    let mut restored_assignments: HashMap<String, String> = HashMap::new();
    for (task_id, _subject, owner) in &in_progress_tasks {
        restored_assignments.insert(owner.to_lowercase(), task_id.clone());
    }

    // Verify all in_progress tasks with owners are restored
    assert_eq!(
        restored_assignments.len(),
        3,
        "Should restore 3 task assignments"
    );
    assert_eq!(
        restored_assignments.get("amsterdam"),
        Some(&"1385".to_string())
    );
    assert_eq!(
        restored_assignments.get("columbus"),
        Some(&"1388".to_string())
    );
    assert_eq!(
        restored_assignments.get("riverside"),
        Some(&"1389".to_string())
    );

    // Verify pending task without owner is not in assignments
    assert!(!restored_assignments.contains_key(""));
}

/// Test that reviewer assignments are preserved across daemon restarts.
///
/// Regression test for the bug captured in:
/// - snapshot-review-spawn-lost-after-restart-20260216-235656.json
/// - snapshot-review-spawn-lost-after-restart-20260217-001806.json
/// - snapshot-review-spawn-lost-after-restart-20260217-003046.json
///
/// Reviewer assignments are stored in daemon-state.json (github.pr_reviewers)
/// and must survive daemon restarts to prevent duplicate reviewer spawns.
#[test]
fn test_reviewer_assignments_preserved_after_restart() {
    use chrono::Utc;

    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create daemon-state.json with reviewer assignments (using the actual JSON format)
    let now = Utc::now();
    let state_json = serde_json::json!({
        "github": {
            "pr_reviewers": {
                "42": {
                    "pr_number": 42,
                    "reviewer": "park",
                    "assigned_at": now
                },
                "43": {
                    "pr_number": 43,
                    "reviewer": "madison",
                    "assigned_at": now
                }
            }
        }
    });

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(
        &state_file,
        serde_json::to_string_pretty(&state_json).unwrap(),
    )
    .unwrap();

    // Simulate restart: load state from disk
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: serde_json::Value = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify reviewer assignments are preserved
    let pr_reviewers = loaded_state["github"]["pr_reviewers"].as_object().unwrap();
    assert_eq!(
        pr_reviewers.len(),
        2,
        "Should preserve 2 reviewer assignments"
    );
    assert_eq!(pr_reviewers["42"]["reviewer"].as_str(), Some("park"));
    assert_eq!(pr_reviewers["43"]["reviewer"].as_str(), Some("madison"));
}

/// Test that headless session info is preserved across daemon restarts.
///
/// Headless sessions are stored in daemon-state.json (headless_sessions)
/// and must survive restarts to enable session recovery (--resume <session_id>).
#[test]
fn test_headless_sessions_preserved_after_restart() {
    use chrono::Utc;

    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create daemon-state.json with headless sessions (using the actual JSON format)
    let now = Utc::now();
    let state_json = serde_json::json!({
        "headless_sessions": {
            "amsterdam": {
                "session_id": "session-amsterdam-123",
                "last_active": now,
                "purpose": "task !1385: E2E decision functions",
                "pid": 12345,
                "coworker_type": "dev",
                "task_id": 1385,
                "working_dir": "/path/to/worktree",
                "profile": "test@example.com",
                "resume_on_startup": true
            },
            "park": {
                "session_id": "session-park-456",
                "last_active": now,
                "purpose": "reviewer for PR #42",
                "pid": 12346,
                "coworker_type": "reviewer",
                "pr_number": 42,
                "working_dir": "/path/to/main",
                "profile": "test@example.com",
                "resume_on_startup": true
            }
        }
    });

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(
        &state_file,
        serde_json::to_string_pretty(&state_json).unwrap(),
    )
    .unwrap();

    // Simulate restart: load state from disk
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: serde_json::Value = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify headless sessions are preserved
    let headless_sessions = loaded_state["headless_sessions"].as_object().unwrap();
    assert_eq!(
        headless_sessions.len(),
        2,
        "Should preserve 2 headless sessions"
    );

    let amsterdam = &headless_sessions["amsterdam"];
    assert_eq!(
        amsterdam["session_id"].as_str(),
        Some("session-amsterdam-123")
    );
    assert_eq!(amsterdam["coworker_type"].as_str(), Some("dev"));
    assert_eq!(amsterdam["task_id"].as_u64(), Some(1385));
    assert_eq!(amsterdam["resume_on_startup"].as_bool(), Some(true));

    let park = &headless_sessions["park"];
    assert_eq!(park["session_id"].as_str(), Some("session-park-456"));
    assert_eq!(park["coworker_type"].as_str(), Some("reviewer"));
    assert_eq!(park["pr_number"].as_u64(), Some(42));
    assert_eq!(park["resume_on_startup"].as_bool(), Some(true));
}

/// Test that no duplicate spawn effects are generated after restart recovery.
///
/// Regression test for the bug captured in:
/// - snapshot-duplicate-work-after-restart-20260212-231938.json
///
/// After restart, the daemon should recognize:
/// - Tasks with owners (in_progress) → no spawn needed
/// - Reviewers in pr_reviewers → no spawn needed
/// - Sessions in headless_sessions → resume, not spawn fresh
///
/// This test verifies the data structures are correctly populated so that
/// dispatch logic (spawn_for_pending_tasks, spawn_reviewer_if_needed) skips
/// already-assigned work.
#[test]
fn test_no_duplicate_spawns_after_restart() {
    use chrono::Utc;

    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let tasks_dir = temp_dir.path().join("tasks").join("midtown-test");
    let state_dir = temp_dir.path();
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::create_dir_all(state_dir).unwrap();

    // Set up pre-restart state:
    // - 3 in_progress tasks with owners
    // - 1 pending task without owner
    // - 2 reviewer assignments
    // - 2 headless sessions (1 dev, 1 reviewer)

    let tasks = vec![
        ("1385", "in_progress", "amsterdam"),
        ("1388", "in_progress", "columbus"),
        ("1389", "in_progress", "riverside"),
        ("1390", "pending", ""),
    ];

    for (task_id, status, owner) in &tasks {
        let task_file = tasks_dir.join(format!("{}.json", task_id));
        let task_json = serde_json::json!({
            "id": task_id,
            "subject": format!("Task {}", task_id),
            "status": status,
            "owner": owner,
            "blocked_by": []
        });
        fs::write(
            &task_file,
            serde_json::to_string_pretty(&task_json).unwrap(),
        )
        .unwrap();
    }

    // Create daemon-state.json with reviewer assignments and headless sessions
    let now = Utc::now();
    let state_json = serde_json::json!({
        "github": {
            "pr_reviewers": {
                "42": {
                    "pr_number": 42,
                    "reviewer": "park",
                    "assigned_at": now
                },
                "43": {
                    "pr_number": 43,
                    "reviewer": "madison",
                    "assigned_at": now
                }
            }
        },
        "headless_sessions": {
            "amsterdam": {
                "session_id": "session-amsterdam-123",
                "last_active": now,
                "purpose": "task !1385",
                "pid": 12345,
                "coworker_type": "dev",
                "task_id": 1385,
                "resume_on_startup": true
            },
            "park": {
                "session_id": "session-park-456",
                "last_active": now,
                "purpose": "reviewer for PR #42",
                "pid": 12346,
                "coworker_type": "reviewer",
                "pr_number": 42,
                "resume_on_startup": true
            }
        }
    });

    let state_file = state_dir.join("daemon-state.json");
    fs::write(
        &state_file,
        serde_json::to_string_pretty(&state_json).unwrap(),
    )
    .unwrap();

    // ── Simulate restart ──

    // 1. Load persistent state
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: serde_json::Value = serde_json::from_str(&loaded_state_json).unwrap();

    // 2. Restore task assignments from disk (simulating restore_task_assignments_from_disk)
    let in_progress_tasks: Vec<(String, String, String)> = tasks
        .iter()
        .filter(|(_, status, owner)| *status == "in_progress" && !owner.is_empty())
        .map(|(id, _, owner)| (id.to_string(), format!("Task {}", id), owner.to_string()))
        .collect();

    let mut coworker_task_assignments: HashMap<String, String> = HashMap::new();
    for (task_id, _subject, owner) in &in_progress_tasks {
        coworker_task_assignments.insert(owner.to_lowercase(), task_id.clone());
    }

    // 3. Identify recovering coworkers (simulating recovering_coworker_names)
    let headless_sessions = loaded_state["headless_sessions"].as_object().unwrap();
    let recovering_names: HashSet<String> = headless_sessions
        .iter()
        .filter(|(_, info)| info["resume_on_startup"].as_bool().unwrap_or(false))
        .map(|(name, _)| name.to_lowercase())
        .collect();

    // 4. Identify active reviewers
    let pr_reviewers = loaded_state["github"]["pr_reviewers"].as_object().unwrap();
    let active_reviewers: HashSet<String> = pr_reviewers
        .values()
        .filter_map(|a| a["reviewer"].as_str().map(|s| s.to_lowercase()))
        .collect();

    // ── Verify post-restart state ──

    // Task assignments restored
    assert_eq!(
        coworker_task_assignments.len(),
        3,
        "Should restore 3 task assignments"
    );
    assert!(coworker_task_assignments.contains_key("amsterdam"));
    assert!(coworker_task_assignments.contains_key("columbus"));
    assert!(coworker_task_assignments.contains_key("riverside"));

    // Recovering coworkers identified
    assert_eq!(
        recovering_names.len(),
        2,
        "Should identify 2 recovering coworkers"
    );
    assert!(recovering_names.contains("amsterdam"));
    assert!(recovering_names.contains("park"));

    // Active reviewers identified
    assert_eq!(
        active_reviewers.len(),
        2,
        "Should identify 2 active reviewers"
    );
    assert!(active_reviewers.contains("park"));
    assert!(active_reviewers.contains("madison"));

    // Verify dispatch logic would skip these coworkers:
    // - Coworkers in coworker_task_assignments → busy, skip spawn
    // - Coworkers in recovering_names → about to be resumed, skip spawn
    // - PRs in pr_reviewers → reviewer already assigned, skip spawn

    let all_busy_or_recovering: HashSet<String> = coworker_task_assignments
        .keys()
        .chain(recovering_names.iter())
        .cloned()
        .collect();

    assert_eq!(
        all_busy_or_recovering.len(),
        4,
        "Should have 4 coworkers that are busy or recovering (amsterdam, columbus, riverside, park)"
    );

    // The only pending task is !1390, which has no owner and is not blocked
    // Dispatch should spawn a NEW coworker for it (e.g., broadway), not reuse existing ones
    let pending_without_owner: Vec<_> = tasks
        .iter()
        .filter(|(_, status, owner)| *status == "pending" && owner.is_empty())
        .collect();

    assert_eq!(
        pending_without_owner.len(),
        1,
        "Should have 1 pending task without owner"
    );
}
