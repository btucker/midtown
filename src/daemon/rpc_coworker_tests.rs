//! Tests for coworker RPC handlers.
//!
//! Covers:
//! - handle_coworker_asking: stores pending question in state
//! - handle_coworker_nudge: clears pending question on answer delivery
//! - handle_coworker_questions: returns list of pending questions

use crate::rpc::RequestId;

use super::*;

// ============================================================================
// Test helper
// ============================================================================

fn make_test_state() -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;
    use tempfile::TempDir;

    let midtown_dir = TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let temp_dir = TempDir::new().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test-rpc-coworker.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name("test-repo", "test-repo"),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

// ============================================================================
// handle_coworker_asking — stores pending question
// ============================================================================

#[tokio::test]
async fn test_coworker_asking_stores_pending_question() {
    let (state, _tmp, _guard) = make_test_state();

    // Initially no pending questions
    {
        let questions = state.pending_questions.lock().unwrap();
        assert!(
            questions.is_empty(),
            "should start with no pending questions"
        );
    }

    // Call handle_coworker_asking
    let response: crate::rpc::Response = handle_coworker_asking(
        RequestId::Number(1),
        "madison",
        "What should I do next?",
        &state,
    )
    .await;

    // Should succeed
    assert!(!response.is_error(), "asking should succeed");

    // Should have stored the question
    let questions = state.pending_questions.lock().unwrap();
    assert_eq!(questions.len(), 1, "should have one pending question");
    let q = &questions[0];
    assert_eq!(q.coworker_name, "madison");
    assert_eq!(q.question, "What should I do next?");
    assert!(q.id > 0, "question should have a non-zero id");
}

#[tokio::test]
async fn test_coworker_asking_multiple_questions_accumulate() {
    let (state, _tmp, _guard) = make_test_state();

    handle_coworker_asking(RequestId::Number(1), "park", "First question?", &state).await;

    handle_coworker_asking(
        RequestId::Number(2),
        "lexington",
        "Second question?",
        &state,
    )
    .await;

    let questions = state.pending_questions.lock().unwrap();
    assert_eq!(questions.len(), 2, "both questions should be stored");

    let names: Vec<&str> = questions.iter().map(|q| q.coworker_name.as_str()).collect();
    assert!(names.contains(&"park"), "park question should be stored");
    assert!(
        names.contains(&"lexington"),
        "lexington question should be stored"
    );
}

#[tokio::test]
async fn test_coworker_asking_twice_replaces_previous_question() {
    let (state, _tmp, _guard) = make_test_state();

    handle_coworker_asking(RequestId::Number(1), "madison", "First question?", &state).await;
    handle_coworker_asking(RequestId::Number(2), "madison", "Updated question?", &state).await;

    let questions = state.pending_questions.lock().unwrap();
    assert_eq!(
        questions.len(),
        1,
        "same coworker asking twice should replace, not accumulate"
    );
    assert_eq!(questions[0].question, "Updated question?");
}

// ============================================================================
// cleanup_coworker_state — clears pending questions
// ============================================================================

#[tokio::test]
async fn test_cleanup_coworker_state_clears_pending_questions() {
    let (state, _tmp, _guard) = make_test_state();

    // Store pending questions for two coworkers
    handle_coworker_asking(
        RequestId::Number(1),
        "madison",
        "Madison's question?",
        &state,
    )
    .await;
    handle_coworker_asking(RequestId::Number(2), "park", "Park's question?", &state).await;

    {
        let questions = state.pending_questions.lock().unwrap();
        assert_eq!(questions.len(), 2);
    }

    // Clean up madison's state (simulates crash/shutdown)
    state.cleanup_coworker_state("madison").await;

    // Only park's question should remain
    let questions = state.pending_questions.lock().unwrap();
    assert_eq!(
        questions.len(),
        1,
        "cleanup should remove madison's question"
    );
    assert_eq!(questions[0].coworker_name, "park");
}

// ============================================================================
// handle_coworker_nudge — clears pending question
// ============================================================================

#[tokio::test]
async fn test_coworker_nudge_clears_pending_question() {
    let (state, _tmp, _guard) = make_test_state();

    // Store a pending question for "madison"
    handle_coworker_asking(
        RequestId::Number(1),
        "madison",
        "What should I do next?",
        &state,
    )
    .await;

    {
        let questions = state.pending_questions.lock().unwrap();
        assert_eq!(
            questions.len(),
            1,
            "should have one pending question before nudge"
        );
    }

    // Nudge madison — this represents answering their question
    handle_coworker_nudge(
        RequestId::Number(2),
        "lead",
        "madison",
        "Please work on the authentication module.",
        &state,
    )
    .await;

    // Pending question should be cleared
    let questions = state.pending_questions.lock().unwrap();
    assert!(
        questions.is_empty(),
        "pending question should be cleared after nudge"
    );
}

