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
/// This ensures SpawnForTask picks up a bound_thread_id so coworker messages
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

// ── --task thread resolution tests ───────────────────────────────────────────
//
// These tests verify the semantics that `--task <id>` should resolve to
// `task_thread_id` (the conversation thread root) rather than `task_message_id`
// (the announcement message). When a task is created with `--thread-id <parent>`,
// the announcement becomes a reply within that thread, so using `task_message_id`
// as the thread parent would nest replies incorrectly.

/// When a task is created with `--thread-id`, the `--task` resolution should
/// prefer `task_thread_id` (the parent thread) over `task_message_id` (the
/// announcement reply). This mirrors the logic in `channel_post_for_task`.
#[test]
fn test_task_thread_resolution_prefers_thread_id_over_message_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let parent_thread = "user-original-message-uuid";
    let announcement_reply = "announcement-reply-uuid";

    // Task created with --thread-id: announcement is a reply in the parent thread
    ps.task_thread_id
        .insert(task_id.to_string(), parent_thread.to_string());
    ps.task_message_id
        .insert(task_id.to_string(), announcement_reply.to_string());

    // Simulate the --task resolution logic from channel_post_for_task:
    // prefer thread_id, fall back to message_id
    let resolved = ps
        .task_thread_id
        .get(task_id)
        .or_else(|| ps.task_message_id.get(task_id));

    assert_eq!(
        resolved,
        Some(&parent_thread.to_string()),
        "--task should resolve to the parent thread, not the announcement reply"
    );
    assert_ne!(
        resolved,
        Some(&announcement_reply.to_string()),
        "using task_message_id would create nested replies instead of siblings"
    );
}

/// When a task is created without `--thread-id`, `task_thread_id` equals
/// `task_message_id`, so `--task` resolution produces the same result
/// regardless of which field is used.
#[test]
fn test_task_thread_resolution_equivalent_without_explicit_thread_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let announcement_id = "announcement-uuid";

    // Task created without --thread-id: both point to the announcement
    ps.task_message_id
        .insert(task_id.to_string(), announcement_id.to_string());
    ps.task_thread_id
        .insert(task_id.to_string(), announcement_id.to_string());

    let resolved = ps
        .task_thread_id
        .get(task_id)
        .or_else(|| ps.task_message_id.get(task_id));

    assert_eq!(
        resolved,
        Some(&announcement_id.to_string()),
        "--task should resolve to the announcement message (same as thread_id)"
    );
}

