use std::collections::HashSet;
use std::fs;
use std::path::Path;

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn test_sync_directory_with_cleanup_copies_and_removes_stale_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("destination");

    write_file(&source.join(".system/skill-a/SKILL.md"), "A");
    write_file(&source.join(".system/skill-b/SKILL.md"), "B");

    write_file(&destination.join(".system/skill-a/SKILL.md"), "old");
    write_file(&destination.join(".system/stale/SKILL.md"), "remove me");

    super::sync_directory_with_cleanup(&source, &destination).unwrap();

    assert_eq!(
        fs::read_to_string(destination.join(".system/skill-a/SKILL.md")).unwrap(),
        "A"
    );
    assert_eq!(
        fs::read_to_string(destination.join(".system/skill-b/SKILL.md")).unwrap(),
        "B"
    );
    assert!(
        !destination.join(".system/stale").exists(),
        "stale entries should be removed from destination"
    );
}

#[test]
fn test_sync_directory_with_cleanup_replaces_type_mismatch_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("destination");

    // source has a file where destination has a directory
    write_file(&source.join("flat"), "flat-content");
    fs::create_dir_all(destination.join("flat/nested")).unwrap();

    // source has a directory where destination has a file
    write_file(&source.join("tree/item.txt"), "tree-content");
    write_file(&destination.join("tree"), "stale-file");

    super::sync_directory_with_cleanup(&source, &destination).unwrap();

    assert!(destination.join("flat").is_file());
    assert_eq!(
        fs::read_to_string(destination.join("flat")).unwrap(),
        "flat-content"
    );
    assert!(destination.join("tree").is_dir());
    assert_eq!(
        fs::read_to_string(destination.join("tree/item.txt")).unwrap(),
        "tree-content"
    );
}

#[test]
fn test_sync_directory_with_cleanup_noop_when_paths_match() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    write_file(&shared.join("skill/SKILL.md"), "content");

    super::sync_directory_with_cleanup(&shared, &shared).unwrap();
    assert_eq!(
        fs::read_to_string(shared.join("skill/SKILL.md")).unwrap(),
        "content"
    );
}

#[test]
fn test_filter_healthy_plugins_accepts_valid_claude_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("superpowers/1.0.0");
    write_file(
        &plugin_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers"}"#,
    );

    let plugins = vec![serde_json::json!({
        "id": "superpowers@official",
        "installPath": plugin_dir.to_str().unwrap()
    })];

    let result = super::filter_healthy_plugins(&plugins);
    assert_eq!(result, HashSet::from(["superpowers@official".to_string()]));
}

#[test]
fn test_filter_healthy_plugins_accepts_root_plugin_json() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("my-plugin/1.0.0");
    write_file(&plugin_dir.join("plugin.json"), r#"{"name":"my-plugin"}"#);

    let plugins = vec![serde_json::json!({
        "id": "my-plugin@mp",
        "installPath": plugin_dir.to_str().unwrap()
    })];

    let result = super::filter_healthy_plugins(&plugins);
    assert_eq!(result, HashSet::from(["my-plugin@mp".to_string()]));
}

#[test]
fn test_filter_healthy_plugins_rejects_orphaned_installation() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("superpowers/4.3.1");
    write_file(
        &plugin_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers"}"#,
    );
    // Add orphaned marker
    write_file(&plugin_dir.join(".orphaned_at"), "1772752997808");

    let plugins = vec![serde_json::json!({
        "id": "superpowers@official",
        "installPath": plugin_dir.to_str().unwrap()
    })];

    let result = super::filter_healthy_plugins(&plugins);
    assert!(
        result.is_empty(),
        "Orphaned plugin should be excluded: {:?}",
        result
    );
}

#[test]
fn test_filter_healthy_plugins_rejects_missing_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("superpowers/4.3.1");
    // Only LICENSE, no plugin manifest
    write_file(&plugin_dir.join("LICENSE"), "MIT");

    let plugins = vec![serde_json::json!({
        "id": "superpowers@official",
        "installPath": plugin_dir.to_str().unwrap()
    })];

    let result = super::filter_healthy_plugins(&plugins);
    assert!(
        result.is_empty(),
        "Plugin without manifest should be excluded: {:?}",
        result
    );
}

#[test]
fn test_filter_healthy_plugins_skips_entries_without_install_path() {
    let plugins = vec![serde_json::json!({
        "id": "no-path@official"
    })];

    let result = super::filter_healthy_plugins(&plugins);
    assert!(result.is_empty());
}

#[test]
fn test_filter_healthy_plugins_mixed_healthy_and_orphaned() {
    let tmp = tempfile::tempdir().unwrap();

    // Healthy plugin
    let healthy_dir = tmp.path().join("code-review/1.0.0");
    write_file(
        &healthy_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"code-review"}"#,
    );

    // Orphaned plugin
    let orphaned_dir = tmp.path().join("superpowers/4.3.1");
    write_file(&orphaned_dir.join("LICENSE"), "MIT");
    write_file(&orphaned_dir.join(".orphaned_at"), "1772752997808");

    let plugins = vec![
        serde_json::json!({
            "id": "code-review@official",
            "installPath": healthy_dir.to_str().unwrap()
        }),
        serde_json::json!({
            "id": "superpowers@official",
            "installPath": orphaned_dir.to_str().unwrap()
        }),
    ];

    let result = super::filter_healthy_plugins(&plugins);
    assert_eq!(result, HashSet::from(["code-review@official".to_string()]));
}
