//! Tests for channel RPC handlers.

use super::*;
use std::process::Command;

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

    let channel_router = crate::ChannelRouter::new(&base_dir, repo_name);
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
    let (state, _tmp, _guard) = make_test_state("midtown-test-rpc-channel-queue-user");
    let adapter_id = "test-adapter-user";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
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
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "nudge_text");
    assert!(
        messages[0].text.starts_with("user (")
            && messages[0].text.ends_with("): please check this"),
        "nudge text should be 'user (<id>): please check this', got: {}",
        messages[0].text
    );
    assert!(messages[0].submit);
}

#[tokio::test]
async fn test_user_at_project_name_queues_single_nudge() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-rpc-channel-user-at-project");
    let adapter_id = "test-adapter-user-at-project";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    let msg = format!("@{} please ack", state.repo_name);
    let response = handle_channel_post(1_i64.into(), "user", &msg, None, None, &state).await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");

    assert_eq!(
        messages.len(),
        1,
        "user @project message should nudge lead exactly once"
    );
    assert_eq!(messages[0].kind, "nudge_text");
    assert!(
        messages[0].text.starts_with("user ("),
        "expected user-message nudge format, got: {}",
        messages[0].text
    );
}

#[tokio::test]
async fn test_coworker_at_lead_queues_headed_lead_nudge() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-rpc-channel-queue-coworker");
    let adapter_id = "test-adapter-coworker";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
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
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .text
            .contains("york mentioned @midtown-test-rpc-channel-queue-coworker"),
        "queue entry should summarize coworker @{{project_name}} mention"
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
    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-no-lead");
    let adapter_id = "test-adapter-topic-no-lead";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
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
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
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
    let (state, _tmp, _guard) = make_test_state("midtown-test-main-channel-nudge");
    let adapter_id = "test-adapter-main-nudge";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Post to main channel (None = default channel)
    let response =
        handle_channel_post(2_i64.into(), "user", "hello main", None, None, &state).await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Main lead should be nudged for main channel user messages"
    );
    assert!(
        messages[0].text.starts_with("user (") && messages[0].text.ends_with("): hello main"),
        "nudge text should be 'user (<id>): hello main', got: {}",
        messages[0].text
    );
}

/// Verify that a user message to the main channel nudges the lead EVEN when the user
/// @mentions a specific coworker. The lead should always be informed of user messages.
#[tokio::test]
async fn test_user_message_with_coworker_mention_still_nudges_lead() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-lead-nudge-despite-mention");
    let adapter_id = "test-adapter-lead-nudge-mention";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
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
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Lead should be nudged even when user @mentions a coworker"
    );
    assert!(
        messages[0].text.starts_with("user (")
            && messages[0].text.ends_with("): @york can you check this?"),
        "nudge text should be 'user (<id>): @york can you check this?', got: {}",
        messages[0].text
    );
}

/// Verify that a user message to a topic channel with an inactive session
/// attempts to resume the channel lead (succeeds without error, no main lead nudge).
///
/// In the test environment, spawn_coworker will fail (no real worktrees), but the
/// error is handled gracefully and the main lead is not nudged.
#[tokio::test]
async fn test_user_message_to_topic_channel_inactive_lead_attempts_resume() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-resume-lead");
    let adapter_id = "test-adapter-topic-resume";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
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
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
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
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-read-channel");

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
    let response = handle_channel_read(999.into(), true, None, None, Some("auth"), &state).await;

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
    let (state, _tmp, _guard) = make_test_state("midtown-test-thread-parent-id");
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
    let (state, _tmp, _guard) = make_test_state("midtown-test-no-thread-parent-id");

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
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-read-thread-parent-id");
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

    let response = handle_channel_read(999.into(), true, None, None, None, &state).await;

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
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-read-last");

    // Post 10 messages to the channel
    for i in 1..=10 {
        let msg = format!("Test message {}", i);
        let _response = handle_channel_post(i.into(), "test", &msg, None, None, &state).await;
    }

    // Request last 3 messages
    let response = handle_channel_read(999.into(), false, Some(3), None, None, &state).await;

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

/// Verify that the fresh spawn path registers a placeholder in `channel_lead_sessions`
/// so that NudgeChannelLead effects are not silently dropped and daemon restart
/// recovery can find this channel's session.
#[tokio::test]
async fn test_fresh_spawn_registers_channel_lead_sessions() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-fresh-spawn-register");
    let adapter_id = "test-adapter-fresh-spawn";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // No saved session ID — fresh spawn path will be taken.
    // Verify channel_lead_sessions is empty beforehand.
    {
        let ps = state.persistent_state.lock().await;
        assert!(
            !ps.channel_lead_sessions.contains_key("new-feature"),
            "Precondition: channel_lead_sessions should not contain 'new-feature'"
        );
    }

    // Post to topic channel — triggers fresh spawn path.
    // In test env, spawn_coworker may succeed or fail depending on environment,
    // but the placeholder registration should happen before spawn is attempted.
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "start work on new feature",
        Some("new-feature"),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // The placeholder is registered before spawn and kept regardless of whether
    // spawn succeeds or fails. An empty-string placeholder is harmless — startup
    // recovery handles it with SessionMode::Fresh. Keeping it ensures the channel
    // is registered for restart recovery even if this spawn attempt failed.
    {
        let ps = state.persistent_state.lock().await;
        assert!(
            ps.channel_lead_sessions.contains_key("new-feature"),
            "channel_lead_sessions should contain placeholder for 'new-feature' after fresh spawn"
        );
    }
}