/// Backward compatibility: tasks created before `task_thread_id` was introduced
/// only have `task_message_id`. The resolution should fall back to `message_id`.
#[test]
fn test_task_thread_resolution_falls_back_to_message_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let announcement_id = "old-announcement-uuid";

    // Legacy task: only has task_message_id, no task_thread_id
    ps.task_message_id
        .insert(task_id.to_string(), announcement_id.to_string());

    let resolved = ps
        .task_thread_id
        .get(task_id)
        .or_else(|| ps.task_message_id.get(task_id));

    assert_eq!(
        resolved,
        Some(&announcement_id.to_string()),
        "should fall back to message_id when thread_id is not available"
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

// ── task_announcement_message tests ──────────────────────────────────────────

/// When thread_id is Some, the announcement should be a thread reply.
#[test]
fn test_task_announcement_message_with_thread_id_is_threaded() {
    let msg = task_announcement_message("ops", "ops", "Fix the bug", Some("parent-thread-id"));
    assert_eq!(
        msg.thread_parent_id,
        Some("parent-thread-id".to_string()),
        "announcement should be a thread reply when thread_id is provided"
    );
    assert_eq!(msg.channel_name(), "ops");
}

/// When thread_id is None, the announcement should be top-level.
#[test]
fn test_task_announcement_message_without_thread_id_is_top_level() {
    let msg = task_announcement_message("ops", "ops", "Fix the bug", None);
    assert!(
        msg.thread_parent_id.is_none(),
        "announcement should be top-level when no thread_id"
    );
    assert_eq!(msg.channel_name(), "ops");
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

// ── resolve_effective_task_channel tests ──────────────────────────────────────

/// An archived channel should resolve to the ops channel for routing.
/// This is the core bug fix: tasks created with `--channel daemon` (archived)
/// were spawning a "daemon" channel lead with wrong attribution.
#[test]
fn test_resolve_effective_task_channel_archived_falls_back_to_ops() {
    let effective = resolve_effective_task_channel("daemon", true, false, "midtown");
    assert_eq!(effective, "ops", "archived channels should route to ops");
}

/// A non-archived topic channel should pass through unchanged.
#[test]
fn test_resolve_effective_task_channel_active_channel_unchanged() {
    let effective = resolve_effective_task_channel("notes", false, false, "midtown");
    assert_eq!(effective, "notes", "active channels should pass through");
}

/// The main channel should pass through unchanged (it's never archived).
#[test]
fn test_resolve_effective_task_channel_main_channel_unchanged() {
    let effective = resolve_effective_task_channel("midtown", false, false, "midtown");
    assert_eq!(
        effective, "midtown",
        "main channel should pass through unchanged"
    );
}

/// When the ops channel is also archived, fall back to the main channel.
#[test]
fn test_resolve_effective_task_channel_ops_also_archived_falls_back_to_main() {
    let effective = resolve_effective_task_channel("daemon", true, true, "midtown");
    assert_eq!(
        effective, "midtown",
        "when ops is also archived, should fall back to main channel"
    );
}

/// Combined test: archived channel with announcement routing.
/// When a task is created with `--channel daemon` (archived), the announcement
/// should be authored by "ops" (the effective channel lead), not "daemon".
#[test]
fn test_archived_channel_announcement_uses_ops_author() {
    let effective = resolve_effective_task_channel("daemon", true, false, "midtown");
    let author = task_created_message_author(effective, "midtown");
    assert_eq!(
        author, "ops",
        "archived channel tasks should be announced by the ops channel lead"
    );
}

/// Combined test: active channel with announcement routing stays correct.
#[test]
fn test_active_channel_announcement_uses_channel_author() {
    let effective = resolve_effective_task_channel("notes", false, false, "midtown");
    let author = task_created_message_author(effective, "midtown");
    assert_eq!(
        author, "notes",
        "active channel tasks should be announced by the channel lead"
    );
}

/// The effective channel should be what gets stored in ps.task_channel,
/// so downstream routing (insights, MIDTOWN_CHANNEL, handle_task_metadata)
/// all use the correct routable channel.
#[test]
fn test_archived_channel_stores_effective_in_task_channel_mapping() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";

    // Simulate what handle_task_create now does: resolve effective channel
    // first, then store it in ps.task_channel.
    let effective = resolve_effective_task_channel("daemon", true, false, "midtown");
    apply_task_channel_mapping(&mut ps.task_channel, task_id, Some(effective), false);

    assert_eq!(
        ps.task_channel.get("42"),
        Some(&"ops".to_string()),
        "ps.task_channel should store the effective channel (ops), not the archived name (daemon)"
    );
}

/// When ops is also archived, the main channel should be stored.
#[test]
fn test_ops_archived_stores_main_channel_in_task_channel_mapping() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";

    let effective = resolve_effective_task_channel("daemon", true, true, "midtown");
    apply_task_channel_mapping(&mut ps.task_channel, task_id, Some(effective), false);

    assert_eq!(
        ps.task_channel.get("42"),
        Some(&"midtown".to_string()),
        "when ops is archived, ps.task_channel should store the main channel"
    );
}

// ── task_parent storage tests ────────────────────────────────────────────────

/// When `parent` is provided in handle_task_create, the task_parent mapping
/// in DaemonPersistentState is populated.
#[test]
fn test_task_parent_stored_in_persistent_state() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "43";
    let parent: Option<&str> = Some("42");

    // Replicate the handle_task_create storage logic
    if let Some(p) = parent {
        ps.task_parent.insert(task_id.to_string(), p.to_string());
    }

    assert_eq!(
        ps.task_parent.get("43"),
        Some(&"42".to_string()),
        "parent should be stored in persistent state's task_parent map"
    );
}

