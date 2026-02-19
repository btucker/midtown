//! Tests for channel RPC handlers.

use super::*;
use std::process::Command;

fn make_test_state(repo_name: &str) -> DaemonState {
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

    // Leak temp_dir so it survives the test
    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    DaemonState::new(
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
    .expect("daemon state")
}

#[test]
fn test_unescape_shell_artifacts_exclamation() {
    assert_eq!(
        unescape_shell_artifacts("Game time\\! Let's go"),
        "Game time! Let's go"
    );
}

#[test]
fn test_unescape_shell_artifacts_multiple_exclamations() {
    assert_eq!(
        unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
        "Wow! Amazing! Done!"
    );
}

#[test]
fn test_unescape_shell_artifacts_no_escapes() {
    assert_eq!(
        unescape_shell_artifacts("Normal message with ! marks"),
        "Normal message with ! marks"
    );
}

#[test]
fn test_unescape_shell_artifacts_preserves_other_backslashes() {
    assert_eq!(
        unescape_shell_artifacts("path\\to\\file and \\!"),
        "path\\to\\file and !"
    );
}

#[test]
fn test_extract_review_note_pr_standard_format() {
    let msg = "@lead [Review Note] PR #708: The new is_ui_chrome() pattern for ctrl+ key hints is heuristic. Please determine if this warrants a follow-up task.";
    assert_eq!(extract_review_note_pr(msg), Some(708));
}

#[test]
fn test_extract_review_note_pr_no_match() {
    assert_eq!(extract_review_note_pr("@lead some regular message"), None);
    assert_eq!(extract_review_note_pr("fixed PR #42"), None);
    assert_eq!(extract_review_note_pr("[Review Note] no PR ref"), None);
}

#[test]
fn test_extract_review_note_pr_various_numbers() {
    assert_eq!(
        extract_review_note_pr("@lead [Review Note] PR #1: minor issue"),
        Some(1)
    );
    assert_eq!(
        extract_review_note_pr("@lead [Review Note] PR #9999: edge case"),
        Some(9999)
    );
}

#[tokio::test]
async fn test_user_message_queues_headed_lead_nudge() {
    let state = make_test_state("midtown-test-rpc-channel-queue-user");
    let adapter_id = "test-adapter-user";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "please check this",
        None,
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "nudge_text");
    assert_eq!(messages[0].text, "user: please check this");
    assert!(messages[0].submit);
}

#[tokio::test]
async fn test_coworker_at_lead_queues_headed_lead_nudge() {
    let state = make_test_state("midtown-test-rpc-channel-queue-coworker");
    let adapter_id = "test-adapter-coworker";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    let response = handle_channel_post(
        2_i64.into(),
        "york",
        "@lead need a review",
        None,
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].text.contains("york mentioned @lead"),
        "queue entry should summarize coworker @lead mention"
    );
}

#[test]
fn test_parse_duration_seconds() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5sec"), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("10second"), Some(Duration::from_secs(10)));
    assert_eq!(parse_duration("15seconds"), Some(Duration::from_secs(15)));
}

#[test]
fn test_parse_duration_minutes() {
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("10min"), Some(Duration::from_secs(600)));
    assert_eq!(parse_duration("2minute"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("3minutes"), Some(Duration::from_secs(180)));
}

#[test]
fn test_parse_duration_hours() {
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("2hr"), Some(Duration::from_secs(7200)));
    assert_eq!(parse_duration("3hour"), Some(Duration::from_secs(10800)));
    assert_eq!(parse_duration("4hours"), Some(Duration::from_secs(14400)));
}

#[test]
fn test_parse_duration_days() {
    assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86400)));
    assert_eq!(parse_duration("2day"), Some(Duration::from_secs(172800)));
    assert_eq!(parse_duration("3days"), Some(Duration::from_secs(259200)));
}

#[test]
fn test_parse_duration_invalid() {
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("5x"), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("5.5m"), None); // floats not supported
}

/// Verify that a user message to a topic channel with no active channel lead
/// succeeds without error and does NOT nudge the main lead.
#[tokio::test]
async fn test_user_message_to_topic_channel_no_lead_no_main_nudge() {
    let state = make_test_state("midtown-test-topic-no-lead");
    let adapter_id = "test-adapter-topic-no-lead";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    // Post to a topic channel with no active channel lead
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "hello topic",
        Some("auth-refactor"),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Main lead should NOT be nudged for topic channel user messages
    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert!(
        messages.is_empty(),
        "Main lead should not be nudged when user posts to a topic channel without a channel lead"
    );
}

/// Verify that a user message to the main channel still nudges the main lead.
#[tokio::test]
async fn test_user_message_to_main_channel_nudges_lead() {
    let state = make_test_state("midtown-test-main-channel-nudge");
    let adapter_id = "test-adapter-main-nudge";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    // Post to main channel (None = default channel)
    let response =
        handle_channel_post(2_i64.into(), "user", "hello main", None, None, &state).await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Main lead should be nudged for main channel user messages"
    );
    assert_eq!(messages[0].text, "user: hello main");
}

/// Verify that a user message to the main channel nudges the lead EVEN when the user
/// @mentions a specific coworker. The lead should always be informed of user messages.
#[tokio::test]
async fn test_user_message_with_coworker_mention_still_nudges_lead() {
    let state = make_test_state("midtown-test-lead-nudge-despite-mention");
    let adapter_id = "test-adapter-lead-nudge-mention";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    // Post a message that @mentions a coworker (not @lead)
    let response = handle_channel_post(
        3_i64.into(),
        "user",
        "@york can you check this?",
        None,
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Lead should be nudged even when user @mentions a coworker"
    );
    assert_eq!(messages[0].text, "user: @york can you check this?");
}

