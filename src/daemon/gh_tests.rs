use super::*;

// These tests call the real `gh` CLI and require GitHub auth + a real repo.
// They are `#[ignore]`d and run only when explicitly opted in.

/// gh_patch_comment returns Err when the comment does not exist (404 from gh).
#[tokio::test]
#[ignore]
async fn gh_patch_comment_returns_err_on_missing_comment() {
    let result = gh_patch_comment("owner/repo", 999_999_999_999, "body").await;
    assert!(result.is_err(), "expected Err for nonexistent comment");
}

/// gh_delete_comment returns Err when the comment does not exist.
#[tokio::test]
#[ignore]
async fn gh_delete_comment_returns_err_on_missing_comment() {
    let result = gh_delete_comment("owner/repo", 999_999_999_999).await;
    assert!(result.is_err(), "expected Err for nonexistent comment");
}