/// When `parent` is provided with a `!` or `#` prefix, the prefix is stripped
/// before storing (consistent with handle_view's ID normalization).
#[test]
fn test_task_parent_normalizes_prefixed_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();

    // Replicate the handle_task_create normalization logic
    for (input, expected) in [("!42", "42"), ("#42", "42"), ("42", "42")] {
        let normalized = input
            .strip_prefix('!')
            .or_else(|| input.strip_prefix('#'))
            .unwrap_or(input);
        ps.task_parent
            .insert("child".to_string(), normalized.to_string());
        assert_eq!(
            ps.task_parent.get("child"),
            Some(&expected.to_string()),
            "parent '{}' should be normalized to '{}'",
            input,
            expected
        );
    }
}

/// When `parent` is `None`, the task_parent mapping is not modified.
#[test]
fn test_task_parent_not_stored_when_none() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "43";
    let parent: Option<&str> = None;

    if let Some(p) = parent {
        ps.task_parent.insert(task_id.to_string(), p.to_string());
    }

    assert!(
        ps.task_parent.is_empty(),
        "task_parent should remain empty when parent is None"
    );
}

// ── child task thread inheritance tests ──────────────────────────────────────

/// When a child task is created with a parent but no explicit thread_id,
/// it should inherit the parent's task_thread_id so all messages from the
/// child task go to the same thread as the parent.
#[test]
fn test_child_task_inherits_parent_thread_id() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();

    // Parent task "42" has a thread binding
    ps.task_thread_id
        .insert("42".to_string(), "thread-parent-msg".to_string());

    // Create child task "43" with parent "42", no explicit thread_id
    let task_id = "43";
    let thread_id: Option<&str> = None;
    let parent: Option<&str> = Some("42");

    // Replicate the handle_task_create storage logic (with inheritance)
    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }
    if let Some(p) = parent {
        let normalized = p
            .strip_prefix('!')
            .or_else(|| p.strip_prefix('#'))
            .unwrap_or(p);
        ps.task_parent
            .insert(task_id.to_string(), normalized.to_string());
        // Inherit parent's thread_id if child doesn't have one
        if !ps.task_thread_id.contains_key(task_id)
            && let Some(parent_thread) = ps.task_thread_id.get(normalized).cloned()
        {
            ps.task_thread_id.insert(task_id.to_string(), parent_thread);
        }
    }

    assert_eq!(
        ps.task_thread_id.get("43"),
        Some(&"thread-parent-msg".to_string()),
        "child task should inherit parent's task_thread_id"
    );
}

/// When a child task has an explicit thread_id, it should NOT be overridden
/// by the parent's thread_id.
#[test]
fn test_child_task_explicit_thread_id_not_overridden() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();

    ps.task_thread_id
        .insert("42".to_string(), "parent-thread".to_string());

    let task_id = "43";
    let thread_id: Option<&str> = Some("child-explicit-thread");
    let parent: Option<&str> = Some("42");

    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }
    if let Some(p) = parent {
        let normalized = p
            .strip_prefix('!')
            .or_else(|| p.strip_prefix('#'))
            .unwrap_or(p);
        ps.task_parent
            .insert(task_id.to_string(), normalized.to_string());
        if !ps.task_thread_id.contains_key(task_id)
            && let Some(parent_thread) = ps.task_thread_id.get(normalized).cloned()
        {
            ps.task_thread_id.insert(task_id.to_string(), parent_thread);
        }
    }

    assert_eq!(
        ps.task_thread_id.get("43"),
        Some(&"child-explicit-thread".to_string()),
        "explicit thread_id should not be overridden by parent's"
    );
}

/// When a parent task has no thread_id, the child should not get one either.
#[test]
fn test_child_task_no_inheritance_when_parent_has_no_thread() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    // Parent "42" has no task_thread_id

    let task_id = "43";
    let thread_id: Option<&str> = None;
    let parent: Option<&str> = Some("42");

    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }
    if let Some(p) = parent {
        let normalized = p
            .strip_prefix('!')
            .or_else(|| p.strip_prefix('#'))
            .unwrap_or(p);
        ps.task_parent
            .insert(task_id.to_string(), normalized.to_string());
        if !ps.task_thread_id.contains_key(task_id)
            && let Some(parent_thread) = ps.task_thread_id.get(normalized).cloned()
        {
            ps.task_thread_id.insert(task_id.to_string(), parent_thread);
        }
    }

    assert!(
        !ps.task_thread_id.contains_key("43"),
        "child should not get a thread_id when parent has none"
    );
}

