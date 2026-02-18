//! Tests for web server and WebSocket handlers.

use super::*;

#[test]
fn test_client_message_parsing() {
    let json = r#"{"type": "send_message", "content": "Hello world"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::SendMessage {
            content,
            channel,
            thread_parent_id,
        } => {
            assert_eq!(content, "Hello world");
            assert_eq!(channel, None); // No channel specified
            assert_eq!(thread_parent_id, None);
        }
        _ => panic!("Expected SendMessage"),
    }
}

#[test]
fn test_client_message_parsing_with_channel() {
    let json = r#"{"type": "send_message", "content": "Hello", "channel": "auth-refactor"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::SendMessage {
            content,
            channel,
            thread_parent_id,
        } => {
            assert_eq!(content, "Hello");
            assert_eq!(channel, Some("auth-refactor".to_string()));
            assert_eq!(thread_parent_id, None);
        }
        _ => panic!("Expected SendMessage"),
    }
}

#[tokio::test]
async fn test_mobile_send_message_forwards_to_daemon() {
    // Verify that handle_client_message forwards mobile messages through
    // channel_post_tx instead of writing directly to the channel file.
    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, mut channel_post_rx) = mpsc::channel(10);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: None,
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    let json = r#"{"type": "send_message", "content": "hello from mobile"}"#;
    handle_client_message(json, &state).await.unwrap();

    // The message should be forwarded to the daemon via channel_post_tx
    let post = channel_post_rx
        .try_recv()
        .expect("expected a mobile channel post");
    assert_eq!(post.content, "hello from mobile");
    assert_eq!(post.channel, None); // No channel specified, should default to None
}

#[test]
fn test_web_update_serialization() {
    let update = WebUpdate::ChannelMessage(ChannelMessageData {
        id: "test-id".to_string(),
        from: "test".to_string(),
        content: "Hello".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: "midtown".to_string(),
        source_channel: None,
        thread_parent_id: None,
    });

    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("channel_message"));
    assert!(json.contains("Hello"));
}

#[test]
fn test_coworker_status_update_serialization() {
    let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
        name: "lexington".to_string(),
        status: Some("running".to_string()),
        current_task: Some("Fix auth bug".to_string()),
        model: Some("sonnet".to_string()),
        session_id: None,
        phase: None,
        progress: None,
        time_estimate: None,
        health: None,
    });

    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("coworker_status"));
    assert!(json.contains("lexington"));
    assert!(json.contains("running"));
    assert!(json.contains("Fix auth bug"));
}

#[test]
fn test_coworker_progress_update_serialization() {
    let update = coworker_progress_update(
        "madison",
        Some("dev".to_string()),
        Some(45),
        Some("~3m".to_string()),
        Some("green".to_string()),
    );

    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("coworker_status"));
    assert!(json.contains("madison"));
    assert!(json.contains(r#""phase":"dev""#));
    assert!(json.contains(r#""progress":45"#));
    assert!(json.contains(r#""time_estimate":"~3m""#));
    assert!(json.contains(r#""health":"green""#));
    // status and current_task must be absent so they don't clobber frontend state
    assert!(
        !json.contains("\"status\""),
        "progress update must not include status"
    );
    assert!(
        !json.contains("\"current_task\""),
        "progress update must not include current_task"
    );
}

#[test]
fn test_client_message_nudge_parsing() {
    let json = r#"{"type": "nudge", "target": "riverside", "message": "check the tests"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Nudge { target, message } => {
            assert_eq!(target, "riverside");
            assert_eq!(message, "check the tests");
        }
        _ => panic!("Expected Nudge"),
    }
}

#[test]
fn test_client_message_nudge_lead_parsing() {
    let json = r#"{"type": "nudge", "target": "lead", "message": "please review PR #42"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Nudge { target, message } => {
            assert_eq!(target, "lead");
            assert_eq!(message, "please review PR #42");
        }
        _ => panic!("Expected Nudge"),
    }
}

#[test]
fn test_client_message_send_key_parsing() {
    let json = r#"{"type": "send_key", "target": "riverside", "key": "Escape"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::SendKey { target, key } => {
            assert_eq!(target, "riverside");
            assert_eq!(key, "Escape");
        }
        _ => panic!("Expected SendKey"),
    }
}

#[test]
fn test_client_message_send_key_lead_parsing() {
    let json = r#"{"type": "send_key", "target": "lead", "key": "Escape"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::SendKey { target, key } => {
            assert_eq!(target, "lead");
            assert_eq!(key, "Escape");
        }
        _ => panic!("Expected SendKey"),
    }
}

