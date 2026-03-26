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
    let (session_agg_tx, _session_agg_rx) = crate::daemon::session_events::channel();
    let state = DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
        session_agg_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

/// Insert a minimal session record into persistent state for testing.
async fn insert_test_session(state: &DaemonState, session_id: &str, name: &str) {
    let mut ps = state.persistent_state.lock().await;
    ps.sessions.insert(
        session_id.to_string(),
        crate::daemon::state::SessionRecord {
            session_id: session_id.to_string(),
            name: name.to_string(),
            is_running: true,
            ..Default::default()
        },
    );
}

/// Post a parent message and return its ID for use in thread reply tests.
async fn post_parent_message(state: &DaemonState, channel: Option<&str>) -> String {
    let response = handle_channel_post(
        999_i64.into(),
        "setup",
        "Parent message for thread tests",
        channel,
        None,
        state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "parent message post should succeed"
    );
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
    let ch = state.channel_router.get_channel(channel_name).unwrap();
    let messages = ch.read_all().unwrap();
    messages.last().unwrap().id.clone()
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
    // \t gets converted to tab, but other backslash sequences like \f are preserved
    assert_eq!(
        unescape_shell_artifacts("path\\ffile and \\!"),
        "path\\ffile and !"
    );
}

#[test]
fn test_unescape_shell_artifacts_newline() {
    assert_eq!(
        unescape_shell_artifacts("line one\\n\\nline two"),
        "line one\n\nline two"
    );
}

#[test]
fn test_unescape_shell_artifacts_tab() {
    assert_eq!(
        unescape_shell_artifacts("col1\\tcol2\\tcol3"),
        "col1\tcol2\tcol3"
    );
}

#[test]
fn test_unescape_shell_artifacts_mixed_escapes() {
    assert_eq!(
        unescape_shell_artifacts("Hello\\! Here's the update:\\n\\n- item one\\n- item two"),
        "Hello! Here's the update:\n\n- item one\n- item two"
    );
}

#[test]
fn test_unescape_shell_artifacts_preserves_backtick_content() {
    // Literal \n inside backticks should be preserved
    assert_eq!(
        unescape_shell_artifacts("Use `\\n` for newlines"),
        "Use `\\n` for newlines"
    );
}

#[test]
fn test_unescape_shell_artifacts_preserves_code_block_content() {
    // Literal \n inside code blocks should be preserved
    assert_eq!(
        unescape_shell_artifacts("Example:\\n```\\ncode\\n```\\nDone"),
        "Example:\n```\\ncode\\n```\nDone"
    );
}

#[test]
fn test_unescape_shell_artifacts_preserves_inline_code_with_tabs() {
    assert_eq!(
        unescape_shell_artifacts("Use `\\t` for tabs\\nin your code"),
        "Use `\\t` for tabs\nin your code"
    );
}

#[test]
fn test_unescape_shell_artifacts_unclosed_backtick() {
    // Known limitation: an unclosed backtick causes `in_inline_code` to stay true
    // for the remainder of the string, suppressing escape conversions. This is
    // acceptable because coworker messages are generated by Claude Code, which
    // produces well-formed markdown. Informal text with odd backticks is rare.
    assert_eq!(
        unescape_shell_artifacts("it's a `shortcut\\nand more\\ntext"),
        "it's a `shortcut\\nand more\\ntext" // escapes preserved after unclosed backtick
    );
}

#[test]
fn test_unescape_shell_artifacts_regex_content_outside_code() {
    // \n in technical content outside code spans IS converted. This is a known
    // trade-off: the function optimizes for the common case (coworker status
    // messages with \n for line breaks) over the rare case (regex discussion
    // outside backticks). Users discussing regex should use backtick code spans.
    assert_eq!(
        unescape_shell_artifacts("Regex: \\n+ matches one or more"),
        "Regex: \n+ matches one or more"
    );
}

