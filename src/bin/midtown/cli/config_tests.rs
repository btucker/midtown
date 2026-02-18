//! Tests for the `midtown config` CLI command.

use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper: create a temp dir for config files and return a GlobalConfig path inside it.
fn temp_global_config(dir: &TempDir) -> PathBuf {
    dir.path().join("config.toml")
}

/// Helper: create a temp dir for config files and return a FullProjectConfig path inside it.
fn temp_project_config(dir: &TempDir) -> PathBuf {
    dir.path().join("project_config.toml")
}

// ──────────────────────────────────────────────────────────────────────────────
// Key validation
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_key_returns_helpful_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = set_global_key("nonexistent.key", "value", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("Unknown key"),
        "Expected 'Unknown key' in: {msg}"
    );
    assert!(
        msg.contains("nonexistent.key"),
        "Expected key name in: {msg}"
    );
    // Should list valid keys
    assert!(
        msg.contains("default.max_coworkers"),
        "Expected valid keys in: {msg}"
    );
}

#[test]
fn unknown_key_get_returns_helpful_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = get_global_key("bogus.field", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("Unknown key"),
        "Expected 'Unknown key' in: {msg}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Type errors
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn invalid_integer_returns_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = set_global_key("default.max_coworkers", "not_a_number", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("invalid") || msg.contains("parse") || msg.contains("integer"),
        "Expected parse error in: {msg}"
    );
}

#[test]
fn invalid_bool_returns_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = set_global_key("default.zellij_swap_layout", "maybe", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("invalid") || msg.contains("parse") || msg.contains("bool"),
        "Expected parse error in: {msg}"
    );
}

#[test]
fn invalid_personality_returns_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = set_global_key("default.personality", "extreme", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("invalid") || msg.contains("personality") || msg.contains("normal"),
        "Expected personality error in: {msg}"
    );
}

#[test]
fn invalid_chat_layout_returns_error() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let result = set_global_key("default.chat_layout", "floating", &config_path);
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(
        msg.contains("invalid") || msg.contains("chat_layout") || msg.contains("auto"),
        "Expected chat_layout error in: {msg}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Global config set/get roundtrips
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn set_and_get_global_max_coworkers() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.max_coworkers", "4", &config_path).unwrap();
    let value = get_global_key("default.max_coworkers", &config_path).unwrap();
    assert_eq!(value, "4");
}

#[test]
fn set_and_get_global_personality() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.personality", "fun", &config_path).unwrap();
    let value = get_global_key("default.personality", &config_path).unwrap();
    assert_eq!(value, "fun");
}

#[test]
fn set_and_get_global_bool() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.zellij_swap_layout", "true", &config_path).unwrap();
    let value = get_global_key("default.zellij_swap_layout", &config_path).unwrap();
    assert_eq!(value, "true");
}

#[test]
fn set_and_get_global_chat_layout() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.chat_layout", "split", &config_path).unwrap();
    let value = get_global_key("default.chat_layout", &config_path).unwrap();
    assert_eq!(value, "split");
}

#[test]
fn set_and_get_global_string() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.user_display_name", "Alice", &config_path).unwrap();
    let value = get_global_key("default.user_display_name", &config_path).unwrap();
    assert_eq!(value, "Alice");
}

#[test]
fn set_and_get_global_webhook_port() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("daemon.webhook_port", "47099", &config_path).unwrap();
    let value = get_global_key("daemon.webhook_port", &config_path).unwrap();
    assert_eq!(value, "47099");
}

#[test]
fn set_and_get_global_github_user() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("daemon.github_user", "octocat", &config_path).unwrap();
    let value = get_global_key("daemon.github_user", &config_path).unwrap();
    assert_eq!(value, "octocat");
}

#[test]
fn set_and_get_global_pr_poll_interval() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("daemon.pr_poll_interval_secs", "120", &config_path).unwrap();
    let value = get_global_key("daemon.pr_poll_interval_secs", &config_path).unwrap();
    assert_eq!(value, "120");
}

#[test]
fn set_and_get_global_chat_monitor_enabled() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("daemon.chat_monitor_enabled", "false", &config_path).unwrap();
    let value = get_global_key("daemon.chat_monitor_enabled", &config_path).unwrap();
    assert_eq!(value, "false");
}

#[test]
fn get_unset_key_returns_not_set() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    // Don't write anything; file doesn't exist yet
    let value = get_global_key("default.max_coworkers", &config_path).unwrap();
    assert_eq!(value, "(not set)");
}

// ──────────────────────────────────────────────────────────────────────────────
// Project config set/get roundtrips
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn set_and_get_project_max_coworkers() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key("default.max_coworkers", "6", &config_path).unwrap();
    let value = get_project_key("default.max_coworkers", &config_path).unwrap();
    assert_eq!(value, "6");
}

#[test]
fn set_and_get_project_daemon_webhook_secret() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key("daemon.webhook_secret", "my-secret", &config_path).unwrap();
    let value = get_project_key("daemon.webhook_secret", &config_path).unwrap();
    assert_eq!(value, "my-secret");
}