// ── task.prompt model validation tests ────────────────────────────────────────

/// handle_task_prompt should reject invalid model formats before any session lookup.
/// We test this indirectly by verifying validate_model_format catches bad formats.
#[test]
fn test_task_prompt_model_validation_rejects_invalid() {
    // These formats would be rejected by handle_task_prompt before session lookup
    assert!(validate_model_format("invalid-no-slash").is_err());
    assert!(validate_model_format("claude/").is_err());
    assert!(validate_model_format("/opus").is_err());
    assert!(validate_model_format("unknown/opus").is_err());
}

/// handle_task_prompt should accept valid model formats.
#[test]
fn test_task_prompt_model_validation_accepts_valid() {
    assert!(validate_model_format("claude/opus").is_ok());
    assert!(validate_model_format("claude/sonnet").is_ok());
    assert!(validate_model_format("codex/o3").is_ok());
}

/// When --model is provided to task prompt, it should override the task's configured
/// model. This tests the apply_task_model pattern used in the resume path.
#[test]
fn test_task_prompt_model_override_applies_to_config() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "lexington".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::Fresh,
        None,
        Some("42".to_string()),
    );

    // Simulate the --model override path from handle_task_prompt
    let mut override_map = HashMap::new();
    override_map.insert("42".to_string(), "claude/opus".to_string());
    config.apply_task_model(&override_map, "42");

    assert_eq!(config.model, "opus");
    assert_eq!(config.auth_provider, crate::auth::AuthProvider::Claude);
}

/// When no --model is provided, the task's configured model from persistent state
/// should be used.
#[test]
fn test_task_prompt_uses_task_configured_model() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "lexington".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::Fresh,
        None,
        Some("42".to_string()),
    );

    // Simulate the persistent state model lookup path
    let mut task_model = HashMap::new();
    task_model.insert("42".to_string(), "codex/o3".to_string());
    config.apply_task_model(&task_model, "42");

    assert_eq!(config.model, "o3");
    assert_eq!(config.auth_provider, crate::auth::AuthProvider::Codex);
}

/// When neither --model nor task model is configured, the default model should remain.
/// The default is determined by config (may vary by machine), so just verify it's non-empty.
#[test]
fn test_task_prompt_uses_default_model_when_none_configured() {
    let config = crate::launch::LaunchConfig::coworker(
        "lexington".to_string(),
        "nonexistent-test-repo".to_string(),
        crate::launch::SessionMode::Fresh,
        None,
        Some("42".to_string()),
    );

    // No apply_task_model call — model stays at default
    assert!(!config.model.is_empty(), "default model should be set");
}

/// The resume config should use ResumeSession mode with the correct session ID.
#[test]
fn test_task_prompt_resume_config_uses_session_id() {
    let session_id = "test-session-uuid-123";
    let config = crate::launch::LaunchConfig::coworker(
        "lexington".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::ResumeSession(session_id.to_string()),
        Some("Fix the bug".to_string()),
        Some("42".to_string()),
    );

    assert!(
        matches!(config.session_mode, crate::launch::SessionMode::ResumeSession(ref id) if id == session_id)
    );
    assert_eq!(config.initial_prompt.as_deref(), Some("Fix the bug"));
    assert_eq!(config.task_id.as_deref(), Some("42"));
}

/// Task ID prefix stripping (! and #) is used by handle_task_prompt.
/// Test the stripping logic directly.
#[test]
fn test_task_prompt_strips_id_prefixes() {
    fn strip(id: &str) -> &str {
        id.strip_prefix('#')
            .or_else(|| id.strip_prefix('!'))
            .unwrap_or(id)
    }
    assert_eq!(strip("!42"), "42");
    assert_eq!(strip("#42"), "42");
    assert_eq!(strip("42"), "42");
    assert_eq!(strip("!100"), "100");
}

// ── task.handoff tests ───────────────────────────────────────────────────────