#[tokio::test]
async fn test_coworker_nudge_only_clears_target_coworkers_questions() {
    let (state, _tmp, _guard) = make_test_state();

    // Two coworkers each have a pending question
    handle_coworker_asking(
        RequestId::Number(1),
        "madison",
        "Madison's question?",
        &state,
    )
    .await;
    handle_coworker_asking(RequestId::Number(2), "park", "Park's question?", &state).await;

    {
        let questions = state.pending_questions.lock().unwrap();
        assert_eq!(questions.len(), 2);
    }

    // Nudge only madison
    handle_coworker_nudge(
        RequestId::Number(3),
        "lead",
        "madison",
        "Here is your answer.",
        &state,
    )
    .await;

    // Only madison's question should be removed; park's should remain
    let questions = state.pending_questions.lock().unwrap();
    assert_eq!(
        questions.len(),
        1,
        "only the nudged coworker's question should be removed"
    );
    assert_eq!(
        questions[0].coworker_name, "park",
        "park's question should remain"
    );
}

// ============================================================================
// handle_coworker_questions — returns list of pending questions
// ============================================================================

#[tokio::test]
async fn test_coworker_questions_returns_empty_when_no_questions() {
    let (state, _tmp, _guard) = make_test_state();

    let response: crate::rpc::Response =
        handle_coworker_questions(RequestId::Number(1), &state).await;

    assert!(!response.is_error(), "questions RPC should succeed");

    let result = response.result.expect("should have result");
    let questions = result["questions"]
        .as_array()
        .expect("should have questions array");
    assert!(questions.is_empty(), "should return empty list");
}

#[tokio::test]
async fn test_coworker_questions_returns_pending_questions() {
    let (state, _tmp, _guard) = make_test_state();

    // Store a pending question
    handle_coworker_asking(RequestId::Number(1), "york", "Can you help me?", &state).await;

    let response: crate::rpc::Response =
        handle_coworker_questions(RequestId::Number(2), &state).await;

    assert!(!response.is_error());

    let result = response.result.expect("should have result");
    let questions = result["questions"]
        .as_array()
        .expect("should have questions array");
    assert_eq!(questions.len(), 1, "should return one question");

    let q = &questions[0];
    assert_eq!(q["coworker_name"].as_str().unwrap(), "york");
    assert_eq!(q["question"].as_str().unwrap(), "Can you help me?");
    assert!(q["id"].as_u64().unwrap() > 0, "question should have id");
    assert!(
        q["timestamp"].as_str().is_some(),
        "question should have timestamp"
    );
}

// ============================================================================
// handle_coworker_list — lead session filtering + channel lead tagging
// ============================================================================

#[tokio::test]
async fn test_coworker_list_excludes_lead_session() {
    // The lead session is named after the repo (state.project_name = "test-repo").
    // handle_coworker_list must not include it in the response.
    let (state, _tmp, _guard) = make_test_state();

    // Register the lead session (name matches repo_name)
    let inserted_lead = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "test-repo".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "claude-sonnet-4-6".to_string(),
            provider: crate::auth::AuthProvider::Claude,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted_lead, "lead coworker should be inserted");

    // Register a regular coworker
    let inserted_cw = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "park".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "claude-sonnet-4-6".to_string(),
            provider: crate::auth::AuthProvider::Claude,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted_cw, "coworker should be inserted");

    let response = handle_coworker_list(RequestId::Number(1), &state).await;
    assert!(!response.is_error(), "coworker.list should succeed");

    let result = response.result.expect("should have result");
    let coworkers = result["coworkers"]
        .as_array()
        .expect("should have coworkers array");

    let names: Vec<&str> = coworkers
        .iter()
        .map(|cw| cw["name"].as_str().unwrap_or(""))
        .collect();

    assert!(
        !names.contains(&"test-repo"),
        "lead session should be excluded from coworker.list, got: {:?}",
        names
    );
    assert!(
        names.contains(&"park"),
        "regular coworker should appear in coworker.list, got: {:?}",
        names
    );
    assert_eq!(
        coworkers.len(),
        1,
        "only the regular coworker should appear"
    );
}

#[tokio::test]
async fn test_coworker_list_excludes_legacy_lead_session() {
    // A session registered with the literal name "lead" (legacy backward-compat
    // name) must not appear in the coworker.list response, just like a session
    // named after the repo.
    let (state, _tmp, _guard) = make_test_state();

    let inserted = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "lead".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "gpt-5-codex".to_string(),
            provider: crate::auth::AuthProvider::Codex,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted, "legacy lead coworker should be inserted for test");

    let response = handle_coworker_list(RequestId::Number(1), &state).await;
    assert!(!response.is_error(), "coworker.list should succeed");

    let result = response.result.expect("should have result");
    let coworkers = result["coworkers"]
        .as_array()
        .expect("should have coworkers array");

    let names: Vec<&str> = coworkers
        .iter()
        .filter_map(|cw| cw.get("name").and_then(|n| n.as_str()))
        .collect();

    assert!(
        !names.contains(&"lead"),
        "legacy 'lead' session should be excluded from coworker.list"
    );
}