#[test]
fn set_and_get_project_chat_min_width() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key("default.chat_min_width", "200", &config_path).unwrap();
    let value = get_project_key("default.chat_min_width", &config_path).unwrap();
    assert_eq!(value, "200");
}

#[test]
fn set_and_get_project_zellij_chat_pane_size() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key("default.zellij_chat_pane_size", "40", &config_path).unwrap();
    let value = get_project_key("default.zellij_chat_pane_size", &config_path).unwrap();
    assert_eq!(value, "40");
}

#[test]
fn set_and_get_project_worktree_cleanup_retention_hours() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key(
        "daemon.worktree_cleanup_retention_hours",
        "48",
        &config_path,
    )
    .unwrap();
    let value = get_project_key("daemon.worktree_cleanup_retention_hours", &config_path).unwrap();
    assert_eq!(value, "48");
}

// ──────────────────────────────────────────────────────────────────────────────
// List output
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn list_global_shows_all_keys() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.max_coworkers", "3", &config_path).unwrap();
    set_global_key("daemon.webhook_port", "47024", &config_path).unwrap();

    let output = list_global_config(&config_path).unwrap();
    assert!(
        output.contains("default.max_coworkers"),
        "Expected key in list: {output}"
    );
    assert!(output.contains("3"), "Expected value in list: {output}");
    assert!(
        output.contains("daemon.webhook_port"),
        "Expected key in list: {output}"
    );
    assert!(output.contains("47024"), "Expected value in list: {output}");
    // All 14 supported keys should appear
    assert!(output.contains("default.personality"), "Missing: {output}");
    assert!(output.contains("default.chat_layout"), "Missing: {output}");
    assert!(
        output.contains("default.chat_min_width"),
        "Missing: {output}"
    );
    assert!(
        output.contains("default.zellij_swap_layout"),
        "Missing: {output}"
    );
    assert!(
        output.contains("default.zellij_chat_pane_size"),
        "Missing: {output}"
    );
    assert!(
        output.contains("default.user_display_name"),
        "Missing: {output}"
    );
    assert!(output.contains("default.bin_command"), "Missing: {output}");
    assert!(
        output.contains("daemon.webhook_secret"),
        "Missing: {output}"
    );
    assert!(
        output.contains("daemon.pr_poll_interval_secs"),
        "Missing: {output}"
    );
    assert!(
        output.contains("daemon.chat_monitor_enabled"),
        "Missing: {output}"
    );
    assert!(output.contains("daemon.github_user"), "Missing: {output}");
    assert!(
        output.contains("daemon.worktree_cleanup_retention_hours"),
        "Missing: {output}"
    );
}

#[test]
fn list_global_shows_not_set_for_unset_values() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let output = list_global_config(&config_path).unwrap();
    // Should show (not set) for unset fields
    assert!(
        output.contains("(not set)"),
        "Expected '(not set)' in: {output}"
    );
}

#[test]
fn list_global_shows_config_file_path() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    let output = list_global_config(&config_path).unwrap();
    assert!(
        output.contains(&config_path.to_string_lossy().to_string()),
        "Expected config path in: {output}"
    );
}

#[test]
fn list_project_shows_config_file_path() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    let output = list_project_config(&config_path).unwrap();
    assert!(
        output.contains(&config_path.to_string_lossy().to_string()),
        "Expected config path in: {output}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Persistence: set/load roundtrip via actual config structs
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn set_persists_to_disk_via_global_config_load() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.max_coworkers", "7", &config_path).unwrap();

    // Reload by reading and parsing the file directly (GlobalConfig::load() hardcodes its path)
    let contents = std::fs::read_to_string(&config_path).unwrap();
    let loaded: midtown::config::GlobalConfig = toml::from_str(&contents).unwrap();
    assert_eq!(loaded.default.max_coworkers, Some(7));
}

#[test]
fn set_persists_to_disk_via_project_config_load() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_project_config(&dir);

    set_project_key("default.max_coworkers", "5", &config_path).unwrap();

    let loaded = midtown::config::FullProjectConfig::load_from(&config_path).unwrap();
    assert_eq!(loaded.default.max_coworkers, Some(5));
}

#[test]
fn multiple_sets_accumulate_correctly() {
    let dir = TempDir::new().unwrap();
    let config_path = temp_global_config(&dir);

    set_global_key("default.max_coworkers", "5", &config_path).unwrap();
    set_global_key("default.personality", "wild", &config_path).unwrap();
    set_global_key("daemon.webhook_port", "47025", &config_path).unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    let loaded: midtown::config::GlobalConfig = toml::from_str(&contents).unwrap();
    assert_eq!(loaded.default.max_coworkers, Some(5));
    assert_eq!(
        loaded.default.personality,
        Some(midtown::config::Personality::Wild)
    );
    assert_eq!(loaded.daemon.webhook_port, Some(47025));
}