/// Verify that when a channel lead session is marked stopped (session record with
/// is_running=false), the resume path is skipped even if `channel_lead_sessions`
/// still has a stale session ID. This prevents crash loops.
#[tokio::test]
async fn test_crash_loop_guard_skips_resume_when_headless_cleared() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-crash-loop-guard");
    let adapter_id = "test-adapter-crash-loop";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Set up the crash-loop scenario:
    // - channel_lead_sessions has a stale session ID
    // - the corresponding SessionRecord is marked as not running (death handler cleared it)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions.insert(
            "auth-refactor".to_string(),
            "stale-session-id-xyz".to_string(),
        );
        ps.sessions.insert(
            "stale-session-id-xyz".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "stale-session-id-xyz".to_string(),
                current_name: Some("auth-refactor".to_string()),
                coworker_type: "channel-lead".to_string(),
                channel: Some("auth-refactor".to_string()),
                is_running: false,
                resume_on_startup: false,
                ..Default::default()
            },
        );
    }

    // Post to topic channel — should NOT attempt resume with stale ID,
    // should fall back to Fresh spawn instead.
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "check auth status",
        Some("auth-refactor"),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Main lead should NOT be nudged (topic channel message)
    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert!(
        messages.is_empty(),
        "Main lead should not be nudged for topic channel user messages"
    );
}

#[tokio::test]
async fn test_handle_channel_post_clears_stale_channel_lead_mapping_on_resume_failure() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-lead-resume-failure");
    let adapter_id = "test-adapter-channel-lead-resume-failure";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    let channel_name = "multi-platform";
    let stale_session_id = "non-existent-session-id-xyz";

    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_name.to_string(), stale_session_id.to_string());
    }

    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "can you continue this flow?",
        Some(channel_name),
        None,
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "channel.post should succeed even when resume path fails"
    );

    let ps = state.persistent_state.lock().await;
    let mapped = ps.channel_lead_sessions.get(channel_name).cloned();
    assert!(
        mapped.as_deref() != Some(stale_session_id),
        "Stale channel lead session mapping should not be reused after resume failure"
    );
}

/// When a user sends a thread reply, the lead nudge must use the PARENT's ID
/// (thread_parent_id), not the reply's own ID.
///
/// Bug: using the reply's own ID caused the lead to reply with `--thread reply_id`,
/// creating a nested reply that is invisible to the user in both the web UI and TUI.
/// The user's message appeared to "reply to itself" because the lead echoed the content
/// with the wrong thread ID, making the response appear in the wrong thread slot.
///
/// Fix: pass `thread_parent_id` as `msg_id` in `WakeReason::UserMessage` so the lead
/// uses `--thread parent_id` and creates a sibling reply in the correct thread.
#[tokio::test]
async fn test_user_thread_reply_nudge_uses_parent_id() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-thread-reply-nudge-parent-id");
    let adapter_id = "test-adapter-thread-reply-nudge";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    let parent_id = "parent-message-uuid-456";

    // Post a user thread reply (simulating the user sending a reply from the thread panel)
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "this is my thread reply",
        None,
        Some(parent_id),
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "nudge_text");

    // The nudge MUST use the parent's ID so the lead replies with --thread parent_id,
    // creating a sibling reply in the correct thread. Using the reply's own UUID
    // would cause the lead to create a nested reply invisible to the user.
    let expected = format!("user ({}): this is my thread reply", parent_id);
    assert_eq!(
        messages[0].text, expected,
        "nudge for thread reply should use parent_id '{}', not the reply's own UUID",
        parent_id
    );
}

/// Verify that `clear_lead_respawn_cooldown` removes the lead entry from
/// `coworker_stop_times`, allowing `ensure_lead_alive()` to respawn on the next tick.
#[tokio::test]
async fn test_clear_lead_respawn_cooldown_removes_stop_time() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-clear-lead-cooldown");

    // After rename, lead stop times are keyed by repo_name (lowercase), not "lead"
    let lead_key = state.repo_name.to_lowercase();

    // Simulate lead having been stopped (which sets a stop time)
    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(lead_key.clone(), chrono::Utc::now());
    }

    // Precondition: stop time is set
    {
        let stop_times = state.coworker_stop_times.read().unwrap();
        assert!(
            stop_times.contains_key(&lead_key),
            "Precondition: lead stop time should be set (key: {})",
            lead_key
        );
    }

    // Clear the cooldown
    state.clear_lead_respawn_cooldown();

    // Postcondition: stop time is removed
    {
        let stop_times = state.coworker_stop_times.read().unwrap();
        assert!(
            !stop_times.contains_key(&lead_key),
            "Lead stop time should be removed after clear_lead_respawn_cooldown"
        );
    }
}

