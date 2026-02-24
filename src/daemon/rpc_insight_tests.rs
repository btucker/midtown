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

/// When a coworker reports an insight without specifying a channel and they are
/// assigned to a task with a channel, the insight is routed to the task channel,
/// not the main channel.
#[tokio::test]
async fn test_insight_routes_to_task_channel_when_no_explicit_channel() {
    let (state, temp_dir, _guard) = make_test_state("testrepo");

    // Assign coworker1 to task 42 in channel "my-feature"
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "test-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "test-session-id".to_string(),
                task_id: Some("42".to_string()),
                current_name: Some("coworker1".to_string()),
                ..Default::default()
            },
        );
        ps.task_channel
            .insert("42".to_string(), "my-feature".to_string());
    }

    let response = handle_insight_report(
        RequestId::Number(1),
        "coworker1",
        "A task-specific insight",
        None, // No explicit channel — should auto-route to "my-feature"
        &state,
    )
    .await;

    assert_eq!(response.result.expect("should succeed")["posted"], true);

    // The insight should have been routed to the "my-feature" task channel
    let task_channel_file = temp_dir
        .path()
        .join("channels")
        .join("my-feature")
        .join("history")
        .join("current.jsonl");
    assert!(
        task_channel_file.exists(),
        "insight should be posted to the task channel, not main"
    );
    let content = std::fs::read_to_string(&task_channel_file).unwrap();
    assert!(
        content.contains("A task-specific insight"),
        "insight text should be in task channel file"
    );

    // The main channel should NOT have the insight
    let main_channel_file = temp_dir
        .path()
        .join("channels")
        .join("midtown")
        .join("history")
        .join("current.jsonl");
    let main_content = std::fs::read_to_string(&main_channel_file).unwrap_or_default();
    assert!(
        !main_content.contains("A task-specific insight"),
        "insight should not be cross-posted to main channel"
    );
}

/// When a channel lead session reports an insight, the RPC should return early
/// with posted=false and reason="channel_lead" — their output already reaches
/// the channel via the normal auto-posting path.
///
/// Suppression is driven solely by the `sessions` map (coworker_type == "channel-lead"),
/// not by `channel_lead_sessions`.
#[tokio::test]
async fn test_insight_channel_lead_suppressed() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    // Register a channel lead session via `sessions` — this is what the
    // implementation checks. `channel_lead_sessions` is NOT consulted.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "cl-session-abc".to_string(),
            super::super::state::SessionRecord {
                session_id: "cl-session-abc".to_string(),
                current_name: Some("ops-lead".to_string()),
                coworker_type: "channel-lead".to_string(),
                working_dir: "/tmp/test".to_string(),
                ..Default::default()
            },
        );
    }

    let response = handle_insight_report(
        RequestId::Number(10),
        "ops-lead",
        "An insight from the channel lead",
        None,
        &state,
    )
    .await;

    let result = response.result.expect("should return success result");
    assert_eq!(result["posted"], false);
    assert_eq!(result["reason"], "channel_lead");
}

/// Suppression does NOT fire when only `channel_lead_sessions` is populated
/// but the agent's `sessions` entry is missing. The check reads `sessions`,
/// not `channel_lead_sessions`.
#[tokio::test]
async fn test_insight_channel_lead_sessions_alone_does_not_suppress() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    // Populate channel_lead_sessions but leave `sessions` empty.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("ops".to_string(), "cl-session-abc".to_string());
        // Intentionally NOT inserting into ps.sessions
    }

    let response = handle_insight_report(
        RequestId::Number(10),
        "ops-lead",
        "An insight that should not be suppressed",
        None,
        &state,
    )
    .await;

    // Without a matching `sessions` entry, suppression does not fire.
    let result = response.result.expect("should return success result");
    assert_eq!(result["posted"], true);
}

/// A channel lead reporting an insight still records the hash, so a non-lead
/// coworker reporting the same insight text afterwards is correctly deduplicated.
#[tokio::test]
async fn test_insight_channel_lead_hash_recorded_for_dedup() {
    let (state, _temp_dir, _guard) = make_test_state("testrepo");

    // Register a channel lead session
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "cl-session-abc".to_string(),
            super::super::state::SessionRecord {
                session_id: "cl-session-abc".to_string(),
                current_name: Some("ops-lead".to_string()),
                coworker_type: "channel-lead".to_string(),
                working_dir: "/tmp/test".to_string(),
                ..Default::default()
            },
        );
    }

    // Channel lead reports the insight first (suppressed, not posted)
    let lead_response = handle_insight_report(
        RequestId::Number(1),
        "ops-lead",
        "Shared insight text",
        None,
        &state,
    )
    .await;
    let lead_result = lead_response.result.expect("should succeed");
    assert_eq!(lead_result["reason"], "channel_lead");

    // Non-lead coworker reports the same insight text → should be deduplicated
    let coworker_response = handle_insight_report(
        RequestId::Number(2),
        "coworker1",
        "Shared insight text",
        None,
        &state,
    )
    .await;
    let coworker_result = coworker_response.result.expect("should succeed");
    assert_eq!(coworker_result["posted"], false);
    assert_eq!(coworker_result["reason"], "duplicate");
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