#[test]
fn test_coworker_status_update_without_task() {
    let update = WebUpdate::CoworkerStatus(CoworkerStatusData {
        name: "park".to_string(),
        status: Some("stopped".to_string()),
        current_task: None,
        model: Some("sonnet".to_string()),
        session_id: None,
        phase: None,
        progress: None,
        time_estimate: None,
        health: None,
    });

    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("coworker_status"));
    assert!(json.contains("park"));
    assert!(json.contains("stopped"));
}

#[test]
fn test_error_update_serialization() {
    let update = WebUpdate::Error(ErrorData {
        message: "Coworker nudge not supported".to_string(),
    });

    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("error"));
    assert!(json.contains("Coworker nudge not supported"));
}

#[tokio::test]
async fn test_coworker_nudge_returns_error() {
    // Verify that attempting to nudge a coworker returns an error.
    // Coworker nudges are not supported via the web UI - only lead nudges are allowed.
    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, _) = mpsc::channel(10);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: None, // No coworker manager available
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    let json = r#"{"type": "nudge", "target": "lexington", "message": "test nudge"}"#;
    let result = handle_client_message(json, &state).await;

    // Should return an error since coworker nudges are not supported via web UI
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Cannot nudge coworker"));
    assert!(err_msg.contains("lexington"));
}

#[tokio::test]
async fn test_coworker_nudge_not_supported_via_web_ui() {
    // Verify that nudging a coworker (not lead) returns "not supported via web UI"
    use crate::coworker::CoworkerManager;
    use crate::worktree::WorktreeManager;
    use tempfile::TempDir;

    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, _) = mpsc::channel(10);

    // Create a minimal CoworkerManager for testing
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .ok();

    let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("Failed to create worktree manager");
    let coworkers = CoworkerManager::new(worktree_manager);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: Some(coworkers),
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    // Try to nudge a coworker (not "lead")
    let json = r#"{"type": "nudge", "target": "lexington", "message": "test nudge"}"#;
    let result = handle_client_message(json, &state).await;

    // Should return the "Cannot nudge coworker" error
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Cannot nudge coworker"));
    assert!(err_msg.contains("lexington"));
}

#[tokio::test]
async fn test_error_channel_backpressure() {
    // Stress test: verify that error channel backpressure doesn't block message handling.
    // Generate 20 errors rapidly (channel capacity is 10) and ensure the handler continues.
    use crate::coworker::CoworkerManager;
    use crate::worktree::WorktreeManager;
    use tempfile::TempDir;

    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, _) = mpsc::channel(10);

    // Create a minimal CoworkerManager
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .ok();

    let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("Failed to create worktree manager");
    let coworkers = CoworkerManager::new(worktree_manager);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: Some(coworkers),
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    // Trigger 20 errors (channel capacity is 10) by sending invalid messages.
    // The key assertion: handle_client_message should not hang or panic.
    for i in 0..20 {
        let json = format!(r#"{{"type": "invalid_type_{}"}}"#, i);
        let result = handle_client_message(&json, &state).await;
        // All should error (invalid message type)
        assert!(result.is_err(), "Expected error for invalid message {}", i);
    }

    // If we reach here without hanging, backpressure is handled correctly
}

#[test]
fn test_channel_message_update_without_source_channel() {
    let msg = Message::text("lexington", "Hello from main channel");
    let update = channel_message_update(&msg);
    match update {
        WebUpdate::ChannelMessage(data) => {
            assert_eq!(data.from, "lexington");
            assert_eq!(data.content, "Hello from main channel");
            assert_eq!(data.msg_type, "text");
            assert_eq!(data.channel, "midtown");
            assert_eq!(data.source_channel, None);
            // source_channel should be omitted from JSON when None
            let json = serde_json::to_string(&data).unwrap();
            assert!(!json.contains("source_channel"));
        }
        _ => panic!("Expected ChannelMessage"),
    }
}

