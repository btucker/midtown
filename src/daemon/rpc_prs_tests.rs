use super::*;

// ============================================================================
// Helper: create a minimal DaemonState for merge-gate tests
// ============================================================================

fn make_merge_test_state() -> (
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
        "/tmp/test-merge-gate.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir.clone()],
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
// handle_pr_merge gate tests
// ============================================================================

/// When a reviewer is actively assigned to a PR, `handle_pr_merge` must reject
/// the merge request immediately — even before checking other gates.
///
/// This is the hard gate that prevents the PR #1624 incident: a coworker merged
/// while the reviewer was still actively working on the review.
#[tokio::test]
async fn test_merge_blocked_while_reviewer_actively_assigned() {
    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number: u64 = 1624;

    // Assign a reviewer to the PR (simulates daemon spawning a reviewer coworker)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            "park",
            crate::github_state::AssignmentSource::Webhook,
        );
    }

    // Attempt to merge — should be rejected because reviewer is active
    let response = handle_pr_merge(crate::rpc::RequestId::Number(1), pr_number, &state).await;

    assert!(
        response.error.is_some(),
        "merge should be rejected when reviewer is actively assigned"
    );
    let err_msg = response.error.unwrap().message;
    assert!(
        err_msg.contains("reviewer") || err_msg.contains("review in progress"),
        "error should mention active reviewer, got: {}",
        err_msg
    );
}

/// When no reviewer is assigned, the reviewer-active gate should not block.
/// (Other gates may still block — this test only verifies the reviewer gate
/// doesn't produce a false positive.)
#[tokio::test]
async fn test_merge_not_blocked_when_no_reviewer_assigned() {
    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number: u64 = 999;

    // No reviewer assigned — the reviewer-active gate should pass.
    // The merge will still fail on other gates (review not completed, etc.)
    // but the error should NOT mention an active reviewer.
    let response = handle_pr_merge(crate::rpc::RequestId::Number(2), pr_number, &state).await;

    // The response will be an error (no review exists, no PR data, etc.)
    // but it should not mention "review in progress" or active reviewer
    if let Some(err) = &response.error {
        assert!(
            !err.message.contains("review in progress"),
            "should not block on reviewer gate when no reviewer is assigned, got: {}",
            err.message
        );
    }
}

/// After a reviewer assignment is removed (review completed), merge should
/// not be blocked by the reviewer-active gate.
#[tokio::test]
async fn test_merge_not_blocked_after_reviewer_assignment_removed() {
    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number: u64 = 42;

    // Assign then remove reviewer (simulates completed review flow)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            "park",
            crate::github_state::AssignmentSource::Webhook,
        );
        ps.github.remove_assignment(pr_number);
    }

    let response = handle_pr_merge(crate::rpc::RequestId::Number(3), pr_number, &state).await;

    // May fail on other gates, but should NOT mention active reviewer
    if let Some(err) = &response.error {
        assert!(
            !err.message.contains("review in progress"),
            "should not block on reviewer gate after assignment removed, got: {}",
            err.message
        );
    }
}

#[test]
fn test_extract_coworker_from_pr_body() {
    assert_eq!(
        extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
        Some("york".to_string())
    );
    assert_eq!(
        extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
        Some("park".to_string())
    );
    assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
    assert_eq!(extract_coworker_from_pr_body(""), None);
}

#[test]
fn test_extract_reviewer_from_pr_comments() {
    let comments = vec![serde_json::json!({
        "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
        "createdAt": "2026-01-29T10:00:00Z"
    })];
    let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
    assert_eq!(reviewer, Some("lexington".to_string()));
    assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

    let comments = vec![serde_json::json!({
        "body": "## Code Review by vernon\nLGTM",
        "createdAt": "2026-01-29T11:00:00Z"
    })];
    let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
    assert_eq!(reviewer, Some("vernon".to_string()));

    let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
    assert_eq!(reviewer, None);
}

#[test]
fn test_pr_ci_status() {
    assert_eq!(pr_ci_status(&[]), "unknown");
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})]),
        "passed"
    );
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})]),
        "failed"
    );
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
        "running"
    );
}

#[test]
fn test_prs_cache_hit_and_miss() {
    let cache = PrsCache::new();
    let key: u64 = 42;
    let value = serde_json::json!({"prs": []});

    assert!(cache.get(key).is_none(), "empty cache should miss");
    cache.set(value.clone(), key);
    assert_eq!(cache.get(key), Some(value), "should hit after set");
    assert!(cache.get(key + 1).is_none(), "different key should miss");
}