/// Verify that `clear_lead_respawn_cooldown` is a no-op when no stop time exists.
#[test]
fn test_clear_lead_respawn_cooldown_noop_when_no_stop_time() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (state, _tmp, _guard) = make_test_state("midtown-test-clear-lead-noop");

        // No stop time set — clearing should not panic
        state.clear_lead_respawn_cooldown();

        let lead_key = state.repo_name.to_lowercase();
        let stop_times = state.coworker_stop_times.read().unwrap();
        assert!(
            !stop_times.contains_key(&lead_key),
            "No stop time should exist after no-op clear"
        );
    });
}

/// Verify that a user message on the main channel while the lead is dead
/// clears the respawn cooldown (lead stop time removed).
#[tokio::test]
async fn test_user_message_with_dead_lead_clears_cooldown() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dead-lead-clears-cooldown");
    let adapter_id = "test-adapter-dead-lead-cooldown";
    let lead_key = state.repo_name.to_lowercase();
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Simulate lead having been stopped 2 minutes ago (within 5-min cooldown)
    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(
            lead_key.clone(),
            chrono::Utc::now() - chrono::Duration::minutes(2),
        );
    }

    // Lead is not alive (session_manager has no live session for repo_name)
    // and not attached — it is dead.
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "hello, anyone there?",
        None,
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // The cooldown should have been cleared, allowing immediate respawn on next tick
    let stop_times = state.coworker_stop_times.read().unwrap();
    assert!(
        !stop_times.contains_key(&lead_key),
        "Lead stop time should be cleared after user message with dead lead"
    );
}

/// Verify that channel.create creates a new channel successfully.
#[tokio::test]
async fn test_handle_channel_create_new_channel() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-create");

    let response = super::handle_channel_create(1_i64.into(), "new-channel", &state);
    assert!(response.error.is_none(), "channel.create should succeed");
    let result = response.result.unwrap();
    assert_eq!(result["success"].as_bool(), Some(true));
}

/// Verify that channel.create is idempotent: creating an existing channel succeeds.
#[tokio::test]
async fn test_handle_channel_create_idempotent() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-create-idempotent");

    // Create twice — second call should also succeed
    let r1 = super::handle_channel_create(1_i64.into(), "my-channel", &state);
    assert!(r1.error.is_none(), "first create should succeed");
    let r2 = super::handle_channel_create(2_i64.into(), "my-channel", &state);
    assert!(
        r2.error.is_none(),
        "second create (idempotent) should succeed"
    );
}

/// Verify that channel.archive archives an existing channel.
#[tokio::test]
async fn test_handle_channel_archive_existing_channel() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-archive");

    // Create the channel first
    let r = super::handle_channel_create(1_i64.into(), "old-channel", &state);
    assert!(r.error.is_none(), "create should succeed");

    // Archive it
    let response = super::handle_channel_archive(2_i64.into(), "old-channel", &state).await;
    assert!(response.error.is_none(), "channel.archive should succeed");
    let result = response.result.unwrap();
    assert_eq!(result["success"].as_bool(), Some(true));
}

/// Verify that channel.archive rejects archiving a non-existent channel.
#[tokio::test]
async fn test_handle_channel_archive_nonexistent_channel() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-archive-nonexistent");

    let response = super::handle_channel_archive(1_i64.into(), "does-not-exist", &state).await;
    assert!(
        response.error.is_some(),
        "archiving a non-existent channel should return an error"
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("does not exist"),
        "error message should indicate the channel does not exist"
    );
}

/// Verify that channel.archive rejects archiving the project's main channel.
///
/// The main channel name is dynamic — it matches the repo name — so this test
/// verifies the guard works regardless of what the project is called.
#[tokio::test]
async fn test_handle_channel_archive_rejects_main_channel() {
    let repo_name = "test-archive-guard-project";
    let (state, _tmp, _guard) = make_test_state(repo_name);

    // The default (main) channel has the same name as the repo
    let main_channel = state.channel_router.default_channel_name().to_string();
    let response = super::handle_channel_archive(1_i64.into(), &main_channel, &state).await;
    assert!(
        response.error.is_some(),
        "archiving the main channel '{}' should return an error",
        main_channel
    );
}