/// Handoff builds a resume LaunchConfig with the correct agent_name_override.
#[test]
fn test_task_handoff_config_uses_agent_override() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::ResumeSession("session-abc".to_string()),
        None, // no initial prompt — handoff just swaps the agent
        Some("42".to_string()),
    );
    config.agent_name_override = Some("midtown-code-reviewer".to_string());

    assert_eq!(
        config.agent_name_override.as_deref(),
        Some("midtown-code-reviewer")
    );
    assert!(
        matches!(config.session_mode, crate::launch::SessionMode::ResumeSession(ref id) if id == "session-abc")
    );
    assert_eq!(config.initial_prompt, None);
    assert_eq!(config.task_id.as_deref(), Some("42"));
}

/// Handoff applies task model configuration to the resumed session.
#[test]
fn test_task_handoff_applies_task_model() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::ResumeSession("session-abc".to_string()),
        None,
        Some("42".to_string()),
    );
    config.agent_name_override = Some("midtown-code-reviewer".to_string());

    let mut task_model = HashMap::new();
    task_model.insert("42".to_string(), "claude/opus".to_string());
    config.apply_task_model(&task_model, "42");

    assert_eq!(config.model, "opus");
    assert_eq!(config.auth_provider, crate::auth::AuthProvider::Claude);
    // Agent override should be independent of model
    assert_eq!(
        config.agent_name_override.as_deref(),
        Some("midtown-code-reviewer")
    );
}

/// Handoff resolves coworker name from preferred_name, current_name, or task owner.
#[test]
fn test_task_handoff_name_resolution() {
    // Replicate the name resolution logic from handle_task_handoff
    fn resolve_name<'a>(
        preferred: Option<&'a str>,
        current: Option<&'a str>,
        owner: Option<&'a str>,
    ) -> &'a str {
        preferred.or(current).or(owner).unwrap_or("unknown")
    }

    assert_eq!(
        resolve_name(Some("park"), Some("madison"), Some("lexington")),
        "park"
    );
    assert_eq!(
        resolve_name(None, Some("madison"), Some("lexington")),
        "madison"
    );
    assert_eq!(resolve_name(None, None, Some("lexington")), "lexington");
    assert_eq!(resolve_name(None, None, None), "unknown");
}

/// Handoff sets working directory from session record when available.
#[test]
fn test_task_handoff_sets_working_dir() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::ResumeSession("session-abc".to_string()),
        None,
        Some("42".to_string()),
    );

    // Simulate the working_dir assignment from handle_task_handoff
    let recorded_dir = "/Users/test/.midtown/projects/test/worktrees/park";
    if !recorded_dir.is_empty() {
        config.working_dir = Some(std::path::PathBuf::from(recorded_dir));
    }

    assert_eq!(
        config.working_dir,
        Some(std::path::PathBuf::from(
            "/Users/test/.midtown/projects/test/worktrees/park"
        ))
    );
}

/// Handoff skips working_dir assignment when session record has empty working_dir.
#[test]
fn test_task_handoff_skips_empty_working_dir() {
    let mut config = crate::launch::LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        crate::launch::SessionMode::ResumeSession("session-abc".to_string()),
        None,
        Some("42".to_string()),
    );

    let recorded_dir = "";
    if !recorded_dir.is_empty() {
        config.working_dir = Some(std::path::PathBuf::from(recorded_dir));
    }

    assert_eq!(config.working_dir, None);
}

/// Task ID prefix stripping also works in the handoff path.
#[test]
fn test_task_handoff_strips_id_prefixes() {
    fn strip(id: &str) -> &str {
        id.strip_prefix('#')
            .or_else(|| id.strip_prefix('!'))
            .unwrap_or(id)
    }
    assert_eq!(strip("!42"), "42");
    assert_eq!(strip("#42"), "42");
    assert_eq!(strip("42"), "42");
}

// ── handle_task_handoff async tests ──────────────────────────────────────────
//
// These require a minimal DaemonState to exercise the async handler's
// error paths (task not found, session not found).

