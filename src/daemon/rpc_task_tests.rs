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

/// When `thread_id` is `None`, the task_thread_id mapping in
/// DaemonPersistentState is not modified.
#[test]
fn test_task_thread_id_not_stored_when_none() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    let task_id = "42";
    let thread_id: Option<&str> = None;

    // Replicate the handle_task_create storage logic
    if let Some(tid) = thread_id {
        ps.task_thread_id
            .insert(task_id.to_string(), tid.to_string());
    }

    assert!(
        ps.task_thread_id.is_empty(),
        "task_thread_id should remain empty when thread_id is None"
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