#[test]
fn test_unescape_shell_artifacts_path_with_tabs() {
    // \t in paths outside code spans IS converted to a tab. Document this
    // explicitly — paths with \t are uncommon in channel messages, and the
    // conversion is consistent with the function's purpose.
    assert_eq!(
        unescape_shell_artifacts("path\\to\\file"),
        "path\to\\file" // \t converted, \f preserved
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "nudge_text");
    assert!(
        messages[0].text.starts_with("user (channel-msg-id: ")
            && messages[0].text.ends_with("): please check this"),
        "nudge text should be 'user (channel-msg-id: <id>): please check this', got: {}",
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
            &state.project_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    let msg = format!("@{} please ack", state.project_name);
    let response = handle_channel_post(1_i64.into(), "user", &msg, None, None, &state).await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");

    assert_eq!(
        messages.len(),
        1,
        "user @project message should nudge lead exactly once"
    );
    assert_eq!(messages[0].kind, "nudge_text");
    assert!(
        messages[0].text.starts_with("user (channel-msg-id: "),
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Main lead should be nudged for main channel user messages"
    );
    assert!(
        messages[0].text.starts_with("user (channel-msg-id: ")
            && messages[0].text.ends_with("): hello main"),
        "nudge text should be 'user (channel-msg-id: <id>): hello main', got: {}",
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(
        messages.len(),
        1,
        "Lead should be nudged even when user @mentions a coworker"
    );
    assert!(
        messages[0].text.starts_with("user (channel-msg-id: ")
            && messages[0].text.ends_with("): @york can you check this?"),
        "nudge text should be 'user (channel-msg-id: <id>): @york can you check this?', got: {}",
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
            &state.project_name,
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
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
    let response = handle_channel_read(
        999.into(),
        true,
        None,
        None,
        Some("auth"),
        None,
        None,
        None,
        &state,
    )
    .await;

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
    let parent_id = post_parent_message(&state, None).await;

    // Post a thread reply
    let response = handle_channel_post(
        1_i64.into(),
        "york",
        "This is a reply in a thread",
        None,
        Some(&parent_id),
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // Read back messages and verify thread_parent_id is set
    let channel = state.channel_router.default_channel().unwrap();
    let messages = channel.read_all().unwrap();
    assert_eq!(messages.len(), 2); // parent + reply
    assert_eq!(
        messages[1].thread_parent_id,
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

    // Post a top-level message and get its ID for use as thread parent
    let parent_id = post_parent_message(&state, None).await;

    // Post a thread reply referencing the real parent
    let _r = handle_channel_post(
        2_i64.into(),
        "york",
        "Thread reply",
        None,
        Some(&parent_id),
        &state,
    )
    .await;

    let response =
        handle_channel_read(999.into(), true, None, None, None, None, None, None, &state).await;

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
        Some(parent_id.as_str()),
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
    let response = handle_channel_read(
        999.into(),
        false,
        Some(3),
        None,
        None,
        None,
        None,
        None,
        &state,
    )
    .await;

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
            &state.project_name,
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
            &state.project_name,
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
                name: "auth-refactor".to_string(),
                agent_type: "midtown-channel-lead".to_string(),
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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
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
            &state.project_name,
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
            &state.project_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Post a parent message to get a valid thread_parent_id
    let parent_id = post_parent_message(&state, None).await;

    // Drain the headed queue of any nudges from the parent post
    let _ = state
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await;

    // Post a user thread reply (simulating the user sending a reply from the thread panel)
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "this is my thread reply",
        None,
        Some(&parent_id),
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let (messages, _capture) = state
        .headed_poll(&state.project_name, adapter_id, 0, 10)
        .await
        .expect("poll headed queue");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "nudge_text");

    // The nudge MUST use the parent's ID so the lead replies with --thread parent_id,
    // creating a sibling reply in the correct thread. Using the reply's own UUID
    // would cause the lead to create a nested reply invisible to the user.
    // The nudge also includes --thread/--channel instructions after the message preview.
    assert!(
        messages[0]
            .text
            .contains(&format!("user (channel-msg-id: {})", parent_id)),
        "nudge for thread reply should use parent_id, got: {}",
        messages[0].text
    );
    assert!(
        messages[0].text.contains("this is my thread reply"),
        "nudge should contain the message content"
    );
    // Thread reply nudge should include thread instructions
    assert!(
        messages[0].text.contains("--thread"),
        "thread reply nudge should include --thread instruction, got: {}",
        messages[0].text
    );
}

/// Verify that `clear_lead_respawn_cooldown` removes the lead entry from
/// `coworker_stop_times`, allowing `ensure_lead_alive()` to respawn on the next tick.
#[tokio::test]
async fn test_clear_lead_respawn_cooldown_removes_stop_time() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-clear-lead-cooldown");

    // After rename, lead stop times are keyed by repo_name (lowercase), not "lead"
    let lead_key = state.project_name.to_lowercase();

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

        let lead_key = state.project_name.to_lowercase();
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
    let lead_key = state.project_name.to_lowercase();
    state
        .headed_register(
            &state.project_name,
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

/// Verify that a user message on a topic channel while the channel lead is
/// dead clears the channel lead's respawn cooldown (stop time removed),
/// matching the main lead's expedite pattern.
#[tokio::test]
async fn test_user_message_in_topic_channel_clears_channel_lead_cooldown() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-channel-lead-cooldown");

    let channel_name = "ops";

    // Register the channel lead in persistent state so the daemon knows about it
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_name.to_string(), "sess-ops-123".to_string());
        ps.save_for_repo(state.paths.dir_key()).unwrap();
    }

    // Simulate channel lead having been stopped 2 minutes ago (within 5-min cooldown)
    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(
            channel_name.to_string(),
            chrono::Utc::now() - chrono::Duration::minutes(2),
        );
    }

    // User posts in the topic channel — the channel lead is dead
    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "hello ops channel",
        Some(channel_name),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    // The channel lead's cooldown should have been cleared
    let stop_times = state.coworker_stop_times.read().unwrap();
    assert!(
        !stop_times.contains_key(channel_name),
        "Channel lead stop time should be cleared after user message with dead channel lead"
    );
}

/// A channel lead with a live process handle but a usage-limit screen is not
/// actually able to respond. User messages must still clear its respawn
/// cooldown so recovery can happen immediately.
#[tokio::test]
async fn test_user_message_in_topic_channel_usage_limited_lead_clears_channel_lead_cooldown() {
    use super::super::sessions::SessionStatus;

    let (state, _tmp, _guard) =
        make_test_state("midtown-test-topic-channel-lead-usage-limit-cooldown");

    let channel_name = "ops";

    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_name.to_string(), "sess-ops-usage-limit".to_string());
        ps.save_for_repo(state.paths.dir_key()).unwrap();
    }

    state
        .session_manager
        .insert_test_session(channel_name, SessionStatus::Running)
        .await;
    state
        .session_manager
        .set_test_session_health_flags(channel_name, true, false, false)
        .await;
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(move |name: &str| {
            name.eq_ignore_ascii_case(channel_name)
        })));

    {
        let mut stop_times = state.coworker_stop_times.write().unwrap();
        stop_times.insert(
            channel_name.to_string(),
            chrono::Utc::now() - chrono::Duration::minutes(2),
        );
    }

    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "hello ops channel",
        Some(channel_name),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    let stop_times = state.coworker_stop_times.read().unwrap();
    assert!(
        !stop_times.contains_key(channel_name),
        "Usage-limited channel lead should be treated as dead for cooldown clearing"
    );
}