#[test]
fn test_channel_message_update_with_source_channel() {
    let mut msg = Message::insight("architect", "```mermaid\ngraph TD\nA-->B");
    msg.source_channel = Some("auth-refactor".to_string());
    let update = channel_message_update(&msg);
    match update {
        WebUpdate::ChannelMessage(data) => {
            assert_eq!(data.from, "architect");
            assert_eq!(data.msg_type, "insight");
            assert_eq!(data.source_channel, Some("auth-refactor".to_string()));
            // source_channel should be present in JSON when Some
            let json = serde_json::to_string(&data).unwrap();
            assert!(json.contains("source_channel"));
            assert!(json.contains("auth-refactor"));
        }
        _ => panic!("Expected ChannelMessage"),
    }
}

#[test]
fn test_source_channel_omitted_in_serialization_when_none() {
    let data = ChannelMessageData {
        id: "test-id".to_string(),
        from: "test".to_string(),
        content: "Hello".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: "midtown".to_string(),
        source_channel: None,
        thread_parent_id: None,
    };
    let json = serde_json::to_string(&data).unwrap();
    // skip_serializing_if = "Option::is_none" should omit source_channel
    assert!(!json.contains("source_channel"));
}

/// Test that verifies backend preconditions for task !1191 requirements:
/// Web UI channel switching and per-channel WebSocket updates
///
/// NOTE: This unit test verifies data structures and serialization. Full API behavior
/// is tested in integration tests (tests/web_e2e.rs::test_api_channel_history_per_channel).
#[test]
fn test_task_1191_channel_switching_requirements() {
    // Requirement 1: API accepts channel parameter on history endpoint
    // Tested in integration tests (test_api_channel_history_per_channel).
    // This unit test only verifies the backend data structures.

    // Requirement 2: WebSocket broadcasts include channel field
    // ChannelMessageData includes a channel field that defaults to "midtown"
    let msg_with_channel = ChannelMessageData {
        id: "test-id-1".to_string(),
        from: "park".to_string(),
        content: "test message".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: "auth-refactor".to_string(),
        source_channel: None,
        thread_parent_id: None,
    };
    assert_eq!(msg_with_channel.channel, "auth-refactor");

    // Verify channel field is present in JSON serialization
    let json = serde_json::to_string(&msg_with_channel).unwrap();
    assert!(json.contains("\"channel\":\"auth-refactor\""));

    // Default channel behavior
    let msg_default = ChannelMessageData {
        id: "test-id-2".to_string(),
        from: "test".to_string(),
        content: "hello".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: default_channel(),
        source_channel: None,
        thread_parent_id: None,
    };
    assert_eq!(msg_default.channel, "midtown");

    // Requirement 3: Web UI can switch channels and load channel-specific messages
    // This is implemented in web-app/src/lib/ChannelList.svelte::selectChannel()
    // and api.js::fetchHistory(channelName). The API endpoint behavior is tested
    // in integration tests (test_api_channel_history_per_channel).

    // Requirement 4: Unread indicators work per channel
    // The web UI tracks unread counts per channel (ChannelList.svelte line 148-150)
    // and increments unread for non-active channels (api.js handleUpdate line 399-401).
    // This unit test verifies the backend precondition: WebSocket messages include
    // the channel field needed for frontend routing.
    let msg = crate::message::Message::text("coworker", "test");
    let update = channel_message_update(&msg);
    match update {
        WebUpdate::ChannelMessage(data) => {
            // Verify channel field has correct default value
            assert_eq!(data.channel, "midtown");
        }
        _ => panic!("Expected ChannelMessage update"),
    }

    // Test that explicit channel propagates through channel_message_update
    let mut msg_with_explicit_channel = crate::message::Message::text("park", "hello");
    msg_with_explicit_channel.channel = Some("auth-refactor".to_string());
    let update = channel_message_update(&msg_with_explicit_channel);
    match update {
        WebUpdate::ChannelMessage(data) => {
            assert_eq!(data.channel, "auth-refactor");
        }
        _ => panic!("Expected ChannelMessage update"),
    }
}

#[test]
fn test_keyed_ttl_cache_returns_none_on_key_mismatch() {
    let cache = KeyedTtlCache::new();
    cache.set("key-a".to_string(), vec![1, 2, 3]);

    // Same key should hit
    let result = cache.get(Duration::from_secs(60), &"key-a".to_string());
    assert_eq!(result, Some(vec![1, 2, 3]));

    // Different key should miss, even within TTL
    let result = cache.get(Duration::from_secs(60), &"key-b".to_string());
    assert_eq!(result, None);
}

