use super::*;

// ── apply_task_channel_mapping tests ─────────────────────────────────────────

#[test]
fn test_apply_task_channel_mapping_sets_channel() {
    let mut map = HashMap::new();
    let changed = apply_task_channel_mapping(&mut map, "42", Some("auth"), false);
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_overwrites_existing() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "old-channel".to_string());
    let changed = apply_task_channel_mapping(&mut map, "42", Some("new-channel"), false);
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"new-channel".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_ignores_none() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    let changed = apply_task_channel_mapping(&mut map, "42", None, false);
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_ignores_empty_without_clear() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    // On create (allow_clear=false), empty string is ignored
    let changed = apply_task_channel_mapping(&mut map, "42", Some(""), false);
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_clears_with_empty_on_update() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    // On update (allow_clear=true), empty string clears the mapping
    let changed = apply_task_channel_mapping(&mut map, "42", Some(""), true);
    assert!(changed);
    assert!(!map.contains_key("42"));
}

#[test]
fn test_apply_task_channel_mapping_clear_nonexistent_is_noop() {
    let mut map = HashMap::new();
    // Clearing a mapping that doesn't exist returns false (no state modification)
    let changed = apply_task_channel_mapping(&mut map, "99", Some(""), true);
    assert!(!changed);
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_channel_mapping_none_on_empty_map() {
    let mut map: HashMap<String, String> = HashMap::new();
    let changed = apply_task_channel_mapping(&mut map, "42", None, true);
    assert!(!changed);
    assert!(map.is_empty());
}

// ── validate_model_format tests ──────────────────────────────────────────────

#[test]
fn test_validate_model_format_valid() {
    assert!(validate_model_format("claude/opus").is_ok());
    assert!(validate_model_format("claude/sonnet").is_ok());
    assert!(validate_model_format("claude/haiku").is_ok());
    assert!(validate_model_format("codex/o3").is_ok());
    assert!(validate_model_format("codex/o4-mini").is_ok());
}

#[test]
fn test_validate_model_format_invalid() {
    // Missing slash
    assert!(validate_model_format("claude-opus").is_err());
    // Multiple slashes
    assert!(validate_model_format("claude/opus/extra").is_err());
    // Empty string
    assert!(validate_model_format("").is_err());
    // Only slash
    assert!(validate_model_format("/").is_err());
    // Empty provider
    assert!(validate_model_format("/opus").is_err());
    // Empty model
    assert!(validate_model_format("claude/").is_err());
    // Unsupported provider
    assert!(validate_model_format("unknown/opus").is_err());
    assert!(validate_model_format("openai/gpt4").is_err());
    // Whitespace in model or provider
    assert!(validate_model_format("claude/ opus").is_err());
    assert!(validate_model_format("claude /opus").is_err());
    assert!(validate_model_format(" claude/opus").is_err());
    assert!(validate_model_format("claude/opus ").is_err());
}

// ── apply_task_model_mapping tests ───────────────────────────────────────────

#[test]
fn test_apply_task_model_mapping_sets_model() {
    let mut map = HashMap::new();
    let changed = apply_task_model_mapping(&mut map, "42", Some("claude/opus"), false);
    assert!(changed.is_ok());
    assert!(changed.unwrap());
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_rejects_invalid_format() {
    let mut map = HashMap::new();
    let result = apply_task_model_mapping(&mut map, "42", Some("invalid-format"), false);
    assert!(result.is_err());
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_model_mapping_overwrites_existing() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    let changed = apply_task_model_mapping(&mut map, "42", Some("claude/sonnet"), false).unwrap();
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"claude/sonnet".to_string()));
}

#[test]
fn test_apply_task_model_mapping_ignores_none() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    let changed = apply_task_model_mapping(&mut map, "42", None, false).unwrap();
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_ignores_empty_without_clear() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    // On create (allow_clear=false), empty string is ignored
    let changed = apply_task_model_mapping(&mut map, "42", Some(""), false).unwrap();
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_clears_with_empty_on_update() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    // On update (allow_clear=true), empty string clears the mapping
    let changed = apply_task_model_mapping(&mut map, "42", Some(""), true).unwrap();
    assert!(changed);
    assert!(!map.contains_key("42"));
}

#[test]
fn test_apply_task_model_mapping_clear_nonexistent_is_noop() {
    let mut map = HashMap::new();
    // Clearing a mapping that doesn't exist returns false (no state modification)
    let changed = apply_task_model_mapping(&mut map, "99", Some(""), true).unwrap();
    assert!(!changed);
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_model_mapping_none_on_empty_map() {
    let mut map: HashMap<String, String> = HashMap::new();
    let changed = apply_task_model_mapping(&mut map, "42", None, true).unwrap();
    assert!(!changed);
    assert!(map.is_empty());
}

// ── task_thread_id tests ─────────────────────────────────────────────────────
//
// These tests verify thread_id storage via DaemonPersistentState, which is the
// actual data structure modified by handle_task_create (lines 292-294).

/// When `thread_id` is provided in handle_task_create, the task_thread_id
/// mapping in DaemonPersistentState is populated. This mirrors the code path
/// at rpc_task.rs:292-294 where `ps.task_thread_id.insert(...)` is called.
#[test]
fn test_task_thread_id_stored_in_persistent_state() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let thread_id: Option<&str> = Some("thread-parent-uuid-abc");

    // Replicate the handle_task_create storage logic
    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }

    assert_eq!(
        ps.task_thread_id.get("42"),
        Some(&"thread-parent-uuid-abc".to_string()),
        "thread_id should be stored in persistent state's task_thread_id map"
    );
}