/// If the stored channel lead session is usage-limited, channel.post must not
/// treat a direct send as success — it should skip the stale session and fall
/// through to resume/spawn recovery logic instead.
#[tokio::test]
async fn test_user_message_to_topic_channel_skips_direct_nudge_for_usage_limited_lead() {
    use super::super::sessions::SessionStatus;

    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-lead-usage-limit-nudge");
    let channel_name = "ops";
    let send_attempts = std::sync::Arc::new(std::sync::Mutex::new(0usize));

    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_name.to_string(), "sess-ops-usage-limit".to_string());
        ps.save_for_repo(state.paths.dir_key()).unwrap();
    }

    state
        .session_manager
        .insert_test_session(channel_name, SessionStatus::Running)
        .await;
    state
        .session_manager
        .set_test_session_health_flags(channel_name, true, false, false)
        .await;
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(move |name: &str| {
            name.eq_ignore_ascii_case(channel_name)
        })));

    {
        let send_attempts = send_attempts.clone();
        state
            .session_manager
            .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(
                move |_session_id: &str, _message: &str| {
                    *send_attempts.lock().unwrap() += 1;
                    Ok(())
                },
            )));
    }

    let response = handle_channel_post(
        1_i64.into(),
        "user",
        "need ops help",
        Some(channel_name),
        None,
        &state,
    )
    .await;
    assert!(response.error.is_none(), "channel.post should succeed");

    assert_eq!(
        *send_attempts.lock().unwrap(),
        0,
        "Usage-limited channel lead should not receive a direct nudge"
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
                name: "feature-x".to_string(),
                agent_type: "midtown-channel-lead".to_string(),
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

    // Seed persistent state: channel_lead_sessions, sessions
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert("auth-v1".to_string(), "session-auth-123".to_string());
        ps.sessions.insert(
            "session-auth-123".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "session-auth-123".to_string(),
                name: old_lead_session_name.clone(),
                agent_type: "midtown-channel-lead".to_string(),
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

// Avenue name rename rejection test removed — with task-based naming,
// avenue names are no longer reserved.

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
            &state.project_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Register "amsterdam" as an active coworker via session record
    let coworker_session_id = "session-amsterdam-xyz";
    insert_test_session(&state, coworker_session_id, "amsterdam").await;

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
        .headed_poll(&state.project_name, adapter_id, 0, 10)
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
            &state.project_name,
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

// ============================================================================
// Thread ID validation tests
// ============================================================================

/// Posting a thread reply with a non-existent thread_parent_id should return
/// an error (-32602), preventing "black hole" messages.
#[tokio::test]
async fn test_channel_post_rejects_invalid_thread_parent_id() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-invalid-thread-parent-id");

    let response = handle_channel_post(
        1_i64.into(),
        "york",
        "Reply to nonexistent thread",
        None,
        Some("nonexistent-parent-uuid"),
        &state,
    )
    .await;
    assert!(
        response.error.is_some(),
        "channel.post with invalid thread_parent_id should return an error"
    );
    let err = response.error.unwrap();
    assert_eq!(err.code, -32602, "error code should be -32602");
    assert!(
        err.message.contains("nonexistent-parent-uuid"),
        "error should mention the invalid thread_parent_id, got: {}",
        err.message
    );
}

/// Thread validation also works for topic channels.
#[tokio::test]
async fn test_channel_post_rejects_invalid_thread_parent_id_topic_channel() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-invalid-thread-topic");

    let response = handle_channel_post(
        1_i64.into(),
        "york",
        "Reply to nonexistent thread in topic channel",
        Some("auth-refactor"),
        Some("nonexistent-parent-uuid"),
        &state,
    )
    .await;
    assert!(
        response.error.is_some(),
        "channel.post with invalid thread_parent_id in topic channel should return an error"
    );
    let err = response.error.unwrap();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("nonexistent-parent-uuid"));
}