fn make_test_state(
    repo_name: &str,
) -> (
    super::super::DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;

    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let project_dir = tempfile::tempdir().expect("project temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(project_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(project_dir.path())
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(project_dir.path())
        .output()
        .expect("git config name");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(project_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(project_dir.path().to_path_buf()).expect("wm");
    let cm = crate::coworker::CoworkerManager::new(wm);
    let channel_router = crate::ChannelRouter::new(project_dir.path(), "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = super::super::DaemonState::new(
        "/tmp/rpc-task-test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
        vec![project_dir.path().to_path_buf()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");

    (state, project_dir, _guard)
}

/// handle_task_handoff returns an error when the task ID does not exist.
#[tokio::test]
async fn test_handle_task_handoff_task_not_found() {
    let (state, _dir, _guard) = make_test_state("handoff-test");
    let response = handle_task_handoff(
        crate::rpc::RequestId::Number(1),
        "nonexistent-999",
        "midtown-code-reviewer",
        None,
        "lead",
        &state,
    )
    .await;

    let json = serde_json::to_value(&response).expect("serialize");
    let error = json.get("error").expect("should have error");
    let message = error.get("message").expect("error message");
    assert!(
        message.as_str().unwrap().contains("not found"),
        "expected 'not found' in error, got: {}",
        message
    );
}

/// handle_task_handoff strips ! and # prefixes from task IDs before lookup.
#[tokio::test]
async fn test_handle_task_handoff_strips_prefix_in_handler() {
    let (state, _dir, _guard) = make_test_state("handoff-strip-test");
    // Both !999 and #999 should resolve to "999" and return "not found"
    // (not a parse error or panic)
    for prefix_id in ["!999", "#999"] {
        let response = handle_task_handoff(
            crate::rpc::RequestId::Number(1),
            prefix_id,
            "midtown-code-reviewer",
            None,
            "lead",
            &state,
        )
        .await;

        let json = serde_json::to_value(&response).expect("serialize");
        let error = json.get("error").expect("should have error");
        let message = error
            .get("message")
            .expect("error message")
            .as_str()
            .unwrap();
        assert!(
            message.contains("not found"),
            "prefix '{}' should strip to '999' and return not found, got: {}",
            prefix_id,
            message
        );
    }
}

/// handle_task_handoff returns "no session found" when the task exists
/// but no session has been assigned to it.
#[tokio::test]
async fn test_handle_task_handoff_no_session_found() {
    let (state, _dir, _guard) = make_test_state("handoff-nosess-test");

    // Create a real task in the test repo's task storage
    let task_id = crate::tasks::create_task_for_repo(
        "Test handoff task",
        "description",
        "Testing handoff task",
        "park",
        "handoff-nosess-test",
        None,
        None,
        None,
    )
    .expect("create task");

    let response = handle_task_handoff(
        crate::rpc::RequestId::Number(2),
        &task_id,
        "midtown-code-reviewer",
        None,
        "lead",
        &state,
    )
    .await;

    let json = serde_json::to_value(&response).expect("serialize");
    let error = json.get("error").expect("should have error");
    let message = error
        .get("message")
        .expect("error message")
        .as_str()
        .unwrap();
    assert!(
        message.contains("No session found"),
        "expected 'No session found', got: {}",
        message
    );
}

/// handle_task_handoff succeeds (updates agent type) when a session mapping
/// exists in task_to_session but the session record is missing from persistent
/// state. Main's implementation gracefully proceeds — it updates task_agent_type
/// and returns success without requiring the session record for the no-message path.
#[tokio::test]
async fn test_handle_task_handoff_session_exists_but_no_record() {
    let (state, _dir, _guard) = make_test_state("handoff-norec-test");

    // Create a real task
    let task_id = crate::tasks::create_task_for_repo(
        "Test handoff no record",
        "description",
        "Testing handoff",
        "park",
        "handoff-norec-test",
        None,
        None,
        None,
    )
    .expect("create task");

    // Insert a fake session mapping (task → session) without a corresponding
    // session record in persistent state
    let fake_session_id = "fake-session-abc-123";
    state
        .task_to_session
        .lock()
        .unwrap()
        .insert(task_id.clone(), fake_session_id.to_string());

    let response = handle_task_handoff(
        crate::rpc::RequestId::Number(3),
        &task_id,
        "midtown-code-reviewer",
        None,
        "lead",
        &state,
    )
    .await;

    // With no message, handle_task_handoff updates task_agent_type and returns
    // success even without a session record (graceful degradation).
    let json = serde_json::to_value(&response).expect("serialize");
    let result = json.get("result").expect("should have result");
    let message = result
        .get("message")
        .expect("result message")
        .as_str()
        .unwrap();
    assert!(
        message.contains("agent type changed"),
        "expected 'agent type changed' in result, got: {}",
        message
    );
}