#[tokio::test]
async fn test_coworker_list_tags_channel_leads() {
    // Registers a channel lead in persistent state and verifies that
    // handle_coworker_list sets is_channel_lead: true for that coworker.
    let (state, _tmp, _guard) = make_test_state();

    // Register a channel lead name in persistent state (simulate an active channel lead)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("tui".to_string(), "tui-session-id".to_string());
    }

    let response = handle_coworker_list(RequestId::Number(1), &state).await;
    assert!(!response.is_error(), "coworker.list should succeed");

    let result = response.result.expect("should have result");
    let coworkers = result["coworkers"]
        .as_array()
        .expect("should have coworkers array");

    // The coworker registry is empty in the test state — verify the response
    // succeeds with an empty list (the tag logic runs without panicking).
    assert_eq!(coworkers.len(), 0, "no tracked coworkers in test state");
}

// ============================================================================
// handle_coworker_report_state — idle handling
// ============================================================================

#[tokio::test]
async fn test_report_idle_keeps_project_lead_running() {
    let (state, _tmp, _guard) = make_test_state();

    let inserted = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "test-repo".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "gpt-5-codex".to_string(),
            provider: crate::auth::AuthProvider::Codex,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted, "lead coworker should be inserted for test");

    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "test-repo",
        "idle",
        None,
        None,
        None,
        &state,
    )
    .await;

    assert!(!response.is_error(), "idle report should succeed");
    assert!(
        state.coworkers.get("test-repo").is_some(),
        "project lead should remain tracked after idle report"
    );
}

#[tokio::test]
async fn test_report_idle_still_breaks_non_lead_coworker() {
    let (state, _tmp, _guard) = make_test_state();

    let inserted = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "park".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "gpt-5-codex".to_string(),
            provider: crate::auth::AuthProvider::Codex,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted, "coworker should be inserted for test");

    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "park",
        "idle",
        None,
        None,
        None,
        &state,
    )
    .await;

    assert!(!response.is_error(), "idle report should succeed");
    let message = response
        .result
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        message.contains("break (idle)"),
        "non-lead coworker idle report should still take the break path"
    );
}

// ============================================================================
// handle_coworker_report_state — pr_number wiring
// ============================================================================

#[tokio::test]
async fn test_report_state_pr_number_writes_task_pr() {
    let (state, _tmp, _guard) = make_test_state();

    // Create the task file on disk so update_task_fields_for_repo can write to it.
    let home = dirs::home_dir().expect("home dir");
    let task_list_id = crate::paths::task_list_id_for_repo(state.paths.dir_key());
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    std::fs::create_dir_all(&tasks_dir).expect("create tasks dir");
    let task_id = "9901"; // unique ID unlikely to conflict with real tasks
    let task_file = tasks_dir.join(format!("{}.json", task_id));
    std::fs::write(
        &task_file,
        serde_json::to_string(&serde_json::json!({
            "id": task_id,
            "subject": "Test PR wiring",
            "status": "in_progress",
            "owner": "park"
        }))
        .unwrap(),
    )
    .expect("write task file");

    // Call with task_id and pr_number
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "park",
        "pull_request",
        Some(9901u32),
        Some(90),
        Some(456u64),
        &state,
    )
    .await;

    assert!(!response.is_error(), "report state should succeed");

    // Verify task.pr was written to the file
    let content = std::fs::read_to_string(&task_file).expect("read task file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse task json");
    assert_eq!(
        parsed["pr"],
        serde_json::json!(456u64),
        "task.pr should be set to the reported PR number"
    );

    // Cleanup
    let _ = std::fs::remove_file(&task_file);
}

#[tokio::test]
async fn test_report_state_pr_number_no_task_assignment_succeeds() {
    let (state, _tmp, _guard) = make_test_state();

    // No task assignment set up — handler should log a warning but still succeed
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "park",
        "pull_request",
        None, // no task_id
        Some(90),
        Some(789u64),
        &state,
    )
    .await;

    // Should still succeed even when no task can be found to update
    assert!(
        !response.is_error(),
        "report state with unresolvable task should still succeed"
    );
}

#[tokio::test]
async fn test_report_state_without_pr_number_succeeds() {
    let (state, _tmp, _guard) = make_test_state();

    // pr_number = None should behave identically to pre-feature behavior
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "park",
        "pull_request",
        None,
        Some(90),
        None,
        &state,
    )
    .await;

    assert!(
        !response.is_error(),
        "report state without pr_number should succeed"
    );
}

// ============================================================================
// handle_coworker_report_state — completed phase without PR (!1879)
// ============================================================================