/// build_topic_thread_nudge_effect passes thread context to the WakeReason.
#[test]
fn test_build_topic_thread_nudge_effect_thread_context() {
    let effect = super::build_topic_thread_nudge_effect(
        "auth-refactor",
        "user question",
        "wake-msg-id".to_string(),
        None,
        Some("parent-id-123"),
    );
    match effect {
        crate::daemon::effects::Effect::NudgeChannelLead { reason, .. } => match reason {
            crate::daemon::wake_reason::WakeReason::UserMessage { thread_ctx, .. } => {
                let ctx = thread_ctx.expect("thread_ctx should be Some when parent ID is provided");
                assert_eq!(
                    ctx.parent_id, "parent-id-123",
                    "thread_ctx.parent_id should be passed through"
                );
                assert_eq!(
                    ctx.channel_name, "auth-refactor",
                    "thread_ctx.channel_name should be set from the channel"
                );
            }
            other => panic!("Expected UserMessage reason, got {:?}", other),
        },
        other => panic!("Expected NudgeChannelLead effect, got {:?}", other),
    }
}
// ============================================================================
// Channel lead @mention routing in topic channels
// ============================================================================

/// When a channel lead posts `@coworker !N ...` in a topic channel, the
/// @mention should be routed to the mentioned coworker (producing a nudge).
///
/// Regression test for !2187: previously, `route_mentions()` was only called
/// for main-channel user messages. Topic channel messages from channel leads
/// skipped @mention routing entirely, so coworkers never received the nudge.
#[tokio::test]
async fn test_channel_lead_mention_in_topic_channel_routes_to_coworker() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-topic-mention-routing");

    // Register a running coworker "amsterdam" so route_mentions can find it
    state
        .coworkers
        .insert_for_testing(crate::coworker::Coworker {
            slot_id: "slot-amsterdam".to_string(),
            name: "amsterdam".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: Some("sess-amsterdam-1".to_string()),
            provider: crate::auth::AuthProvider::Claude,
            model: String::new(),
            profile: String::new(),
        });

    // Set up session record so NudgeSession can resolve the coworker
    insert_test_session(&state, "sess-amsterdam-1", "amsterdam").await;

    // Channel lead "ops" posts a message with @amsterdam mention in the "ops" topic channel
    let response = handle_channel_post(
        1_i64.into(),
        "ops",                                     // channel lead sender
        "@amsterdam !9 keep the condition guards", // message with @mention
        Some("ops"),                               // topic channel
        None,                                      // no thread parent
        &state,
    )
    .await;
    assert!(
        response.error.is_none(),
        "channel.post should succeed: {:?}",
        response.error
    );

    // Retrieve the message ID from the channel to check cooldown dedup records
    let ch = state.channel_router.get_channel("ops").unwrap();
    let messages = ch.read_all().unwrap();
    let posted_msg = messages
        .last()
        .expect("channel should contain the posted message");

    // Verify that route_mentions was called: the cooldown tracker should have
    // recorded a `chat_mention_amsterdam` entry for this message ID, blocking
    // a duplicate nudge.
    let was_mention_routed = {
        let cooldowns = state.cooldowns.lock().unwrap();
        // check() returns false when a recent record exists (i.e., it was routed)
        !cooldowns.check(
            "chat_mention_amsterdam",
            &posted_msg.id,
            std::time::Duration::from_secs(3600),
        )
    };
    assert!(
        was_mention_routed,
        "route_mentions should have been called for @amsterdam in topic channel message"
    );
}

