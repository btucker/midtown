use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn install_writes_files_when_missing() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");

    install_agent_definitions_to(&agents_dir, false).unwrap();

    for (filename, _) in AGENT_DEFINITIONS {
        let path = agents_dir.join(filename);
        assert!(path.exists(), "Expected {} to be installed", filename);
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("---"),
            "Expected YAML frontmatter in {}",
            filename
        );
    }
}

#[test]
fn install_does_not_overwrite_existing_without_force() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Pre-create one file with custom content
    let custom = "custom user content";
    fs::write(agents_dir.join("midtown-coworker.md"), custom).unwrap();

    install_agent_definitions_to(&agents_dir, false).unwrap();

    // Custom file should be preserved
    let content = fs::read_to_string(agents_dir.join("midtown-coworker.md")).unwrap();
    assert_eq!(
        content, custom,
        "Should not overwrite existing file without force"
    );

    // Other files should be installed
    assert!(agents_dir.join("midtown-reviewer.md").exists());
}

#[test]
fn install_overwrites_with_force() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Pre-create one file with custom content
    fs::write(agents_dir.join("midtown-coworker.md"), "custom").unwrap();

    install_agent_definitions_to(&agents_dir, true).unwrap();

    // Custom file should be overwritten
    let content = fs::read_to_string(agents_dir.join("midtown-coworker.md")).unwrap();
    assert_ne!(content, "custom", "Should overwrite with force=true");
    assert!(
        content.contains("midtown-coworker"),
        "Should contain agent definition"
    );
}

#[test]
fn check_outdated_detects_differences() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Install current versions
    install_agent_definitions_to(&agents_dir, true).unwrap();

    // No files should be outdated
    let outdated = check_agent_definitions_outdated_in(&agents_dir);
    assert!(
        outdated.is_empty(),
        "Freshly installed should not be outdated"
    );

    // Modify one file
    fs::write(agents_dir.join("midtown-coworker.md"), "modified content").unwrap();

    let outdated = check_agent_definitions_outdated_in(&agents_dir);
    assert_eq!(outdated.len(), 1);
    assert_eq!(outdated[0], "midtown-coworker.md");
}

#[test]
fn check_outdated_returns_empty_when_all_match() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");

    install_agent_definitions_to(&agents_dir, true).unwrap();

    let outdated = check_agent_definitions_outdated_in(&agents_dir);
    assert!(outdated.is_empty());
}

#[test]
fn check_outdated_includes_missing_files() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    // Don't install anything — all files are missing

    let outdated = check_agent_definitions_outdated_in(&agents_dir);
    assert_eq!(
        outdated.len(),
        AGENT_DEFINITIONS.len(),
        "All missing files should be reported as outdated"
    );
}