/// Verify that channel.archive cleans up channel_lead_sessions and session records.
///
/// Bug: archiving via CLI (`midtown channel archive`) didn't remove channel lead
/// session state. On-demand triggers (NudgeChannelLead) could then respawn the
/// lead, which recreated the archived channel directory.
#[tokio::test]
async fn test_handle_channel_archive_cleans_up_channel_lead_sessions() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-archive-cleanup-leads");

    // Create the channel
    let r = super::handle_channel_create(1_i64.into(), "feature-x", &state);
    assert!(r.error.is_none(), "create should succeed");

    // Simulate a running channel lead by inserting into persistent state
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("feature-x".to_string(), "session-fx-123".to_string());
        ps.sessions.insert(
            "session-fx-123".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "session-fx-123".to_string(),
                current_name: Some("feature-x".to_string()),
                coworker_type: "channel-lead".to_string(),
                channel: Some("feature-x".to_string()),
                is_running: true,
                resume_on_startup: false,
                ..Default::default()
            },
        );
    }

    // Archive the channel
    let response = super::handle_channel_archive(2_i64.into(), "feature-x", &state).await;
    assert!(response.error.is_none(), "channel.archive should succeed");

    // Verify channel_lead_sessions is cleaned up and session is marked stopped
    {
        let ps = state.persistent_state.lock().await;
        assert!(
            !ps.channel_lead_sessions.contains_key("feature-x"),
            "channel_lead_sessions should be cleaned up after archive"
        );
        // Session record should be marked as not running (not removed entirely)
        if let Some(record) = ps.sessions.get("session-fx-123") {
            assert!(
                !record.is_running,
                "session should be marked as stopped after archive"
            );
            assert!(
                !record.resume_on_startup,
                "session should not resume after archive"
            );
        }
    }
}

#[tokio::test]
async fn test_handle_channel_unarchive_restores_channel() {
    let (state, tmp, _guard) = make_test_state("midtown-test-channel-unarchive");

    // Create and archive channel
    let create_resp = super::handle_channel_create(1_i64.into(), "feature-x", &state);
    assert!(create_resp.error.is_none(), "create should succeed");
    let archive_resp = super::handle_channel_archive(2_i64.into(), "feature-x", &state).await;
    assert!(archive_resp.error.is_none(), "archive should succeed");

    // Unarchive
    let response = super::handle_channel_unarchive(3_i64.into(), "feature-x", &state);
    assert!(response.error.is_none(), "channel.unarchive should succeed");
    let result = response.result.unwrap();
    assert_eq!(result["success"].as_bool(), Some(true));

    // Verify directory moved back to active path
    let active_dir = tmp
        .path()
        .join("channels")
        .join("feature-x")
        .join("history")
        .join("current.jsonl");
    assert!(
        active_dir.exists(),
        "active channel history should exist after unarchive"
    );
    let archived_dir = tmp.path().join("channels").join("feature-x.archived");
    assert!(
        !archived_dir.exists(),
        "archived directory should be removed after unarchive"
    );
}

#[tokio::test]
async fn test_handle_channel_unarchive_requires_archived_channel() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-unarchive-error");

    // No archive yet
    let create_resp = super::handle_channel_create(1_i64.into(), "tui", &state);
    assert!(create_resp.error.is_none(), "create should succeed");

    let response = super::handle_channel_unarchive(2_i64.into(), "tui", &state);
    assert!(response.error.is_some(), "unarchive should fail");
    assert!(
        response
            .error
            .as_ref()
            .is_some_and(|err| err.message.contains("not archived")),
        "error should mention channel is not archived"
    );
}

/// Verify that a second rapid user message within 30s does NOT trigger expedite again
/// (cooldown dedup prevents spam).
#[tokio::test]
async fn test_user_message_dead_lead_respects_expedite_cooldown() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dead-lead-expedite-cooldown");
    let adapter_id = "test-adapter-dead-lead-expedite";
    let lead_key = state.repo_name.to_lowercase();
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Simulate lead having been stopped recently
    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(
            lead_key.clone(),
            chrono::Utc::now() - chrono::Duration::minutes(2),
        );
    }

    // First user message: expedite fires, cooldown cleared
    let _r = handle_channel_post(1_i64.into(), "user", "message one", None, None, &state).await;

    // Re-set stop time (simulate lead died again or cooldown was re-set)
    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(
            lead_key.clone(),
            chrono::Utc::now() - chrono::Duration::minutes(2),
        );
    }

    // Second user message within 30s: expedite cooldown should block re-expedite
    let _r = handle_channel_post(2_i64.into(), "user", "message two", None, None, &state).await;

    // The expedite_cooldown (lead_dead_expedite) should be active (recorded),
    // meaning the second message did not re-trigger expedite. Stop time was NOT cleared.
    let stop_times = state.coworker_stop_times.read().unwrap();
    assert!(
        stop_times.contains_key(&lead_key),
        "Lead stop time should remain on second message within 30s cooldown"
    );
}

// ── Output binding tests (Task 3) ────────────────────────────────────────────

/// Verify that a forked session with a bound thread has its channel posts
/// automatically tagged with that thread_parent_id (output binding).
///
/// This is Task 3: forked topic sessions post into the correct thread without
/// needing to pass `--thread` on every `midtown channel post` call.
///
/// Uses the in-memory `fork_bound_threads` cache (populated by `handle_session_fork`)
/// rather than looking up `bound_thread_id` via the async persistent_state lock.
#[tokio::test]
async fn test_output_binding_auto_tags_forked_session_posts() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-output-binding-auto");
    let thread_id = "thread-parent-uuid-xyz";
    let fork_name = "fork-abcdefgh";

    // Register the fork's bound thread in the in-memory cache
    state
        .fork_bound_threads
        .lock()
        .unwrap()
        .insert(fork_name.to_string(), thread_id.to_string());

    // Post a message from the forked session WITHOUT explicit thread_parent_id
    let response = handle_channel_post(
        1_i64.into(),
        fork_name,
        "Here is my analysis of the thread",
        None,
        None, // no explicit thread_parent_id
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // The message should have been auto-tagged with the bound thread_parent_id
    let channel = state.channel_router.default_channel().unwrap();
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].thread_parent_id,
        Some(thread_id.to_string()),
        "Forked session post should be auto-tagged with bound_thread_id"
    );
}