/// Protected senders (SKIP_SENDERS: "system", "midtown", "github") should NOT
/// trigger route_mentions in topic channels, consistent with chat_monitor_loop.
#[tokio::test]
async fn test_skip_senders_do_not_route_mentions_in_topic_channels() {
    for sender in &["system", "midtown", "github"] {
        let (state, _tmp, _guard) =
            make_test_state(&format!("midtown-test-skip-sender-{}", sender));

        // Register a running coworker "amsterdam" so route_mentions could find it
        state
            .coworkers
            .insert_for_testing(crate::coworker::Coworker {
                slot_id: "slot-amsterdam".to_string(),
                name: "amsterdam".to_string(),
                status: crate::coworker::CoworkerStatus::Running,
                working_dir: "/tmp/test".to_string(),
                started_at: chrono::Utc::now(),
                current_task: None,
                session_id: Some("sess-amsterdam-1".to_string()),
                provider: crate::auth::AuthProvider::Claude,
                model: String::new(),
                profile: String::new(),
            });

        insert_test_session(&state, "sess-amsterdam-1", "amsterdam").await;

        // Protected sender posts a message with @amsterdam mention in a topic channel
        let response = handle_channel_post(
            1_i64.into(),
            sender,
            "@amsterdam check this out",
            Some("ops"),
            None,
            &state,
        )
        .await;
        assert!(
            response.error.is_none(),
            "channel.post should succeed for sender {}: {:?}",
            sender,
            response.error
        );

        // Verify route_mentions was NOT called: cooldown tracker should have
        // no entry for chat_mention_amsterdam.
        let ch = state.channel_router.get_channel("ops").unwrap();
        let messages = ch.read_all().unwrap();
        let posted_msg = messages
            .last()
            .expect("channel should contain the posted message");

        let was_mention_routed = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check(
                "chat_mention_amsterdam",
                &posted_msg.id,
                std::time::Duration::from_secs(3600),
            )
        };
        assert!(
            !was_mention_routed,
            "route_mentions should NOT have been called for SKIP_SENDER '{}' in topic channel",
            sender
        );
    }
}