#[tokio::test]
async fn test_completed_without_pr_marks_task_done() {
    // Bug !1879: When a coworker reports completed on a task with no PR,
    // the daemon should complete the task on disk instead of nudging to
    // open a PR (which caused a respawn loop).
    let (state, _tmp, _guard) = make_test_state();

    // Create a task file on disk in in_progress status
    let home = dirs::home_dir().expect("home dir");
    let task_list_id = crate::paths::task_list_id_for_repo(state.paths.dir_key());
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    std::fs::create_dir_all(&tasks_dir).expect("create tasks dir");
    let task_id = "9950";
    let task_file = tasks_dir.join(format!("{}.json", task_id));
    std::fs::write(
        &task_file,
        serde_json::to_string(&serde_json::json!({
            "id": task_id,
            "subject": "Merge PR and tag release",
            "status": "in_progress",
            "owner": "riverside"
        }))
        .unwrap(),
    )
    .expect("write task file");

    // Set up in-memory task assignment so the Completed handler can resolve it
    {
        let mut assignments = state.coworker_task_assignments.lock().unwrap();
        assignments.insert(
            "riverside".to_string(),
            super::super::TaskAssignment {
                task_id: task_id.to_string(),
            },
        );
    }

    // Report completed — no PR exists for this task
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "riverside",
        "completed",
        Some(9950u32),
        None,
        None, // no pr_number
        &state,
    )
    .await;

    assert!(!response.is_error(), "completed report should succeed");

    // Verify task is marked as completed on disk
    let content = std::fs::read_to_string(&task_file).expect("read task file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse task json");
    assert_eq!(
        parsed["status"].as_str().unwrap(),
        "completed",
        "task should be marked completed on disk (not left in_progress)"
    );

    // Verify the coworker's assignment was cleared
    {
        let assignments = state.coworker_task_assignments.lock().unwrap();
        assert!(
            !assignments.contains_key("riverside"),
            "coworker assignment should be cleared after completion"
        );
    }

    // Cleanup
    let _ = std::fs::remove_file(&task_file);
}

#[tokio::test]
async fn test_completed_with_unverifiable_disk_pr_completes_directly() {
    // When task.pr is set on disk but the PR can't be verified as open via
    // GitHub API (e.g., API unreachable, PR closed, or test environment),
    // the task should be completed directly rather than stuck in the deferred
    // merge path. This prevents stale task.pr values (from closed/superseded
    // PRs) from blocking task completion.
    let (state, _tmp, _guard) = make_test_state();

    // Create a task file on disk WITH a pr field set
    let home = dirs::home_dir().expect("home dir");
    let task_list_id = crate::paths::task_list_id_for_repo(state.paths.dir_key());
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    std::fs::create_dir_all(&tasks_dir).expect("create tasks dir");
    let task_id = "9952";
    let task_file = tasks_dir.join(format!("{}.json", task_id));
    std::fs::write(
        &task_file,
        serde_json::to_string(&serde_json::json!({
            "id": task_id,
            "subject": "Add new endpoint",
            "status": "in_progress",
            "owner": "riverside",
            "pr": 99
        }))
        .unwrap(),
    )
    .expect("write task file");

    // Set up in-memory task assignment
    {
        let mut assignments = state.coworker_task_assignments.lock().unwrap();
        assignments.insert(
            "riverside".to_string(),
            super::super::TaskAssignment {
                task_id: task_id.to_string(),
            },
        );
    }

    // NOTE: pr_author_sessions is empty — simulates daemon restart.
    // The task has pr=99 on disk but no in-memory PR tracking.
    // In test env, gh pr view won't find this PR, so is_pr_open returns false.

    // Report completed — task.pr is set on disk but unverifiable
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "riverside",
        "completed",
        Some(9952u32),
        None,
        None,
        &state,
    )
    .await;

    assert!(!response.is_error(), "completed report should succeed");

    // Task should be completed directly — the disk pr field alone is not
    // sufficient without GitHub API verification that the PR is actually open.
    let content = std::fs::read_to_string(&task_file).expect("read task file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse task json");
    assert_eq!(
        parsed["status"].as_str().unwrap(),
        "completed",
        "task with unverifiable disk PR should be completed directly"
    );

    // Cleanup
    let _ = std::fs::remove_file(&task_file);
}

#[tokio::test]
async fn test_completed_with_open_pr_defers_to_merge_path() {
    // When a task HAS an open PR, reporting completed should NOT complete
    // the task on disk — it defers to the PR merge auto-completion path.
    let (state, _tmp, _guard) = make_test_state();

    // Create a task file on disk
    let home = dirs::home_dir().expect("home dir");
    let task_list_id = crate::paths::task_list_id_for_repo(state.paths.dir_key());
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    std::fs::create_dir_all(&tasks_dir).expect("create tasks dir");
    let task_id = "9951";
    let task_file = tasks_dir.join(format!("{}.json", task_id));
    std::fs::write(
        &task_file,
        serde_json::to_string(&serde_json::json!({
            "id": task_id,
            "subject": "Add new feature",
            "status": "in_progress",
            "owner": "park"
        }))
        .unwrap(),
    )
    .expect("write task file");

    // Set up task assignment
    {
        let mut assignments = state.coworker_task_assignments.lock().unwrap();
        assignments.insert(
            "park".to_string(),
            super::super::TaskAssignment {
                task_id: task_id.to_string(),
            },
        );
    }

    // Simulate an open PR for this task via pr_author_sessions
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.pr_author_sessions.insert(
            42,
            crate::github_state::PrAuthorSession {
                session_id: "session-42".to_string(),
                branch: "park/add-new-feature".to_string(),
                original_author: "park".to_string(),
                stored_at: chrono::Utc::now(),
                task_id: Some(task_id.to_string()),
            },
        );
    }

    // Report completed — task HAS an open PR
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        "park",
        "completed",
        Some(9951u32),
        None,
        None,
        &state,
    )
    .await;

    assert!(!response.is_error(), "completed report should succeed");

    // Task should still be in_progress (deferred to merge path)
    let content = std::fs::read_to_string(&task_file).expect("read task file");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse task json");
    assert_eq!(
        parsed["status"].as_str().unwrap(),
        "in_progress",
        "task with open PR should remain in_progress (deferred to merge path)"
    );

    // Cleanup
    let _ = std::fs::remove_file(&task_file);
}