/// Verify that an explicit thread_parent_id takes priority over the session's
/// bound thread. The session's binding is a fallback, not an override.
#[tokio::test]
async fn test_output_binding_explicit_thread_takes_priority() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-output-binding-priority");
    let bound_thread = "bound-thread-id-111";
    let explicit_thread = "explicit-thread-id-999";
    let fork_name = "fork-priority";

    // Register the fork's bound thread in the in-memory cache
    state
        .fork_bound_threads
        .lock()
        .unwrap()
        .insert(fork_name.to_string(), bound_thread.to_string());

    // Post with an EXPLICIT thread_parent_id (different from the bound one)
    let response = handle_channel_post(
        1_i64.into(),
        fork_name,
        "Explicit thread reply",
        None,
        Some(explicit_thread), // explicit wins
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let channel = state.channel_router.default_channel().unwrap();
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].thread_parent_id,
        Some(explicit_thread.to_string()),
        "Explicit thread_parent_id should take priority over bound_thread_id"
    );
}

/// Verify that sessions without a bound thread do NOT get thread_parent_id
/// injected into their posts. Only forked sessions with a binding are affected.
#[tokio::test]
async fn test_output_binding_no_bound_thread_preserves_none() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-output-binding-none");
    let session_name = "park";

    // fork_bound_threads does NOT contain this session — no binding

    let response = handle_channel_post(
        1_i64.into(),
        session_name,
        "Top-level post with no binding",
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
        "Unbound session post should not have thread_parent_id set"
    );
}

// ── topic_sessions routing tests (Task 2) ────────────────────────────────────

/// Verify that when `topic_sessions` has a mapping for a thread, a user reply
/// in that thread is routed to the fork session (not the channel lead).
///
/// Since `NudgeSession` fails gracefully when no live session exists in test env,
/// the observable behavior is that the main lead is NOT nudged (the routing took
/// the fork path, not the fallback NudgeChannelLead path).
#[tokio::test]
async fn test_thread_routing_with_topic_session_routes_to_fork() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-thread-routing-fork");
    let adapter_id = "test-adapter-fork-routing";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    let thread_id = "thread-uuid-for-fork-routing";
    let fork_session_id = "fork-session-routing-xyz";

    // Register a topic session for this thread
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), fork_session_id.to_string());

    // User posts a thread reply in the topic channel
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "Follow-up question in thread",
        Some("auth-refactor"), // topic channel
        Some(thread_id),       // thread_parent_id with registered fork
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Main lead should NOT be nudged — routing went to the fork session path
    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert!(
        messages.is_empty(),
        "Main lead should not be nudged when routing to a fork session"
    );

    // The fork-session nudge must carry the thread_parent_id as msg_id so the fork
    // posts a sibling reply instead of nesting under the user reply UUID.
    let effect = super::build_topic_thread_nudge_effect(
        "auth-refactor",
        "Follow-up question in thread",
        thread_id.to_string(),
        Some(fork_session_id.to_string()),
    );
    match effect {
        crate::daemon::effects::Effect::NudgeSession { session_id, reason } => {
            assert_eq!(session_id, fork_session_id);
            match reason {
                crate::daemon::wake_reason::WakeReason::UserMessage { msg_id, .. } => {
                    assert_eq!(
                        msg_id, thread_id,
                        "Fork session nudge should target the thread parent"
                    );
                }
                other => panic!(
                    "Expected UserMessage reason for fork nudge, got {:?}",
                    other
                ),
            }
        }
        other => panic!("Expected NudgeSession effect, got {:?}", other),
    }
}

/// Verify that `handle_session_fork` deduplicates: when a topic session already
/// exists for a thread, the handler returns early with `already_exists: true`
/// and does not attempt to spawn a second fork.
#[tokio::test]
async fn test_topic_sessions_dedup_returns_existing() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-sessions-dedup");

    let thread_id = "thread-dedup-uuid";
    let first_fork = "fork-session-first";

    // Pre-populate topic_sessions as if a fork already exists for this thread.
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), first_fork.to_string());

    // Call handle_session_fork for the same thread — should hit the guard and
    // return the existing session without attempting to spawn.
    let response = super::super::rpc_session::handle_session_fork(
        1_i64.into(),
        thread_id,
        "calling-session-id-xyz",
        None,
        &state,
    )
    .await;

    // The guard should return success with already_exists: true
    assert!(
        response.error.is_none(),
        "Dedup guard should return success"
    );
    let result = response.result.unwrap();
    assert_eq!(
        result["already_exists"].as_bool(),
        Some(true),
        "Should indicate the session already exists"
    );
    assert_eq!(
        result["session_id"].as_str(),
        Some(first_fork),
        "Should return the existing fork session ID"
    );

    // topic_sessions should still hold the original mapping (not overwritten)
    let stored = state.topic_sessions.lock().unwrap().get(thread_id).cloned();
    assert_eq!(
        stored,
        Some(first_fork.to_string()),
        "Original fork session should be preserved"
    );
}

