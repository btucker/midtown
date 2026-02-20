//! Tests for insight RPC handlers.

use std::process::Command;

use super::super::DaemonState;
use super::handle_insight_report;
use super::hash_insight;
use crate::rpc::RequestId;

#[test]
fn test_hash_insight_deterministic() {
    let hash1 = hash_insight("Test insight content");
    let hash2 = hash_insight("Test insight content");
    assert_eq!(hash1, hash2);
}

#[test]
fn test_hash_insight_different_content() {
    let hash1 = hash_insight("Insight one");
    let hash2 = hash_insight("Insight two");
    assert_ne!(hash1, hash2);
}

#[test]
fn test_hash_insight_normalizes_whitespace() {
    let hash1 = hash_insight("This is an insight");
    let hash2 = hash_insight("  This  is   an   insight  ");
    let hash3 = hash_insight("This\n  is\nan\ninsight");
    let hash4 = hash_insight("THIS IS AN INSIGHT");

    assert_eq!(hash1, hash2, "extra whitespace should be normalized");
    assert_eq!(hash1, hash3, "newlines should be normalized");
    assert_eq!(hash1, hash4, "case should be normalized");
}

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

/// Posting an insight to the main channel should succeed without attempting to
/// nudge a channel lead (there is none for the main channel).
#[tokio::test]
async fn test_insight_main_channel_posts_successfully() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    let response = handle_insight_report(
        RequestId::Number(1),
        "coworker1",
        "An insight",
        None,
        &state,
    )
    .await;

    let result = response.result.expect("should return success result");
    assert_eq!(result["posted"], true);
}

/// Posting an insight to a topic channel should succeed even when no channel
/// lead session is active — the nudge is skipped gracefully.
#[tokio::test]
async fn test_insight_topic_channel_skips_nudge_when_no_session() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    let response = handle_insight_report(
        RequestId::Number(2),
        "coworker1",
        "Topic insight",
        Some("my-topic"),
        &state,
    )
    .await;

    // Should succeed — nudge is silently skipped when channel lead isn't running.
    let result = response.result.expect("should return success result");
    assert_eq!(result["posted"], true);
}

/// Duplicate insights should be deduplicated and return posted=false.
#[tokio::test]
async fn test_insight_deduplication() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    let first = handle_insight_report(
        RequestId::Number(1),
        "coworker1",
        "Unique insight text",
        None,
        &state,
    )
    .await;
    assert_eq!(first.result.unwrap()["posted"], true);

    let second = handle_insight_report(
        RequestId::Number(2),
        "coworker1",
        "Unique insight text",
        None,
        &state,
    )
    .await;
    let second_result = second.result.unwrap();
    assert_eq!(second_result["posted"], false);
    assert_eq!(second_result["reason"], "duplicate");
}