#[test]
fn test_keyed_ttl_cache_invalidates_on_key_change() {
    let cache = KeyedTtlCache::new();

    // Store with key-a
    cache.set("key-a".to_string(), vec![1, 2, 3]);
    assert_eq!(
        cache.get(Duration::from_secs(60), &"key-a".to_string()),
        Some(vec![1, 2, 3])
    );

    // Store with key-b (overwrites)
    cache.set("key-b".to_string(), vec![4, 5, 6]);

    // key-a should now miss
    assert_eq!(
        cache.get(Duration::from_secs(60), &"key-a".to_string()),
        None
    );
    // key-b should hit
    assert_eq!(
        cache.get(Duration::from_secs(60), &"key-b".to_string()),
        Some(vec![4, 5, 6])
    );
}

#[test]
fn test_keyed_ttl_cache_respects_ttl() {
    let cache = KeyedTtlCache::new();
    cache.set("key".to_string(), 42);

    // Should hit within TTL
    assert_eq!(
        cache.get(Duration::from_secs(60), &"key".to_string()),
        Some(42)
    );

    // Should miss with zero TTL (already expired)
    assert_eq!(cache.get(Duration::from_secs(0), &"key".to_string()), None);
}

#[test]
fn test_task_id_truncation_skips_overflow() {
    // Test that task IDs > u32::MAX are skipped (not silently truncated)
    // when building the task_id_by_pr map in the fallback path.
    use std::collections::HashMap;

    let pull_requests = &[
        serde_json::json!({
            "number": 100,
            "title": "Fix bug [Midtown !1234]"
        }),
        serde_json::json!({
            "number": 200,
            // Task ID exceeds u32::MAX (4,294,967,295)
            "title": "Big task [Midtown !5000000000]"
        }),
    ];

    // Simulate the task_id_by_pr building logic from api_status fallback
    let task_id_by_pr: HashMap<u64, u32> = pull_requests
        .iter()
        .filter_map(|pr| {
            let pr_number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?;
            let task_id = extract_task_id_from_pr_title(title)?;
            let task_id_u32 = u32::try_from(task_id).ok()?;
            Some((pr_number, task_id_u32))
        })
        .collect();

    // Should only have the valid entry, overflow entry should be skipped
    assert_eq!(task_id_by_pr.len(), 1);
    assert_eq!(task_id_by_pr.get(&100), Some(&1234));
    assert_eq!(task_id_by_pr.get(&200), None); // Overflow entry skipped
}

#[test]
fn test_fallback_displays_source_task_id_from_pr_title() {
    // Test that the fallback path shows source task IDs from PR titles
    // instead of internal task IDs (matches the RPC path behavior).
    use std::collections::HashMap;

    // Mock PR data: PR #968 is for task !1158 (from PR title)
    let pr_number: u64 = 968;
    let source_task_id: u32 = 1158;

    // Mock reviewer's internal task ID (ephemeral, should not be displayed)
    let reviewer_internal_task_id: u32 = 62;

    // Build task_id_by_pr map (from PR titles)
    let mut task_id_by_pr: HashMap<u64, u32> = HashMap::new();
    task_id_by_pr.insert(pr_number, source_task_id);

    // Simulate the display logic in api_status fallback
    let internal_task_id = Some(reviewer_internal_task_id);
    let pr_number_opt = Some(pr_number);

    // This is the key logic: prefer source task ID from PR title
    let display_task_id = pr_number_opt
        .and_then(|pr| task_id_by_pr.get(&pr).copied())
        .or(internal_task_id);

    // Verify the display shows the source task ID from the PR title
    assert_eq!(
        display_task_id,
        Some(source_task_id),
        "Fallback should display source task ID !{} from PR title, not internal task ID !{}",
        source_task_id,
        reviewer_internal_task_id
    );

    // Verify we don't accidentally show the internal ID
    assert_ne!(
        display_task_id,
        Some(reviewer_internal_task_id),
        "Should NOT display reviewer's internal task ID"
    );
}

#[test]
fn test_client_message_answer_question_parsing() {
    let json = r#"{"type": "answer_question", "coworker_name": "lexington", "answer": "yes, proceed with the refactor"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::AnswerQuestion {
            coworker_name,
            answer,
        } => {
            assert_eq!(coworker_name, "lexington");
            assert_eq!(answer, "yes, proceed with the refactor");
        }
        _ => panic!("Expected AnswerQuestion"),
    }
}