// ============================================================================
// channel.rename tests
// ============================================================================

/// Renaming a channel moves its directory and updates persistent state.
#[tokio::test]
async fn test_channel_rename_success() {
    let (state, tmp, _guard) = make_test_state("midtown-test-rename-success");
    let base_dir = tmp.path();

    // Create the "auth-v1" channel
    crate::Channel::new(base_dir, "auth-v1").expect("create auth-v1");

    let old_lead_session_name = crate::launch::channel_lead_session_name("auth-v1");

    // Seed persistent state: task_channel, channel_lead_sessions, sessions
    {
        let mut ps = state.persistent_state.lock().await;
        ps.task_channel
            .insert("42".to_string(), "auth-v1".to_string());
        ps.task_channel
            .insert("99".to_string(), "other-channel".to_string());
        ps.channel_lead_sessions
            .insert("auth-v1".to_string(), "session-auth-123".to_string());
        ps.sessions.insert(
            "session-auth-123".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "session-auth-123".to_string(),
                current_name: Some(old_lead_session_name.clone()),
                coworker_type: "channel-lead".to_string(),
                channel: Some("auth-v1".to_string()),
                is_running: true,
                resume_on_startup: false,
                ..Default::default()
            },
        );
    }

    let response = handle_channel_rename(1_i64.into(), "auth-v1", "auth-v2", &state).await;
    assert!(response.error.is_none(), "rename should succeed");
    let result = response.result.expect("should have result");
    assert_eq!(result["success"], true);

    // Old directory gone, new directory present
    assert!(
        !base_dir.join("channels").join("auth-v1").exists(),
        "old channel dir should not exist"
    );
    assert!(
        base_dir.join("channels").join("auth-v2").exists(),
        "new channel dir should exist"
    );

    // Verify persistent state was updated
    let ps = state.persistent_state.lock().await;
    assert_eq!(
        ps.task_channel.get("42").map(String::as_str),
        Some("auth-v2"),
        "task_channel entry for task 42 should be updated to new name"
    );
    assert_eq!(
        ps.task_channel.get("99").map(String::as_str),
        Some("other-channel"),
        "unrelated task_channel entry should be unchanged"
    );

    // Verify channel_lead_sessions is cleaned up (removed, not migrated)
    // so a fresh lead can be spawned on-demand for the new channel name.
    assert!(
        !ps.channel_lead_sessions.contains_key("auth-v1"),
        "old channel_lead_sessions entry should be removed"
    );
    assert!(
        !ps.channel_lead_sessions.contains_key("auth-v2"),
        "stale session ID should not be migrated to new name"
    );

    // Verify session record is marked as stopped (not running, not resume_on_startup)
    if let Some(record) = ps.sessions.get("session-auth-123") {
        assert!(
            !record.is_running,
            "old session should be marked as stopped after rename"
        );
        assert!(
            !record.resume_on_startup,
            "old session should not resume after rename"
        );
    }
}

/// Renaming a non-existent channel returns an error.
#[tokio::test]
async fn test_channel_rename_nonexistent_source() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-rename-nonexistent");

    let response = handle_channel_rename(2_i64.into(), "does-not-exist", "new-name", &state).await;
    assert!(
        response.error.is_some(),
        "should error when old channel does not exist"
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("does not exist"),
        "error message should mention missing channel, got: {}",
        err.message
    );
}

/// Renaming to an already-existing channel name returns an error.
#[tokio::test]
async fn test_channel_rename_target_already_exists() {
    let (state, tmp, _guard) = make_test_state("midtown-test-rename-target-exists");
    let base_dir = tmp.path();

    crate::Channel::new(base_dir, "old-name").expect("create old-name");
    crate::Channel::new(base_dir, "existing-name").expect("create existing-name");

    let response = handle_channel_rename(3_i64.into(), "old-name", "existing-name", &state).await;
    assert!(
        response.error.is_some(),
        "should error when target channel already exists"
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("already exists"),
        "error message should mention existing channel, got: {}",
        err.message
    );
}

/// Renaming the 'midtown' main channel returns an error.
#[tokio::test]
async fn test_channel_rename_midtown_forbidden() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-rename-midtown-forbidden");

    let response = handle_channel_rename(4_i64.into(), "midtown", "new-name", &state).await;
    assert!(
        response.error.is_some(),
        "should error when renaming the 'midtown' channel"
    );
    let err = response.error.unwrap();
    assert!(
        err.message.contains("midtown"),
        "error message should mention the channel restriction, got: {}",
        err.message
    );
}

