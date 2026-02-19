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