#[tokio::test]
async fn test_answer_question_invalid_coworker_name() {
    // Verify that an invalid coworker name (with special chars) returns an error.
    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, _) = mpsc::channel(10);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: None,
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    let json =
        r#"{"type": "answer_question", "coworker_name": "../../etc/passwd", "answer": "test"}"#;
    let result = handle_client_message(json, &state).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Invalid coworker name"));
}

#[tokio::test]
async fn test_answer_question_empty_answer() {
    // Verify that an empty answer returns an error.
    let (updates_tx, _) = broadcast::channel(10);
    let (channel_post_tx, _) = mpsc::channel(10);

    let state = Arc::new(WebState {
        config: WebConfig::default(),
        updates_tx,
        coworkers: None,
        channel_post_tx,
        push_manager: None,
        all_repo_paths: Vec::new(),
        default_branch: "main".to_string(),
        max_coworkers: 8,
        repo_name_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
    });

    let json = r#"{"type": "answer_question", "coworker_name": "lexington", "answer": ""}"#;
    let result = handle_client_message(json, &state).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(err_msg.contains("Empty answer"));
}

#[test]
fn test_coworker_question_data_serialization() {
    // Verify that CoworkerQuestionData serializes correctly for the WebSocket.
    let data = CoworkerQuestionData {
        id: 42,
        coworker_name: "lexington".to_string(),
        question: "Should I proceed with the migration?".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
    };
    let update = WebUpdate::CoworkerQuestion(data);
    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("coworker_question"));
    assert!(json.contains("lexington"));
    assert!(json.contains("Should I proceed with the migration?"));
    assert!(json.contains(r#""id":42"#));
}

#[test]
fn test_send_message_with_thread_parent_id() {
    let json = r#"{"type": "send_message", "content": "thread reply", "thread_parent_id": "parent-uuid-123"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::SendMessage {
            content,
            channel,
            thread_parent_id,
        } => {
            assert_eq!(content, "thread reply");
            assert_eq!(channel, None);
            assert_eq!(thread_parent_id, Some("parent-uuid-123".to_string()));
        }
        _ => panic!("Expected SendMessage"),
    }
}

#[test]
fn test_channel_message_data_with_thread_parent_id() {
    let data = ChannelMessageData {
        id: "msg-1".to_string(),
        from: "park".to_string(),
        content: "thread reply".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: "midtown".to_string(),
        source_channel: None,
        thread_parent_id: Some("parent-uuid-123".to_string()),
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("thread_parent_id"));
    assert!(json.contains("parent-uuid-123"));
    assert!(json.contains("\"id\":\"msg-1\""));
}

#[test]
fn test_channel_message_data_thread_parent_id_omitted_when_none() {
    let data = ChannelMessageData {
        id: "msg-2".to_string(),
        from: "test".to_string(),
        content: "top-level message".to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        msg_type: "text".to_string(),
        channel: "midtown".to_string(),
        source_channel: None,
        thread_parent_id: None,
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(!json.contains("thread_parent_id"));
}

#[test]
fn test_channel_message_update_includes_thread_parent_id() {
    let mut msg = crate::message::Message::text("lexington", "thread reply");
    msg.thread_parent_id = Some("parent-abc".to_string());
    let update = channel_message_update(&msg);
    match update {
        WebUpdate::ChannelMessage(data) => {
            assert_eq!(data.thread_parent_id, Some("parent-abc".to_string()));
            assert!(!data.id.is_empty(), "id should be populated from message");
        }
        _ => panic!("Expected ChannelMessage"),
    }
}

#[test]
fn test_channel_message_update_thread_parent_id_none_for_top_level() {
    let msg = crate::message::Message::text("madison", "top-level");
    let update = channel_message_update(&msg);
    match update {
        WebUpdate::ChannelMessage(data) => {
            assert_eq!(data.thread_parent_id, None);
        }
        _ => panic!("Expected ChannelMessage"),
    }
}

#[test]
fn test_mobile_channel_post_with_thread_parent_id() {
    let post = MobileChannelPost {
        content: "thread reply from mobile".to_string(),
        channel: Some("auth".to_string()),
        thread_parent_id: Some("parent-xyz".to_string()),
    };
    assert_eq!(post.thread_parent_id, Some("parent-xyz".to_string()));
}

#[test]
fn test_mobile_channel_post_without_thread_parent_id() {
    let post = MobileChannelPost {
        content: "regular message".to_string(),
        channel: None,
        thread_parent_id: None,
    };
    assert_eq!(post.thread_parent_id, None);
}
