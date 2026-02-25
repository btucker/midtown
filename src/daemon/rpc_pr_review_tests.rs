//! Tests for the `pr.review` RPC handler.

use std::process::Command;

use super::super::DaemonState;
use super::handle_pr_review;
use crate::rpc::RequestId;

fn make_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let temp_dir = tempfile::tempdir().expect("temp dir");
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
        "/tmp/test.sock".into(),
        cm,
        repo_name.to_string(),
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

/// When a PR already has a reviewer assignment, the handler returns an
/// informational success message rather than attempting another spawn.
#[tokio::test]
async fn test_pr_review_already_assigned_returns_message() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    // Pre-assign a reviewer so the handler can detect it without a GH API call.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            42,
            "lexington",
            crate::github_state::AssignmentSource::Manual,
        );
    }

    let response = handle_pr_review(RequestId::Number(1), 42, &state).await;

    // Should be a success with an informational message (not an error).
    let result = response.result.expect("should return success result");
    let message = result["message"].as_str().expect("message field");
    assert!(
        message.contains("already assigned"),
        "expected 'already assigned' in: {message}"
    );
    assert!(
        message.contains("lexington"),
        "expected reviewer name in: {message}"
    );
    assert!(message.contains("42"), "expected PR number in: {message}");
}

/// When the PR cannot be fetched (no real GitHub connection in tests),
/// the handler should return an RPC error rather than panicking.
#[tokio::test]
async fn test_pr_review_gh_failure_returns_error() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    // No assignment set — handler will proceed to fetch the PR via `gh`,
    // which fails in the test environment (no real GitHub connection).
    let response = handle_pr_review(RequestId::Number(1), 9999, &state).await;

    // Should be an RPC error.
    assert!(
        response.error.is_some(),
        "expected error when gh pr view fails"
    );
}