/// Verify that channel.read with a `thread` parameter returns only messages
/// belonging to that thread (the parent message + its replies).
#[tokio::test]
async fn test_channel_read_with_thread_filter() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-read-thread");

    // Post a parent message
    let parent_id = post_parent_message(&state, None).await;

    // Post a thread reply
    let _r = handle_channel_post(
        1_i64.into(),
        "york",
        "Thread reply 1",
        None,
        Some(&parent_id),
        &state,
    )
    .await;

    // Post a top-level message (NOT in the thread)
    let _r = handle_channel_post(
        2_i64.into(),
        "park",
        "Top-level unrelated",
        None,
        None,
        &state,
    )
    .await;

    // Post another thread reply
    let _r = handle_channel_post(
        3_i64.into(),
        "york",
        "Thread reply 2",
        None,
        Some(&parent_id),
        &state,
    )
    .await;

    // Read with --thread filter: should return parent + 2 replies = 3 messages
    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        Some(&parent_id),
        None,
        None,
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 messages (parent + 2 replies), got {}",
        messages.len()
    );

    // Verify the messages are the parent and thread replies (not the unrelated top-level)
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("Parent message"),
        "First message should be the parent"
    );
    assert!(
        messages[1]["message"]
            .as_str()
            .unwrap()
            .contains("Thread reply 1"),
        "Second message should be thread reply 1"
    );
    assert!(
        messages[2]["message"]
            .as_str()
            .unwrap()
            .contains("Thread reply 2"),
        "Third message should be thread reply 2"
    );
}

/// Verify that channel.read with --thread and --last correctly limits the
/// number of thread messages returned.
#[tokio::test]
async fn test_channel_read_thread_with_last() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-channel-read-thread-last");

    let parent_id = post_parent_message(&state, None).await;

    // Post 5 thread replies
    for i in 1..=5 {
        let _r = handle_channel_post(
            i.into(),
            "york",
            &format!("Thread reply {i}"),
            None,
            Some(&parent_id),
            &state,
        )
        .await;
    }

    // Read thread with --last 3: should return last 3 of the thread (parent + 5 replies = 6 total)
    let response = handle_channel_read(
        999.into(),
        false,
        Some(3),
        None,
        None,
        Some(&parent_id),
        None,
        None,
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 messages with --last 3, got {}",
        messages.len()
    );
}

/// Regression: --thread + --last must filter by thread BEFORE truncating.
/// Previously, --last N took the last N messages from the whole channel and
/// then filtered by thread, returning zero results when unrelated traffic
/// exceeded the window.
#[tokio::test]
async fn test_channel_read_thread_with_last_filters_before_truncating() {
    let (state, _tmp, _guard) =
        make_test_state("midtown-test-channel-read-thread-last-before-trunc");

    let parent_id = post_parent_message(&state, None).await;

    // Post 2 thread replies
    for i in 1..=2 {
        let _r = handle_channel_post(
            i.into(),
            "york",
            &format!("Thread reply {i}"),
            None,
            Some(&parent_id),
            &state,
        )
        .await;
    }

    // Flood the channel with 30 unrelated top-level messages (more than --last 10)
    for i in 1..=30 {
        let _r = handle_channel_post(
            (100 + i).into(),
            "park",
            &format!("Unrelated noise {i}"),
            None,
            None,
            &state,
        )
        .await;
    }

    // Read with --last 10 --thread: should return all 3 thread messages
    // (parent + 2 replies), NOT zero because the 30 noise messages pushed
    // thread messages out of the last-10 window.
    let response = handle_channel_read(
        999.into(),
        false,
        Some(10),
        None,
        None,
        Some(&parent_id),
        None,
        None,
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 thread messages (parent + 2 replies) even with --last 10, got {}",
        messages.len()
    );
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("Parent message"),
        "First message should be the parent"
    );
}

/// Helper: post a message and return its ID.
async fn post_and_get_id(
    state: &DaemonState,
    from: &str,
    content: &str,
    channel: Option<&str>,
    thread_parent_id: Option<&str>,
) -> String {
    let _r = handle_channel_post(
        999_i64.into(),
        from,
        content,
        channel,
        thread_parent_id,
        state,
    )
    .await;
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
    let ch = state.channel_router.get_channel(channel_name).unwrap();
    let messages = ch.read_all().unwrap();
    messages.last().unwrap().id.clone()
}

#[tokio::test]
async fn test_channel_read_message_returns_single_message() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-single");

    let _id1 = post_and_get_id(&state, "alice", "Message 1", None, None).await;
    let id2 = post_and_get_id(&state, "bob", "Message 2", None, None).await;
    let _id3 = post_and_get_id(&state, "carol", "Message 3", None, None).await;

    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some(&id2),
        None,
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "Expected 1 message");
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("Message 2"),
        "Should return the requested message"
    );
}

#[tokio::test]
async fn test_channel_read_message_not_found() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-not-found");

    let _id1 = post_and_get_id(&state, "alice", "Message 1", None, None).await;

    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some("nonexistent-uuid"),
        None,
        &state,
    )
    .await;

    assert!(
        response.error.is_some(),
        "Should return error for nonexistent message"
    );
}