/// Renaming to an invalid channel name returns an error.
#[tokio::test]
async fn test_channel_rename_invalid_new_name() {
    let (state, tmp, _guard) = make_test_state("midtown-test-rename-invalid-name");
    let base_dir = tmp.path();

    crate::Channel::new(base_dir, "valid-name").expect("create valid-name");

    let response = handle_channel_rename(5_i64.into(), "valid-name", "invalid name!", &state).await;
    assert!(
        response.error.is_some(),
        "should error when new name is invalid"
    );
}

/// Renaming evicts the old channel from the ChannelRouter cache.
#[tokio::test]
async fn test_channel_rename_evicts_router_cache() {
    let (state, tmp, _guard) = make_test_state("midtown-test-rename-evict-cache");
    let base_dir = tmp.path();

    crate::Channel::new(base_dir, "cached-channel").expect("create cached-channel");

    // Warm up the router cache by sending a message to the channel
    let msg = crate::message::Message::for_channel(
        "cached-channel",
        "test",
        "hello".to_string(),
        crate::message::MessageType::Text,
    );
    state
        .channel_router
        .send(&msg)
        .expect("send to prime cache");
    assert!(
        state
            .channel_router
            .open_channels()
            .contains(&"cached-channel".to_string()),
        "channel should be in router cache before rename"
    );

    let response =
        handle_channel_rename(6_i64.into(), "cached-channel", "renamed-channel", &state).await;
    assert!(response.error.is_none(), "rename should succeed");

    assert!(
        !state
            .channel_router
            .open_channels()
            .contains(&"cached-channel".to_string()),
        "old channel should be evicted from router cache after rename"
    );
}

/// Renaming with a path-traversal old name returns an error.
#[tokio::test]
async fn test_channel_rename_path_traversal_old_name() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-rename-path-traversal");

    let response = handle_channel_rename(7_i64.into(), "../escape", "new-name", &state).await;
    assert!(
        response.error.is_some(),
        "path traversal in old name should return an error"
    );
}

/// Renaming to a reserved avenue name returns an error.
#[tokio::test]
async fn test_channel_rename_reserved_avenue_name() {
    let (state, tmp, _guard) = make_test_state("midtown-test-rename-avenue-name");
    let base_dir = tmp.path();

    crate::Channel::new(base_dir, "my-channel").expect("create channel");

    let response = handle_channel_rename(8_i64.into(), "my-channel", "park", &state).await;
    assert!(
        response.error.is_some(),
        "renaming to a reserved avenue name should return an error"
    );
}

// ── DM channel routing tests ──────────────────────────────────────────────────

/// Posting to dm-<coworker> when no active session exists returns an error.
/// The channel directory is NOT created when validation fails.
#[tokio::test]
async fn test_dm_post_unknown_coworker_returns_error() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dm-unknown-coworker");

    // "york" has no entry in name_to_session — it's not an active coworker
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "Are you there?",
        Some("dm-york"),
        None,
        &state,
    )
    .await;

    assert!(
        response.error.is_some(),
        "posting to dm- channel with unknown coworker should return an error"
    );
    let err_msg = response.error.unwrap().message;
    assert!(
        err_msg.contains("york"),
        "error should mention the unknown coworker name, got: {}",
        err_msg
    );
}

/// Posting to dm-<coworker> when a session is active routes via NudgeSession
/// (not NudgeChannelLead), so the lead is NOT nudged.
///
/// Since `NudgeSession` fails gracefully when the headless session isn't live
/// in tests, we verify the lead adapter receives NO messages (DM path was taken).
#[tokio::test]
async fn test_dm_post_active_coworker_does_not_nudge_lead() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dm-active-coworker");

    // Register the lead's headed adapter so we can verify it gets no nudge
    let adapter_id = "test-dm-adapter";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Register "amsterdam" as an active coworker in name_to_session
    let coworker_session_id = "session-amsterdam-xyz";
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("amsterdam".to_string(), coworker_session_id.to_string());

    // User posts a DM to amsterdam
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "Hey amsterdam, can you help?",
        Some("dm-amsterdam"),
        None,
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "DM to active coworker should succeed"
    );

    // Lead should NOT be nudged — the DM was routed to the coworker's session
    let (messages, _capture) = state
        .headed_poll(&state.repo_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert!(
        messages.is_empty(),
        "Lead should not receive nudge when user sends DM to an active coworker"
    );
}

/// A non-user sender posting to a dm- channel goes through normally
/// without validation (DM validation only applies to user senders).
#[tokio::test]
async fn test_dm_post_from_coworker_is_not_blocked() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dm-coworker-sender");

    // Register the lead's headed adapter
    let adapter_id = "test-dm-coworker-adapter";
    state
        .headed_register(
            &state.repo_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // "amsterdam" posts to their own DM channel (replying to user)
    // No session registered — but coworker senders skip DM validation
    let response = handle_channel_post(
        1_i64.into(),
        "amsterdam",
        "Sure, I can help with that!",
        Some("dm-amsterdam"),
        None,
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "Coworker posting to their own dm- channel should not be blocked"
    );
}

// ============================================================================
// Auto-fork tests
// ============================================================================

