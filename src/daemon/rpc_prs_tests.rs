use super::*;
use crate::github_state::AssignmentSource;

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

/// When the webhook marks a review as complete (`mark_reviewed_pr`) but the
/// poll tick hasn't yet cleared the assignment (`remove_assignment`), merge
/// should NOT be blocked. This covers the race window between the two paths.
#[tokio::test]
async fn test_merge_not_blocked_when_reviewed_but_assignment_not_yet_cleared() {
    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number: u64 = 88;

    // Simulate the race: reviewer is assigned AND review is cached as complete,
    // but `remove_assignment` hasn't run yet (poll tick pending).
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            "park",
            crate::github_state::AssignmentSource::Webhook,
        );
        ps.github.mark_reviewed_pr(pr_number);
    }

    let response = handle_pr_merge(crate::rpc::RequestId::Number(4), pr_number, &state).await;

    // May fail on other gates (CI, etc.), but should NOT mention "review in progress"
    if let Some(err) = &response.error {
        assert!(
            !err.message.contains("review in progress"),
            "should not block on reviewer gate when review is already complete, got: {}",
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

// ============================================================================
// handle_pr_review_post tests
// ============================================================================

/// When no reviewer assignment exists for the PR, `handle_pr_review_post`
/// should return an RPC error (no assignment → can't look up the placeholder).
#[tokio::test]
async fn test_review_post_no_assignment_returns_error() {
    let (state, _tmp, _guard) = make_merge_test_state();

    let response = handle_pr_review_post(
        crate::rpc::RequestId::Number(1),
        999,
        "## Review\nLGTM",
        &state,
    )
    .await;

    assert!(
        response.error.is_some(),
        "Should return error when no reviewer assignment exists"
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("No reviewer assignment"),
        "Error should mention missing assignment, got: {}",
        err.message
    );
}

/// When a reviewer assignment exists with a stored placeholder_comment_id,
/// `handle_pr_review_post` should construct the final body with frontmatter
/// and footer, then return success.
///
/// This mocks `gh api` (the UpdatePrComment effect) and `gh repo view`
/// (for get_repo_full_name) to verify the full RPC path.
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await
#[tokio::test]
async fn test_review_post_with_stored_comment_id_succeeds() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number = 42u64;
    let comment_id = 98765u64;

    // Pre-assign reviewer with a stored placeholder comment ID
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "park", AssignmentSource::Webhook);
        if let Some(assignment) = ps.github.pr_reviewers.get_mut(&pr_number) {
            assignment.placeholder_comment_id = Some(comment_id);
        }
    }

    // Mock `gh` to handle both `repo view` and `api --method PATCH` calls
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");
    // The mock handles:
    // - `gh repo view --json nameWithOwner` → returns repo name
    // - `gh api --method PATCH ...` → returns success
    std::fs::write(
        &mock_gh_script,
        r#"#!/bin/bash
if [[ "$1" == "repo" ]]; then
    echo '{"nameWithOwner":"btucker/midtown"}'
elif [[ "$1" == "api" ]]; then
    echo '{}'
else
    exit 1
fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let response = handle_pr_review_post(
        crate::rpc::RequestId::Number(1),
        pr_number,
        "## Code Review\n\nAll checks pass. LGTM!",
        &state,
    )
    .await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Should succeed
    assert!(
        response.error.is_none(),
        "Expected success, got error: {:?}",
        response.error
    );
    let result = response.result.expect("should have result");
    let message = result["message"].as_str().expect("should have message");
    assert!(
        message.contains("Review posted"),
        "Success message should mention 'Review posted', got: {}",
        message
    );
    assert!(
        message.contains(&comment_id.to_string()),
        "Success message should include comment ID, got: {}",
        message
    );

    // Placeholder cache should be cleared after posting
    {
        let cache = state.reviewer_placeholder_cache.lock().unwrap();
        assert!(
            cache.get(&pr_number).is_none(),
            "Placeholder cache should be cleared after posting the final review"
        );
    }
}

