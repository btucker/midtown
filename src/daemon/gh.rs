//! GitHub CLI helpers for comment mutation.
//!
//! Thin async wrappers around `gh api` for PATCH and DELETE operations on
//! issue comments. Returns `Result<(), String>` — callers decide how to log
//! and whether to surface errors to RPC callers.

use crate::process::check_cmd_result;

/// Update the body of a GitHub issue comment via `gh api PATCH`.
///
/// Returns `Ok(())` on success. Returns `Err(msg)` where `msg` is the trimmed
/// stderr on non-zero exit, or the spawn error string if the process could not
/// be started.
pub(crate) async fn gh_patch_comment(
    repo: &str,
    comment_id: u64,
    body: &str,
) -> Result<(), String> {
    let endpoint = format!("/repos/{}/issues/comments/{}", repo, comment_id);
    let body_field = format!("body={}", body);
    check_cmd_result(
        tokio::process::Command::new("gh")
            .args(["api", "--method", "PATCH", &endpoint, "-f", &body_field])
            .output()
            .await,
    )
    .map(|_| ())
}

/// Delete a GitHub issue comment via `gh api DELETE`.
///
/// Returns `Ok(())` on success. Returns `Err(msg)` where `msg` is the trimmed
/// stderr on non-zero exit, or the spawn error string if the process could not
/// be started.
pub(crate) async fn gh_delete_comment(repo: &str, comment_id: u64) -> Result<(), String> {
    let endpoint = format!("/repos/{}/issues/comments/{}", repo, comment_id);
    check_cmd_result(
        tokio::process::Command::new("gh")
            .args(["api", "--method", "DELETE", &endpoint])
            .output()
            .await,
    )
    .map(|_| ())
}

#[path = "gh_tests.rs"]
#[cfg(test)]
mod tests;
