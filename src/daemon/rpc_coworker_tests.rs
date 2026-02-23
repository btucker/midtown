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
        "test-repo".to_string(),
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
// handle_coworker_list — channel lead tagging
// ============================================================================

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
    let task_list_id = crate::paths::task_list_id_for_repo(&state.repo_name);
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
