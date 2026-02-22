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