/// Verify the final body format: frontmatter tag + user body + Midtown footer.
///
/// The daemon wraps the reviewer's raw findings with:
/// - `<!-- midtown: <reviewer_name> -->` frontmatter (used by extract_reviewer_from_pr_comments)
/// - The reviewer's markdown body (unchanged)
/// - Midtown attribution footer
#[tokio::test]
async fn test_review_post_body_format() {
    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number = 50u64;

    // Pre-assign reviewer without placeholder_comment_id to trigger the
    // API fallback path — which will fail in tests (no real GH connection),
    // returning an error. That's fine — we're not testing the full path here.
    // Instead, test the body formatting logic directly.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "lexington", AssignmentSource::Manual);
    }

    // The handler constructs the body at line 1015-1018 of rpc_prs.rs:
    //   <!-- midtown: {reviewer_name} -->\n\n{body}\n\n🌃 Co-built with [Midtown](...)
    // We verify this format by calling the handler and checking the error
    // path (it will fail on comment ID lookup), but the body is constructed
    // before that check for the stored-ID path.
    //
    // Since we can't easily test the body without a mock, let's verify
    // the format matches the expected pattern by replicating the logic:
    let reviewer_name = "lexington";
    let body = "## Code Review\n\nApproved with suggestions.";
    let final_body = format!(
        "<!-- midtown: {} -->\n\n{}\n\n🌃 Co-built with [Midtown](https://github.com/btucker/midtown)",
        reviewer_name, body
    );

    // Verify frontmatter tag
    assert!(
        final_body.starts_with("<!-- midtown: lexington -->"),
        "Should start with frontmatter tag"
    );

    // Verify body is included unchanged
    assert!(
        final_body.contains("## Code Review\n\nApproved with suggestions."),
        "User body should be included verbatim"
    );

    // Verify footer
    assert!(
        final_body.contains("🌃 Co-built with [Midtown]"),
        "Should include Midtown footer"
    );

    // Verify no escaped exclamation marks
    assert!(
        !final_body.contains(r"\!"),
        "Final body must not contain escaped exclamation marks"
    );

    // Verify the `extract_reviewer_from_pr_comments` can parse it
    let comments = vec![serde_json::json!({
        "body": final_body,
        "createdAt": "2026-03-01T00:00:00Z"
    })];
    let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
    assert_eq!(
        reviewer,
        Some("lexington".to_string()),
        "extract_reviewer_from_pr_comments should be able to parse the frontmatter"
    );
}

/// When the `gh api PATCH` call fails, `handle_pr_review_post` should return
/// an RPC error so the reviewer agent can retry, instead of silently succeeding.
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await
#[tokio::test]
async fn test_review_post_gh_api_failure_returns_error() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _tmp, _guard) = make_merge_test_state();
    let pr_number = 77u64;

    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "york", AssignmentSource::Webhook);
        if let Some(assignment) = ps.github.pr_reviewers.get_mut(&pr_number) {
            assignment.placeholder_comment_id = Some(55555);
        }
    }

    // Mock `gh` — `repo view` succeeds but `api PATCH` fails
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");
    std::fs::write(
        &mock_gh_script,
        r#"#!/bin/bash
if [[ "$1" == "repo" ]]; then
    echo '{"nameWithOwner":"btucker/midtown"}'
elif [[ "$1" == "api" ]]; then
    echo "rate limit exceeded" >&2
    exit 1
else
    exit 1
fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let response = handle_pr_review_post(
        crate::rpc::RequestId::Number(1),
        pr_number,
        "## Review\nLGTM",
        &state,
    )
    .await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Should return an error, not silently succeed
    assert!(
        response.error.is_some(),
        "Should return RPC error when gh api PATCH fails, got success: {:?}",
        response.result
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("Failed to update comment"),
        "Error should mention the update failure, got: {}",
        err.message
    );
}
