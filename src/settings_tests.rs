use super::*;

/// `ensure_auto_compact_settings` creates `.claude/settings.json` with autoCompact.
///
/// Fork sessions don't receive `--settings <file>` (they use `--resume --fork-session`
/// which skips the explicit settings file to avoid duplicate tool registrations).
/// However, ALL sessions get `--setting-sources project,local`, so they read
/// `.claude/settings.json`. Writing this file into worktrees is the project-agnostic
/// mechanism for fork sessions to get `autoCompact: true`.
///
/// Regression test for !2177 / !2180.
#[test]
fn test_ensure_auto_compact_creates_settings() {
    let dir = tempfile::TempDir::new().expect("temp dir");

    // File doesn't exist yet
    let settings_path = dir.path().join(".claude/settings.json");
    assert!(!settings_path.exists());

    // Call should create it
    ensure_auto_compact_settings(dir.path());
    assert!(settings_path.exists());
    let content = std::fs::read_to_string(&settings_path).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(settings["autoCompact"], true);
}

#[test]
fn test_ensure_auto_compact_idempotent() {
    let dir = tempfile::TempDir::new().expect("temp dir");

    // Create the file first
    ensure_auto_compact_settings(dir.path());

    // Call again — should not error
    ensure_auto_compact_settings(dir.path());

    let settings_path = dir.path().join(".claude/settings.json");
    let content = std::fs::read_to_string(&settings_path).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(settings["autoCompact"], true);
}

#[test]
fn test_ensure_auto_compact_preserves_existing_keys() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    // Write a settings file with other keys but no autoCompact
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"someKey": "someValue"}"#,
    )
    .unwrap();

    ensure_auto_compact_settings(dir.path());

    let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(settings["autoCompact"], true);
    assert_eq!(settings["someKey"], "someValue");
}