/// When a user posts a new top-level message to a topic channel that has a known
/// channel lead session, no auto-fork is attempted. The channel lead handles
/// the message directly. Users dedicate sessions manually via the web UI.
#[tokio::test]
async fn test_user_message_to_topic_channel_with_lead_does_not_auto_fork() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-no-autofork");

    // Register a fake channel lead session for the "web" topic channel
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("web".to_string(), "fake-lead-session-id".to_string());
    }

    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "new question about the web UI",
        Some("web"),
        None,
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "channel.post should succeed without auto-fork"
    );

    // No fork should have been created — topic_sessions stays empty.
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        topic.is_empty(),
        "topic_sessions should be empty — no auto-fork for new top-level messages"
    );
}

/// When a user posts a thread reply to a topic channel that already has a fork
/// session registered in topic_sessions, the existing fork is used for routing
/// (existing behavior preserved).
#[tokio::test]
async fn test_thread_reply_routes_to_existing_fork_session() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-thread-reply-routing");

    // Pre-register a fork session for a specific thread
    let thread_parent_id = "existing-thread-msg-id";
    let fork_session_id = "existing-fork-session-id";
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_parent_id.to_string(), fork_session_id.to_string());

    // User posts a thread reply to this thread
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "follow-up question in thread",
        Some("web"),
        Some(thread_parent_id),
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "thread reply channel.post should succeed"
    );

    // topic_sessions should still contain the same fork (no new fork created)
    let topic = state.topic_sessions.lock().unwrap();
    assert_eq!(
        topic.get(thread_parent_id).map(String::as_str),
        Some(fork_session_id),
        "existing fork session should remain unchanged"
    );
    // And no "pending" sentinels
    assert!(
        !topic.values().any(|v| v == "pending"),
        "no pending sentinels from thread reply routing"
    );
}

/// When no channel lead session is registered for a topic channel, channel.post
/// succeeds and no fork is attempted (channel lead handles directly).
#[tokio::test]
async fn test_user_message_to_topic_channel_without_lead_skips_fork() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-autofork-no-lead");

    // No channel lead session registered for "feature-x"
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "question for the feature-x channel",
        Some("feature-x"),
        None,
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "channel.post should succeed when no channel lead exists"
    );

    // No fork should have been attempted (topic_sessions stays empty)
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        topic.is_empty(),
        "topic_sessions should be empty when no channel lead exists"
    );
}

/// When a thread reply arrives while a fork is still spawning ("pending"
/// sentinel in topic_sessions), the reply must NOT produce a NudgeSession with
/// session_id="pending" — that would silently drop the message. The handler
/// should filter out "pending" and fall back to NudgeChannelLead instead.
#[tokio::test]
async fn test_thread_reply_during_pending_fork_does_not_route_to_pending_session() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-pending-thread-reply");

    let thread_parent_id = "top-level-msg-pending-fork";
    // Simulate auto-fork in progress: sentinel is "pending", not a real session
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_parent_id.to_string(), "pending".to_string());

    // Thread reply arrives during the spawn window
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "follow-up reply while fork is spawning",
        Some("web"),
        Some(thread_parent_id),
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "thread reply should succeed even with pending fork"
    );

    // The "pending" sentinel must not have been removed or changed — it belongs to
    // the spawning fork, not this thread reply.
    let topic = state.topic_sessions.lock().unwrap();
    assert_eq!(
        topic.get(thread_parent_id).map(String::as_str),
        Some("pending"),
        "pending sentinel should be untouched by a thread reply"
    );
}

/// Verify the DmFromUser wake reason encodes the correct reply instruction
/// for a channel.post to a dm- channel.
#[test]
fn test_dm_from_user_wake_reason_reply_instruction() {
    let reason = crate::daemon::wake_reason::WakeReason::DmFromUser {
        content: "Can you look at the tests?".to_string(),
        msg_id: "msg-dm-xyz".to_string(),
        coworker_name: "amsterdam".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("msg-dm-xyz"), "should include msg_id");
    assert!(
        msg.contains("Can you look at the tests?"),
        "should include message content"
    );
    assert!(
        msg.contains("--channel dm-amsterdam"),
        "reply instruction should reference the coworker's DM channel"
    );
    assert!(
        msg.contains("midtown channel post"),
        "reply instruction should include the midtown command"
    );
}

#[test]
fn test_fork_initial_framing_contains_channel_name() {
    let framing = fork_initial_framing("proj-auth");
    assert!(
        framing.contains("#proj-auth"),
        "Fork framing should reference the channel: {framing}"
    );
}

#[test]
fn test_fork_initial_framing_mentions_no_code() {
    let framing = fork_initial_framing("web");
    assert!(
        framing.contains("do NOT write code") || framing.contains("You do NOT write code"),
        "Fork framing should tell the fork not to write code: {framing}"
    );
}

#[test]
fn test_fork_initial_framing_mentions_task_creation() {
    let framing = fork_initial_framing("web");
    assert!(
        framing.contains("midtown task create"),
        "Fork framing should mention task creation: {framing}"
    );
}