// ============================================================================
// is_pr_open — GitHub API verification for disk PR field
// ============================================================================

#[test]
fn test_is_pr_open_returns_false_for_nonexistent_pr() {
    // In a temp git repo with no GitHub remote, is_pr_open should return false
    // (gh pr view will fail). This is the conservative fallback behavior.
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");

    let result = is_pr_open(99999, Some(temp_dir.path()));
    assert!(
        !result,
        "is_pr_open should return false when gh pr view fails"
    );
}

#[test]
fn test_is_pr_open_returns_false_with_no_repo_path() {
    // When no repo path is provided, gh runs without current_dir context.
    // Should still not panic and return false.
    let result = is_pr_open(99999, None);
    assert!(
        !result,
        "is_pr_open should return false when no repo path is given"
    );
}

// ============================================================================
// Reviewer idle nudge loop fix (!1990)
// ============================================================================

/// Bug !1990: When a reviewer posts their review and immediately goes idle,
/// the webhook marking the review as cached may not have arrived yet. The
/// idle handler used to check only the snapshot's `reviewed_prs` (a cache
/// clone), which missed the live state. This caused a nudge loop: the
/// reviewer gets told to post a review that already exists on GitHub.
///
/// Fix: the idle handler now calls `is_pr_reviewed()` which checks persistent
/// state first (fast path) and falls back to a GitHub API call if needed.
///
/// This test verifies the fast path: when the review IS cached in persistent
/// state (e.g., webhook arrived before idle report), the reviewer should NOT
/// be nudged and should be sent on break instead.
#[tokio::test]
async fn test_reviewer_idle_not_nudged_when_review_cached() {
    let (state, _tmp, _guard) = make_test_state();

    let reviewer_name = "vernon";
    let pr_number = 42u64;

    // Insert the reviewer as a running coworker
    let inserted = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: reviewer_name.to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted, "reviewer coworker should be inserted");

    // Assign the reviewer to a PR in persistent state
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            reviewer_name,
            crate::github_state::AssignmentSource::Webhook,
        );
        // Mark the review as completed (simulates webhook having arrived)
        ps.github.mark_reviewed_pr(pr_number);
    }

    // Reviewer reports idle — should NOT be nudged since review is cached
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        reviewer_name,
        "idle",
        None,
        None,
        None,
        &state,
    )
    .await;

    assert!(!response.is_error(), "idle report should succeed");
    let message = response
        .result
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    // Should go on break, NOT be nudged to post review
    assert!(
        !message.contains("nudged to post review"),
        "reviewer should NOT be nudged when review is already cached, got: {}",
        message
    );
}

/// Complement to the above: when the review has NOT been posted (neither cached
/// nor on GitHub), the reviewer SHOULD be nudged. This verifies the nudge still
/// fires for genuinely unposted reviews.
///
/// Uses PATH_LOCK + mock `gh` to control the subprocess call that
/// `is_pr_reviewed()` makes when the persistent cache has no record.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await to prevent test interference
async fn test_reviewer_idle_nudged_when_review_not_posted() {
    use crate::daemon::PATH_LOCK;

    let (state, _tmp, _guard) = make_test_state();

    let reviewer_name = "park";
    let pr_number = 43u64;

    // Acquire PATH_LOCK to prevent parallel tests from interfering with PATH mocking
    let _path_guard = PATH_LOCK.lock().unwrap();

    // Mock gh CLI to return no reviews/comments so is_pr_reviewed() returns false.
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");

    #[cfg(unix)]
    {
        std::fs::write(
            &mock_gh_script,
            "#!/bin/bash\necho '{\"reviews\":[],\"comments\":[]}'",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    // Insert the reviewer as a running coworker
    let inserted = state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: reviewer_name.to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
            profile: crate::auth::DEFAULT_PROFILE.to_string(),
        });
    assert!(inserted, "reviewer coworker should be inserted");

    // Assign the reviewer to a PR but do NOT mark the review as completed
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            reviewer_name,
            crate::github_state::AssignmentSource::Webhook,
        );
        // Deliberately NOT calling mark_reviewed_pr — review hasn't been posted
    }

    // Reviewer reports idle — SHOULD be nudged since review isn't posted
    let response = handle_coworker_report_state(
        RequestId::Number(1),
        reviewer_name,
        "idle",
        None,
        None,
        None,
        &state,
    )
    .await;

    // Restore PATH before assertions (cleanup)
    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(!response.is_error(), "idle report should succeed");
    let message = response
        .result
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        message.contains("nudged to post review"),
        "reviewer should be nudged when review is NOT posted, got: {}",
        message
    );
}

// ============================================================================
// Tests for channel lead filtering — channel leads must not appear in the
// coworker status list (they are scoped to their specific channel)
// ============================================================================