/// Verify that a user message to a topic channel with an inactive session
/// attempts to resume the channel lead (succeeds without error, no main lead nudge).
///
/// In the test environment, spawn_coworker will fail (no real worktrees), but the
/// error is handled gracefully and the main lead is not nudged.
#[tokio::test]
async fn test_user_message_to_topic_channel_inactive_lead_attempts_resume() {
    let state = make_test_state("midtown-test-topic-resume-lead");
    let adapter_id = "test-adapter-topic-resume";
    state
        .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
        .await
        .expect("register headed adapter");

    // Persist a saved session ID for the channel lead so the resume path is taken
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions.insert(
            "auth-refactor".to_string(),
            "saved-session-id-abc".to_string(),
        );
    }

    // Post to the topic channel — channel lead is not running
    let response = handle_channel_post(
        4_i64.into(),
        "user",
        "need auth help",
        Some("auth-refactor"),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Main lead should NOT be nudged (topic channel message)
    let (messages, _capture) = state
        .headed_poll("lead", adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert!(
        messages.is_empty(),
        "Main lead should not be nudged when user posts to a topic channel"
    );
}

/// Verify that channel.read with a channel parameter reads from the specified
/// topic channel, not the main channel.
///
/// Regression test for: channel leads calling `midtown channel read` always
/// got main channel messages instead of their topic channel messages.
#[tokio::test]
async fn test_channel_read_with_channel_parameter() {
    let state = make_test_state("midtown-test-channel-read-channel");

    // Post a message to the topic channel
    let _r = handle_channel_post(
        1_i64.into(),
        "user",
        "hello topic",
        Some("auth"),
        None,
        &state,
    )
    .await;

    // Post a different message to the main channel
    let _r = handle_channel_post(2_i64.into(), "user", "hello main", None, None, &state).await;

    // Reading from the topic channel should return only the topic message
    let response = handle_channel_read(999.into(), true, None, None, Some("auth"), &state);

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "Expected 1 message from topic channel");
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("hello topic"),
        "Expected topic channel message, got: {:?}",
        messages[0]["message"]
    );
}

/// Verify that passing thread_parent_id to handle_channel_post results in the
/// message being stored with thread_parent_id set in the channel log.
#[tokio::test]
async fn test_channel_post_with_thread_parent_id() {
    let state = make_test_state("midtown-test-thread-parent-id");
    let parent_id = "parent-msg-uuid-123";

    // Post a thread reply
    let response = handle_channel_post(
        1_i64.into(),
        "york",
        "This is a reply in a thread",
        None,
        Some(parent_id),
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Read back messages and verify thread_parent_id is set
    let channel = state.channel_router.default_channel().unwrap();
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].thread_parent_id,
        Some(parent_id.to_string()),
        "Message should have thread_parent_id set"
    );
}

/// Verify that posting without thread_parent_id still works (None case).
#[tokio::test]
async fn test_channel_post_without_thread_parent_id() {
    let state = make_test_state("midtown-test-no-thread-parent-id");

    let response = handle_channel_post(
        1_i64.into(),
        "york",
        "Top-level message",
        None,
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let channel = state.channel_router.default_channel().unwrap();
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].thread_parent_id, None,
        "Message should not have thread_parent_id set"
    );
}

/// Verify that handle_channel_read includes thread_parent_id in the response
/// when a message has one set.
#[tokio::test]
async fn test_channel_read_includes_thread_parent_id() {
    let state = make_test_state("midtown-test-channel-read-thread-parent-id");
    let parent_id = "parent-uuid-abc";

    // Post a top-level message
    let _r = handle_channel_post(
        1_i64.into(),
        "park",
        "Top-level message",
        None,
        None,
        &state,
    )
    .await;

    // Post a thread reply
    let _r = handle_channel_post(
        2_i64.into(),
        "york",
        "Thread reply",
        None,
        Some(parent_id),
        &state,
    )
    .await;

    let response = handle_channel_read(999.into(), true, None, None, None, &state);

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);

    // Top-level message should not have thread_parent_id
    assert!(
        messages[0].get("thread_parent_id").is_none(),
        "Top-level message should not have thread_parent_id in response"
    );

    // Thread reply should have thread_parent_id
    assert_eq!(
        messages[1].get("thread_parent_id").and_then(|v| v.as_str()),
        Some(parent_id),
        "Thread reply should have thread_parent_id in RPC response"
    );
}

#[tokio::test]
async fn test_channel_read_with_last_parameter() {
    let state = make_test_state("midtown-test-channel-read-last");

    // Post 10 messages to the channel
    for i in 1..=10 {
        let msg = format!("Test message {}", i);
        let _response = handle_channel_post(i.into(), "test", &msg, None, None, &state).await;
    }

    // Request last 3 messages
    let response = handle_channel_read(999.into(), false, Some(3), None, None, &state);

    // Verify we got exactly 3 messages
    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 messages, got {}",
        messages.len()
    );

    // Verify they are the last 3 messages
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("message 8")
    );
    assert!(
        messages[1]["message"]
            .as_str()
            .unwrap()
            .contains("message 9")
    );
    assert!(
        messages[2]["message"]
            .as_str()
            .unwrap()
            .contains("message 10")
    );
}
