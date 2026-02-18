use std::collections::HashMap;

use super::{
    apply_task_channel_mapping, apply_task_model_mapping, resolve_channel_lead_for_task,
    validate_model_format,
};

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

// -------------------------------------------------------------------------
// resolve_channel_lead_for_task tests
// -------------------------------------------------------------------------

#[test]
fn test_resolve_channel_lead_returns_channel_when_lead_exists() {
    let mut task_channel = HashMap::new();
    task_channel.insert("42".to_string(), "daemon-core".to_string());
    let mut channel_lead_sessions = HashMap::new();
    channel_lead_sessions.insert("daemon-core".to_string(), "session-abc".to_string());

    let result = resolve_channel_lead_for_task("42", &task_channel, &channel_lead_sessions);
    assert_eq!(result, Some("daemon-core".to_string()));
}

#[test]
fn test_resolve_channel_lead_skips_midtown_channel() {
    let mut task_channel = HashMap::new();
    task_channel.insert("42".to_string(), "midtown".to_string());
    let mut channel_lead_sessions = HashMap::new();
    channel_lead_sessions.insert("midtown".to_string(), "session-abc".to_string());

    // Main "midtown" channel has no channel lead
    let result = resolve_channel_lead_for_task("42", &task_channel, &channel_lead_sessions);
    assert_eq!(result, None);
}

#[test]
fn test_resolve_channel_lead_skips_when_no_lead_session() {
    let mut task_channel = HashMap::new();
    task_channel.insert("42".to_string(), "daemon-core".to_string());
    // No channel lead session registered
    let channel_lead_sessions = HashMap::new();

    let result = resolve_channel_lead_for_task("42", &task_channel, &channel_lead_sessions);
    assert_eq!(result, None);
}

#[test]
fn test_resolve_channel_lead_skips_when_no_channel_mapping() {
    // Task has no channel mapping — defaults to "midtown"
    let task_channel = HashMap::new();
    let mut channel_lead_sessions = HashMap::new();
    channel_lead_sessions.insert("daemon-core".to_string(), "session-abc".to_string());

    let result = resolve_channel_lead_for_task("42", &task_channel, &channel_lead_sessions);
    assert_eq!(result, None);
}

#[test]
fn test_resolve_channel_lead_skips_when_no_lead_for_this_channel() {
    let mut task_channel = HashMap::new();
    task_channel.insert("42".to_string(), "web-interface".to_string());
    let mut channel_lead_sessions = HashMap::new();
    // Lead exists for a different channel, not for "web-interface"
    channel_lead_sessions.insert("daemon-core".to_string(), "session-abc".to_string());

    let result = resolve_channel_lead_for_task("42", &task_channel, &channel_lead_sessions);
    assert_eq!(result, None);
}