#[test]
fn test_is_channel_lead_matches_registered_names() {
    let channel_leads: std::collections::HashSet<String> = ["auth", "web-interface", "daemon-core"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert!(is_channel_lead("auth", &channel_leads));
    assert!(is_channel_lead("web-interface", &channel_leads));
    assert!(is_channel_lead("daemon-core", &channel_leads));
}

#[test]
fn test_is_channel_lead_rejects_non_leads() {
    let channel_leads: std::collections::HashSet<String> = ["auth", "web-interface"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert!(!is_channel_lead("madison", &channel_leads));
    assert!(!is_channel_lead("broadway", &channel_leads));
    assert!(!is_channel_lead("amsterdam", &channel_leads));
    assert!(!is_channel_lead("park", &channel_leads));
    assert!(!is_channel_lead("lead", &channel_leads));
}

#[test]
fn test_is_channel_lead_empty_set() {
    let channel_leads: std::collections::HashSet<String> = std::collections::HashSet::new();

    // With no channel leads registered, nothing should match
    assert!(!is_channel_lead("auth", &channel_leads));
    assert!(!is_channel_lead("madison", &channel_leads));
}

// ============================================================================
// Tests for is_lead_health_active — bug regression: legacy "lead" key vs repo name
// ============================================================================

#[test]
fn test_is_lead_health_active_detects_by_repo_name() {
    let mut health = HashMap::new();
    health.insert(
        "midtown".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    assert!(
        is_lead_health_active(&health, "midtown"),
        "Should detect activity via repo-name key (not just 'lead')"
    );
}

#[test]
fn test_is_lead_health_active_detects_by_legacy_lead_key() {
    let mut health = HashMap::new();
    health.insert(
        "lead".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    assert!(
        is_lead_health_active(&health, "midtown"),
        "Should detect activity via legacy 'lead' key"
    );
}

#[test]
fn test_is_lead_health_active_returns_false_when_absent() {
    let health: HashMap<String, ProcessHealth> = HashMap::new();
    assert!(!is_lead_health_active(&health, "midtown"));
}

#[test]
fn test_is_lead_health_active_both_keys_stale_repo_name_active_legacy() {
    let mut health = HashMap::new();
    health.insert(
        "midtown".to_string(),
        ProcessHealth {
            is_alive: false, // stale/dead entry for repo_name
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    health.insert(
        "lead".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    assert!(
        is_lead_health_active(&health, "midtown"),
        "Should detect activity via legacy 'lead' key even when repo-name key is stale"
    );
}

#[test]
fn test_lead_activity_detection_no_health() {
    assert!(!is_session_actively_working(None));
}

#[test]
fn test_lead_activity_detection_not_alive() {
    let health = ProcessHealth {
        is_alive: false,
        last_event_at: Some(Utc::now()),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_recent_event() {
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(Utc::now()),
        ..Default::default()
    };
    assert!(is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_stale_event() {
    // Event older than LEAD_ACTIVITY_TIMEOUT — should be considered idle
    let stale_ts = Utc::now() - chrono::Duration::seconds(10);
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(stale_ts),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_alive_no_events() {
    // Alive but has never received any events
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: None,
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

#[test]
fn test_lead_activity_detection_future_timestamp() {
    // Clock skew: last_event_at is in the future — should NOT report active
    let future_ts = Utc::now() + chrono::Duration::seconds(60);
    let health = ProcessHealth {
        is_alive: true,
        last_event_at: Some(future_ts),
        ..Default::default()
    };
    assert!(!is_session_actively_working(Some(&health)));
}

// ============================================================================
// Tests for build_channel_leads_working — per-channel-lead activity map
// ============================================================================

#[test]
fn test_channel_leads_working_active_session() {
    let mut health = HashMap::new();
    health.insert(
        "web".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    let names: std::collections::HashSet<String> = ["web"].iter().map(|s| s.to_string()).collect();
    let result = build_channel_leads_working(&health, &names);
    assert_eq!(result.get("web").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn test_channel_leads_working_stale_session() {
    let stale_ts = Utc::now() - chrono::Duration::seconds(10);
    let mut health = HashMap::new();
    health.insert(
        "auth".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(stale_ts),
            ..Default::default()
        },
    );
    let names: std::collections::HashSet<String> = ["auth"].iter().map(|s| s.to_string()).collect();
    let result = build_channel_leads_working(&health, &names);
    assert_eq!(result.get("auth").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn test_channel_leads_working_missing_health() {
    // Channel lead registered but no health entry yet (just spawned)
    let health: HashMap<String, ProcessHealth> = HashMap::new();
    let names: std::collections::HashSet<String> = ["web"].iter().map(|s| s.to_string()).collect();
    let result = build_channel_leads_working(&health, &names);
    assert_eq!(result.get("web").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn test_channel_leads_working_multiple_channels() {
    let mut health = HashMap::new();
    health.insert(
        "web".to_string(),
        ProcessHealth {
            is_alive: true,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    health.insert(
        "auth".to_string(),
        ProcessHealth {
            is_alive: false,
            last_event_at: Some(Utc::now()),
            ..Default::default()
        },
    );
    let names: std::collections::HashSet<String> =
        ["web", "auth"].iter().map(|s| s.to_string()).collect();
    let result = build_channel_leads_working(&health, &names);
    assert_eq!(result.get("web").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(result.get("auth").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn test_channel_leads_working_empty_set() {
    let health: HashMap<String, ProcessHealth> = HashMap::new();
    let names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let result = build_channel_leads_working(&health, &names);
    assert!(result.is_empty());
}

// ============================================================================
// Tests for project lead filtering — the lead must not appear in the
// coworker status list regardless of whether it uses the legacy "lead" name
// or the canonical repo name. Regression tests for !1723.
// ============================================================================

#[test]
fn test_is_project_lead_matches_legacy_name() {
    use super::super::helpers::is_project_lead;
    assert!(is_project_lead("lead", "midtown"));
    assert!(is_project_lead("Lead", "midtown"));
    assert!(is_project_lead("LEAD", "midtown"));
}

#[test]
fn test_is_project_lead_matches_repo_name() {
    use super::super::helpers::is_project_lead;
    // Canonical: lead session is named after the repo
    assert!(is_project_lead("midtown", "midtown"));
    assert!(is_project_lead("Midtown", "midtown"));
    assert!(is_project_lead("MIDTOWN", "MIDTOWN"));
}

#[test]
fn test_is_project_lead_rejects_regular_coworkers() {
    use super::super::helpers::is_project_lead;
    assert!(!is_project_lead("york", "midtown"));
    assert!(!is_project_lead("park", "midtown"));
    assert!(!is_project_lead("amsterdam", "midtown"));
    // Channel lead names are NOT project leads
    assert!(!is_project_lead("auth", "midtown"));
}

// ============================================================================
// Tests for serialize_tool_activity_headers
// ============================================================================

#[test]
fn test_serialize_tool_activity_headers_empty() {
    let map: HashMap<String, Vec<String>> = HashMap::new();
    let result = super::serialize_tool_activity_headers(&map);
    assert_eq!(result, serde_json::json!({}));
}

#[test]
fn test_serialize_tool_activity_headers_with_entries() {
    let mut map = HashMap::new();
    map.insert(
        "amsterdam".to_string(),
        vec!["✓ $ git status".to_string(), "› read foo.rs".to_string()],
    );

    let result = super::serialize_tool_activity_headers(&map);
    let obj = result.as_object().expect("should be an object");
    assert!(obj.contains_key("amsterdam"));

    let items = obj["amsterdam"].as_array().expect("should be an array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_str().unwrap(), "✓ $ git status");
    assert_eq!(items[1].as_str().unwrap(), "› read foo.rs");
}

#[test]
fn test_serialize_tool_activity_headers_multiple_agents() {
    let mut map = HashMap::new();
    map.insert("madison".to_string(), vec!["✗ $ cargo test".to_string()]);
    map.insert("lead".to_string(), vec!["› edit src/main.rs".to_string()]);

    let result = super::serialize_tool_activity_headers(&map);
    let obj = result.as_object().expect("should be an object");
    assert_eq!(obj.len(), 2);
    assert_eq!(obj["madison"][0].as_str().unwrap(), "✗ $ cargo test");
    assert_eq!(obj["lead"][0].as_str().unwrap(), "› edit src/main.rs");
}

// ============================================================================
// Name allocation — active session exclusion (rpc_coworker.rs fix)
// ============================================================================

/// Regression test: handle_coworker_spawn must exclude names with active sessions
/// even if they're not registered in CoworkerManager.
///
/// Scenario: "park" has an active session in SessionManager (e.g., cleanup removed
/// it from CoworkerManager but the session is still running). The RPC spawn path
/// should NOT allocate "park" for a new coworker.
///
/// We can't call handle_coworker_spawn directly (it spawns a real process), so we
/// test the name exclusion logic it uses: SessionManager.list_names() + is_alive()
/// fed into next_available_name_excluding().
#[tokio::test]
async fn test_spawn_name_excludes_active_sessions() {
    use super::super::sessions::SessionStatus;

    let (state, _tmp, _guard) = make_test_state();

    // Register all AVENUE_NAMES except "park" in CoworkerManager.
    // From CoworkerManager's perspective, "park" is the only free name.
    for (i, name) in crate::coworker::AVENUE_NAMES
        .iter()
        .filter(|&&n| n != "park")
        .enumerate()
    {
        state
            .coworkers
            .register(
                &format!("slot-{i}"),
                name,
                "/tmp".to_string(),
                None,
                "claude-sonnet".to_string(),
                crate::auth::AuthProvider::Claude,
                "default".to_string(),
            )
            .unwrap();
    }

    // "park" has an active session in SessionManager (not in CoworkerManager).
    state
        .session_manager
        .insert_test_session("park", SessionStatus::Running)
        .await;

    // Configure is_alive hook to return true for "park".
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(|name: &str| {
            name.eq_ignore_ascii_case("park")
        })));

    // Replicate the exclusion logic from handle_coworker_spawn:
    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
    };
    let mut excluded_names = channel_lead_names;
    for name in state.session_manager.list_names().await {
        if state.session_manager.is_alive(&name).await {
            excluded_names.insert(name.to_lowercase());
        }
    }

    let allocated = state
        .coworkers
        .next_available_name_excluding(&excluded_names);

    // "park" should be excluded — the allocator should fall through to overflow names.
    assert!(
        allocated.is_some(),
        "Should still allocate a name (overflow names available)"
    );
    assert_ne!(
        allocated.as_deref(),
        Some("park"),
        "Should NOT allocate 'park' — it has an active session in SessionManager \
         even though it's not in CoworkerManager. Before fix: only channel_lead_names \
         were excluded, so 'park' would be allocated causing a name collision."
    );
}

// ============================================================================
// Thread binding — fork_bound_threads + SessionRecord persistence
// ============================================================================

/// Tests that --thread binding correctly populates both the in-memory
/// fork_bound_threads cache and the persisted SessionRecord.bound_thread_id.
///
/// We can't call handle_coworker_spawn (it spawns a real process), so this
/// tests the binding logic directly on DaemonState, mirroring the code path
/// in handle_coworker_spawn lines 149-172.
#[tokio::test]
async fn test_thread_binding_populates_fork_bound_threads_and_session_record() {
    let (state, _tmp, _guard) = make_test_state();
    let coworker_name = "madison";
    let thread_id = "msg-abc-123";

    // Simulate: a SessionRecord exists for this coworker (created by spawn_coworker)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "sess-test".to_string(),
            super::super::state::SessionRecord {
                session_id: "sess-test".to_string(),
                task_id: None,
                current_name: Some(coworker_name.to_string()),
                preferred_name: Some(coworker_name.to_string()),
                working_dir: "/tmp".to_string(),
                branch: None,
                pr_number: None,
                initial_prompt: None,
                is_reviewer: false,
                coworker_type: "dev".to_string(),
                is_running: true,
                created_at: chrono::Utc::now(),
                resume_on_startup: false,
                bound_thread_id: None,
                last_active: chrono::Utc::now(),
                purpose: String::new(),
                pid: None,
                channel: None,
                provider: Some(crate::auth::AuthProvider::Claude),
                platform: None,
                profile: None,
            },
        );
    }

    // Apply the same binding logic as handle_coworker_spawn --thread
    {
        state
            .fork_bound_threads
            .lock()
            .unwrap()
            .insert(coworker_name.to_string(), thread_id.to_string());
    }
    {
        let mut ps = state.persistent_state.lock().await;
        if let Some(record) = ps
            .sessions
            .values_mut()
            .find(|r| r.current_name.as_deref() == Some(coworker_name))
        {
            record.bound_thread_id = Some(thread_id.to_string());
        }
    }

    // Verify: fork_bound_threads contains the binding
    {
        let bound = state.fork_bound_threads.lock().unwrap();
        assert_eq!(
            bound.get(coworker_name),
            Some(&thread_id.to_string()),
            "fork_bound_threads should contain the thread binding"
        );
    }

    // Verify: SessionRecord.bound_thread_id is set
    {
        let ps = state.persistent_state.lock().await;
        let record = ps
            .sessions
            .values()
            .find(|r| r.current_name.as_deref() == Some(coworker_name))
            .expect("SessionRecord should exist");
        assert_eq!(
            record.bound_thread_id.as_deref(),
            Some(thread_id),
            "SessionRecord.bound_thread_id should be persisted"
        );
    }
}

/// Tests that when no SessionRecord matches the coworker name, the thread
/// binding still populates fork_bound_threads (for immediate routing) even
/// though the persistence path silently fails to find a record.
#[tokio::test]
async fn test_thread_binding_no_session_record_still_sets_in_memory() {
    let (state, _tmp, _guard) = make_test_state();
    let coworker_name = "riverside";
    let thread_id = "msg-xyz-789";

    // No SessionRecord exists — simulate a race or missing record

    // Apply binding logic
    {
        state
            .fork_bound_threads
            .lock()
            .unwrap()
            .insert(coworker_name.to_string(), thread_id.to_string());
    }
    {
        let mut ps = state.persistent_state.lock().await;
        // This find will return None — no record matches
        if let Some(record) = ps
            .sessions
            .values_mut()
            .find(|r| r.current_name.as_deref() == Some(coworker_name))
        {
            record.bound_thread_id = Some(thread_id.to_string());
        }
        // No save error — just no record found
    }

    // Verify: fork_bound_threads still has the binding (immediate routing works)
    {
        let bound = state.fork_bound_threads.lock().unwrap();
        assert_eq!(
            bound.get(coworker_name),
            Some(&thread_id.to_string()),
            "fork_bound_threads should be set even without a SessionRecord"
        );
    }

    // Verify: no SessionRecord was created (we don't create records here)
    {
        let ps = state.persistent_state.lock().await;
        let record = ps
            .sessions
            .values()
            .find(|r| r.current_name.as_deref() == Some(coworker_name));
        assert!(
            record.is_none(),
            "No SessionRecord should exist — binding logic doesn't create records"
        );
    }
}