#[tokio::test]
async fn test_channel_read_message_with_context_top_level() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-ctx-top");

    let _id1 = post_and_get_id(&state, "alice", "Msg 1", None, None).await;
    let _id2 = post_and_get_id(&state, "bob", "Msg 2", None, None).await;
    let id3 = post_and_get_id(&state, "carol", "Msg 3", None, None).await;
    let _id4 = post_and_get_id(&state, "dave", "Msg 4", None, None).await;
    let _id5 = post_and_get_id(&state, "eve", "Msg 5", None, None).await;

    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some(&id3),
        Some(1),
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 messages (1 before + target + 1 after)"
    );
    assert!(messages[0]["message"].as_str().unwrap().contains("Msg 2"));
    assert!(messages[1]["message"].as_str().unwrap().contains("Msg 3"));
    assert!(messages[2]["message"].as_str().unwrap().contains("Msg 4"));
}

#[tokio::test]
async fn test_channel_read_message_with_context_thread_reply() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-ctx-thread-reply");

    let parent_id = post_parent_message(&state, None).await;

    // Post 5 thread replies
    let mut reply_ids = Vec::new();
    for i in 1..=5 {
        let id = post_and_get_id(
            &state,
            "york",
            &format!("Reply {i}"),
            None,
            Some(&parent_id),
        )
        .await;
        reply_ids.push(id);
    }

    // Read reply 3 (index 2) with context=1
    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some(&reply_ids[2]),
        Some(1),
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Expected 3 messages (reply 2, reply 3, reply 4)"
    );
    assert!(messages[0]["message"].as_str().unwrap().contains("Reply 2"));
    assert!(messages[1]["message"].as_str().unwrap().contains("Reply 3"));
    assert!(messages[2]["message"].as_str().unwrap().contains("Reply 4"));
}

#[tokio::test]
async fn test_channel_read_message_with_context_thread_parent() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-ctx-thread-parent");

    // Post 3 channel messages before the thread parent
    let _id1 = post_and_get_id(&state, "alice", "Before 1", None, None).await;
    let _id2 = post_and_get_id(&state, "bob", "Before 2", None, None).await;
    let _id3 = post_and_get_id(&state, "carol", "Before 3", None, None).await;

    // Post the thread parent
    let parent_id = post_and_get_id(&state, "dave", "Thread parent", None, None).await;

    // Post 3 thread replies
    for i in 1..=3 {
        let _r = post_and_get_id(
            &state,
            "eve",
            &format!("Thread reply {i}"),
            None,
            Some(&parent_id),
        )
        .await;
    }

    // Read the parent with context=2
    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some(&parent_id),
        Some(2),
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    // Should be: Before 2, Before 3, Thread parent, Thread reply 1, Thread reply 2
    assert_eq!(
        messages.len(),
        5,
        "Expected 5 messages (2 before + parent + 2 replies)"
    );
    assert!(
        messages[0]["message"]
            .as_str()
            .unwrap()
            .contains("Before 2")
    );
    assert!(
        messages[1]["message"]
            .as_str()
            .unwrap()
            .contains("Before 3")
    );
    assert!(
        messages[2]["message"]
            .as_str()
            .unwrap()
            .contains("Thread parent")
    );
    assert!(
        messages[3]["message"]
            .as_str()
            .unwrap()
            .contains("Thread reply 1")
    );
    assert!(
        messages[4]["message"]
            .as_str()
            .unwrap()
            .contains("Thread reply 2")
    );
}

#[tokio::test]
async fn test_channel_read_message_with_context_at_edges() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-read-message-ctx-edges");

    let id1 = post_and_get_id(&state, "alice", "Msg 1", None, None).await;
    let _id2 = post_and_get_id(&state, "bob", "Msg 2", None, None).await;
    let _id3 = post_and_get_id(&state, "carol", "Msg 3", None, None).await;

    // Read the first message with context=5 (more than available)
    let response = handle_channel_read(
        999.into(),
        false,
        None,
        None,
        None,
        None,
        Some(&id1),
        Some(5),
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Should return all 3 messages without panic"
    );
    assert!(messages[0]["message"].as_str().unwrap().contains("Msg 1"));
    assert!(messages[1]["message"].as_str().unwrap().contains("Msg 2"));
    assert!(messages[2]["message"].as_str().unwrap().contains("Msg 3"));
}