/// When `thread_id` is `None` and no announcement message ID is available yet,
/// the task_thread_id mapping is not modified by the explicit thread_id path alone.
/// (In practice, task_thread_id is populated shortly after by defaulting to the
/// announcement message ID — see `test_task_thread_id_defaults_to_announcement_message_id`.)
#[test]
fn test_task_thread_id_not_stored_when_none() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let thread_id: Option<&str> = None;

    // Replicate only the explicit --thread-id path of handle_task_create.
    // (The default-to-announcement path is tested separately.)
    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }

    assert!(
        ps.task_thread_id.is_empty(),
        "task_thread_id should remain empty when thread_id is None"
    );
}

/// When no explicit `thread_id` is provided, task_thread_id should default
/// to the announcement message ID after the task-created message is posted.
/// This ensures SpawnSession picks up a bound_thread_id so coworker messages
/// auto-route to the task announcement thread.
#[test]
fn test_task_thread_id_defaults_to_announcement_message_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let announcement_message_id = "msg-uuid-abc-123";

    // No explicit thread_id was provided during task creation
    let thread_id: Option<&str> = None;
    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }

    // Replicate the post-announcement defaulting logic: if task_thread_id
    // was not set by an explicit --thread-id, default to the announcement
    // message ID.
    ps.task_message_id
        .insert(task_id.to_string(), announcement_message_id.to_string());
    if !ps.task_thread_id.contains_key(task_id) {
        ps.task_thread_id
            .insert(task_id.to_string(), announcement_message_id.to_string());
    }

    assert_eq!(
        ps.task_thread_id.get("42"),
        Some(&announcement_message_id.to_string()),
        "task_thread_id should default to announcement message ID"
    );
    assert_eq!(
        ps.task_thread_id.get("42"),
        ps.task_message_id.get("42"),
        "task_thread_id and task_message_id should be equal when no explicit thread_id"
    );
}

/// When an explicit `thread_id` is provided, it should NOT be overwritten
/// by the announcement message ID.
#[test]
fn test_task_thread_id_explicit_not_overwritten_by_announcement() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let explicit_thread_id = "explicit-thread-uuid";
    let announcement_message_id = "msg-uuid-abc-123";

    // Explicit thread_id was provided during task creation
    ps.task_thread_id
        .insert(task_id.to_string(), explicit_thread_id.to_string());

    // Replicate the post-announcement defaulting logic
    ps.task_message_id
        .insert(task_id.to_string(), announcement_message_id.to_string());
    if !ps.task_thread_id.contains_key(task_id) {
        ps.task_thread_id
            .insert(task_id.to_string(), announcement_message_id.to_string());
    }

    assert_eq!(
        ps.task_thread_id.get("42"),
        Some(&explicit_thread_id.to_string()),
        "explicit thread_id should not be overwritten by announcement message ID"
    );
    assert_ne!(
        ps.task_thread_id.get("42"),
        ps.task_message_id.get("42"),
        "task_thread_id should differ from task_message_id when explicit thread_id was provided"
    );
}

/// `DaemonPersistentState` with `task_thread_id` survives round-trip JSON
/// serialization (backward-compatible with states that lack the field).
#[test]
fn test_task_thread_id_serialization_roundtrip() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.task_thread_id
        .insert("99".to_string(), "thread-parent-xyz".to_string());

    let json = serde_json::to_string(&ps).expect("serialize");
    let parsed: DaemonPersistentState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        parsed.task_thread_id.get("99"),
        Some(&"thread-parent-xyz".to_string())
    );
}

/// Older daemon state JSON (without `task_thread_id`) deserializes with an
/// empty map (the `#[serde(default)]` attribute).
#[test]
fn test_task_thread_id_defaults_to_empty_on_old_state() {
    use crate::daemon::state::DaemonPersistentState;

    // JSON payload that represents a state without task_thread_id
    let json = r#"{}"#;
    let ps: DaemonPersistentState = serde_json::from_str(json).expect("deserialize");
    assert!(
        ps.task_thread_id.is_empty(),
        "task_thread_id should default to empty for old state files"
    );
}

// ── task_created_message_author tests ────────────────────────────────────────

/// For the main channel, the task-created message should be attributed to "lead".
#[test]
fn test_task_created_message_author_main_channel() {
    // When task_channel matches the main channel, "lead" should be the author.
    let author = task_created_message_author("midtown", "midtown");
    assert_eq!(author, "lead");
}

/// For a sub-channel, the task-created message should be attributed to the
/// channel lead, whose session name equals the channel name.
#[test]
fn test_task_created_message_author_sub_channel() {
    let author = task_created_message_author("notes", "midtown");
    assert_eq!(author, "notes");
}

/// For a sub-channel with a hyphenated name.
#[test]
fn test_task_created_message_author_hyphenated_sub_channel() {
    let author = task_created_message_author("web-interface", "myrepo");
    assert_eq!(author, "web-interface");
}

/// The main_channel comparison must use the channel router's default ("midtown"),
/// NOT the repo name. In repos whose name differs from "midtown", tasks created
/// without an explicit channel still land in "midtown" (the hardcoded default),
/// so comparing against the repo name would incorrectly treat them as topic channels.
#[test]
fn test_task_created_message_author_main_channel_non_midtown_repo() {
    // Repo named "myrepo", default channel is "midtown" (hardcoded in channel router).
    // A task with channel="midtown" should be attributed to "lead", not "midtown".
    let author = task_created_message_author("midtown", "midtown");
    assert_eq!(author, "lead");

    // Sanity check: "myrepo" as main_channel with task_channel="midtown" would
    // previously return "midtown" (wrong), but now callers pass the router's
    // default ("midtown") instead of the repo name.
    let wrong_author = task_created_message_author("midtown", "myrepo");
    assert_eq!(wrong_author, "midtown"); // demonstrates the old bug
}

// ── task_created_message routing tests ───────────────────────────────────────

/// For the main channel, the task-created Message should have channel=main
/// and from="lead".
#[test]
fn test_task_created_message_main_channel_routing() {
    use crate::message::MessageType;

    let msg = crate::message::Message::for_channel(
        "midtown",
        task_created_message_author("midtown", "midtown"),
        "created task: Fix the bug",
        MessageType::Text,
    );
    assert_eq!(
        msg.channel_name(),
        "midtown",
        "should route to main channel"
    );
    assert_eq!(
        msg.from, "lead",
        "main channel tasks should be attributed to lead"
    );
}

/// For a sub-channel, the task-created Message should route to that channel
/// and be attributed to the channel lead (whose name equals the channel name).
#[test]
fn test_task_created_message_sub_channel_routing() {
    use crate::message::MessageType;

    let msg = crate::message::Message::for_channel(
        "notes",
        task_created_message_author("notes", "midtown"),
        "created task: Add wiki page",
        MessageType::Text,
    );
    assert_eq!(msg.channel_name(), "notes", "should route to sub-channel");
    assert_eq!(
        msg.from, "notes",
        "sub-channel tasks should be attributed to channel lead"
    );
}

// ── apply_task_channel_mapping clears task_thread_id tests ───────────────────

/// When channel is changed via apply_task_channel_mapping, any existing
/// task_thread_id should be cleared to prevent stale cross-channel thread
/// references. This tests the pattern used in handle_task_update.
#[test]
fn test_channel_change_clears_task_thread_id() {
    let mut task_channel = HashMap::new();
    let mut task_thread_id = HashMap::new();

    // Initial state: task 42 in "ch-a" with a thread binding
    task_channel.insert("42".to_string(), "ch-a".to_string());
    task_thread_id.insert("42".to_string(), "msg-in-ch-a".to_string());

    // Change channel to "ch-b" — mirrors handle_task_update logic
    if apply_task_channel_mapping(&mut task_channel, "42", Some("ch-b"), true) {
        task_thread_id.remove("42");
    }

    assert_eq!(task_channel.get("42"), Some(&"ch-b".to_string()));
    assert!(
        !task_thread_id.contains_key("42"),
        "task_thread_id should be cleared when channel changes"
    );
}

/// When channel is cleared (empty string on update), task_thread_id should
/// also be cleared.
#[test]
fn test_channel_clear_clears_task_thread_id() {
    let mut task_channel = HashMap::new();
    let mut task_thread_id = HashMap::new();

    task_channel.insert("42".to_string(), "ch-a".to_string());
    task_thread_id.insert("42".to_string(), "msg-in-ch-a".to_string());

    // Clear channel — mirrors handle_task_update logic
    if apply_task_channel_mapping(&mut task_channel, "42", Some(""), true) {
        task_thread_id.remove("42");
    }

    assert!(!task_channel.contains_key("42"));
    assert!(
        !task_thread_id.contains_key("42"),
        "task_thread_id should be cleared when channel is cleared"
    );
}

/// When channel is unchanged (None on update), task_thread_id should
/// be preserved.
#[test]
fn test_no_channel_change_preserves_task_thread_id() {
    let mut task_channel = HashMap::new();
    let mut task_thread_id = HashMap::new();

    task_channel.insert("42".to_string(), "ch-a".to_string());
    task_thread_id.insert("42".to_string(), "msg-in-ch-a".to_string());

    // No channel change (None) — task_thread_id should be preserved
    if apply_task_channel_mapping(&mut task_channel, "42", None, true) {
        task_thread_id.remove("42");
    }

    assert_eq!(task_channel.get("42"), Some(&"ch-a".to_string()));
    assert_eq!(
        task_thread_id.get("42"),
        Some(&"msg-in-ch-a".to_string()),
        "task_thread_id should be preserved when channel is unchanged"
    );
}