#[tokio::test]
async fn test_channel_read_includes_reply_count_for_top_level_messages() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-reply-count");

    // Post a top-level message
    let parent_id = post_and_get_id(&state, "alice", "Thread starter", None, None).await;
    // Post another top-level message (no replies)
    let _no_replies_id = post_and_get_id(&state, "bob", "No replies here", None, None).await;

    // Post 3 thread replies to alice's message
    for i in 1..=3 {
        let _reply_id = post_and_get_id(
            &state,
            "carol",
            &format!("Reply {}", i),
            None,
            Some(&parent_id),
        )
        .await;
    }

    // Read all channel messages (not in a thread)
    let response =
        handle_channel_read(999.into(), true, None, None, None, None, None, None, &state).await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();

    // Find the parent message and check reply_count
    let parent_msg = messages
        .iter()
        .find(|m| m["id"].as_str() == Some(&parent_id))
        .expect("Parent message should be in results");
    assert_eq!(
        parent_msg["reply_count"].as_u64(),
        Some(3),
        "Parent message should have reply_count=3"
    );

    // The no-replies message should not have reply_count
    let no_replies_msg = messages
        .iter()
        .find(|m| m["message"].as_str() == Some("No replies here"))
        .expect("No-replies message should be in results");
    assert!(
        no_replies_msg.get("reply_count").is_none() || no_replies_msg["reply_count"].is_null(),
        "Message without replies should not have reply_count"
    );
}

#[tokio::test]
async fn test_channel_read_thread_omits_reply_count() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-reply-count-thread");

    let parent_id = post_and_get_id(&state, "alice", "Thread parent", None, None).await;
    let _reply_id = post_and_get_id(&state, "bob", "A reply", None, Some(&parent_id)).await;

    // Read the thread — reply_count should not be present since we're reading a thread
    let response = handle_channel_read(
        999.into(),
        true,
        None,
        None,
        None,
        Some(&parent_id),
        None,
        None,
        &state,
    )
    .await;

    assert!(response.error.is_none(), "channel.read should succeed");
    let result = response.result.unwrap();
    let messages = result["messages"].as_array().unwrap();

    // Thread reads should not include reply_count
    for msg in messages {
        assert!(
            msg.get("reply_count").is_none() || msg["reply_count"].is_null(),
            "Thread read should not include reply_count"
        );
    }
}

/// When a user replies to a dead fork's thread, the resume path should use
/// spawn_with_resume_fallback (which tries resume then falls back to fresh)
/// instead of bare spawn_coworker (which has no fallback).
///
/// In test env, spawn will fail either way, but the code path is exercised and
/// the message routes gracefully (falls through to channel lead nudge).
#[tokio::test]
async fn test_dead_fork_thread_reply_uses_resume_fallback() {
    let (state, _tmp, _guard) = make_test_state("midtown-test-dead-fork-resume-fallback");
    let adapter_id = "test-adapter-dead-fork-resume";
    state
        .headed_register(
            &state.project_name,
            adapter_id,
            crate::auth::AuthProvider::Claude,
        )
        .await
        .expect("register headed adapter");

    // Post a parent message to create a valid thread anchor
    let parent_id = post_parent_message(&state, None).await;

    // Insert a dead fork session bound to that thread
    let fork_sid = "dead-fork-session-123";
    let fork_name = "test-fork";
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            fork_sid.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: fork_sid.to_string(),
                name: fork_name.to_string(),
                is_running: false, // dead fork
                bound_thread_id: Some(parent_id.clone()),
                agent_type: "midtown-channel-lead".to_string(),
                initial_prompt: Some("investigate the auth issue".to_string()),
                working_dir: _tmp.path().to_string_lossy().to_string(),
                ..Default::default()
            },
        );
    }

    // Post a thread reply to the dead fork's thread — this triggers the resume path
    let response = handle_channel_post(
        999_i64.into(),
        "user",
        "any update on this?",
        None,
        Some(&parent_id),
        &state,
    )
    .await;

    // The message should be accepted without error (spawn fails but is handled gracefully)
    assert!(
        response.error.is_none(),
        "channel.post to dead fork thread should not return error, got: {:?}",
        response.error
    );
}
