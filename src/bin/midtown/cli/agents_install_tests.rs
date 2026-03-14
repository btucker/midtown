use super::{AGENT_DEFINITIONS, check_agent_definitions_outdated, install_agent_definitions};
use std::fs;
use tempfile::TempDir;

#[test]
fn install_writes_files_when_they_dont_exist() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");

    let result = install_agent_definitions(&agents_dir, false);
    assert!(result.is_ok(), "install should succeed: {:?}", result);

    let installed = result.unwrap();
    assert_eq!(
        installed.len(),
        AGENT_DEFINITIONS.len(),
        "should install all definitions"
    );

    for def in AGENT_DEFINITIONS {
        let path = agents_dir.join(def.filename);
        assert!(path.exists(), "{} should exist", def.filename);
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(
            content, def.content,
            "{} content should match",
            def.filename
        );
    }
}

#[test]
fn install_does_not_overwrite_existing_without_force() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Pre-create one file with custom content
    let custom_content = "# My custom agent definition\n";
    fs::write(
        agents_dir.join(AGENT_DEFINITIONS[0].filename),
        custom_content,
    )
    .unwrap();

    let result = install_agent_definitions(&agents_dir, false);
    assert!(result.is_ok());

    let installed = result.unwrap();
    // Should install all EXCEPT the one that already exists
    assert_eq!(installed.len(), AGENT_DEFINITIONS.len() - 1);

    // The pre-existing file should be untouched
    let content = fs::read_to_string(agents_dir.join(AGENT_DEFINITIONS[0].filename)).unwrap();
    assert_eq!(
        content, custom_content,
        "existing file should not be overwritten"
    );
}

#[test]
fn install_overwrites_existing_with_force() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Pre-create one file with custom content
    let custom_content = "# My custom agent definition\n";
    fs::write(
        agents_dir.join(AGENT_DEFINITIONS[0].filename),
        custom_content,
    )
    .unwrap();

    let result = install_agent_definitions(&agents_dir, true);
    assert!(result.is_ok());

    let installed = result.unwrap();
    assert_eq!(
        installed.len(),
        AGENT_DEFINITIONS.len(),
        "force should install all definitions"
    );

    // The pre-existing file should now have the compiled-in content
    let content = fs::read_to_string(agents_dir.join(AGENT_DEFINITIONS[0].filename)).unwrap();
    assert_eq!(
        content, AGENT_DEFINITIONS[0].content,
        "force should overwrite existing"
    );
}

#[test]
fn check_outdated_detects_differing_files() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Write one file with different content
    fs::write(
        agents_dir.join(AGENT_DEFINITIONS[0].filename),
        "old content",
    )
    .unwrap();

    // Write another file with matching content
    if AGENT_DEFINITIONS.len() > 1 {
        fs::write(
            agents_dir.join(AGENT_DEFINITIONS[1].filename),
            AGENT_DEFINITIONS[1].content,
        )
        .unwrap();
    }

    let outdated = check_agent_definitions_outdated(&agents_dir);
    assert!(
        outdated
            .iter()
            .any(|d| d.filename == AGENT_DEFINITIONS[0].filename),
        "should detect the differing file"
    );
    if AGENT_DEFINITIONS.len() > 1 {
        assert!(
            !outdated
                .iter()
                .any(|d| d.filename == AGENT_DEFINITIONS[1].filename),
            "should not flag matching file"
        );
    }
}

#[test]
fn check_outdated_returns_empty_when_all_match() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Write all files with matching content
    for def in AGENT_DEFINITIONS {
        fs::write(agents_dir.join(def.filename), def.content).unwrap();
    }

    let outdated = check_agent_definitions_outdated(&agents_dir);
    assert!(outdated.is_empty(), "no files should be outdated");
}

#[test]
fn check_outdated_includes_missing_files() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("agents");
    // Don't create the directory — all files are "missing"

    let outdated = check_agent_definitions_outdated(&agents_dir);
    assert_eq!(
        outdated.len(),
        AGENT_DEFINITIONS.len(),
        "all missing files should be reported as outdated"
    );
}

#[test]
fn install_creates_parent_directories() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join("deeply").join("nested").join("agents");

    let result = install_agent_definitions(&agents_dir, false);
    assert!(result.is_ok());
    assert!(agents_dir.exists(), "should create parent directories");
}

#[test]
fn agent_definitions_have_expected_filenames() {
    let filenames: Vec<&str> = AGENT_DEFINITIONS.iter().map(|d| d.filename).collect();
    assert!(filenames.contains(&"midtown-code-author.md"));
    assert!(filenames.contains(&"midtown-code-reviewer.md"));
    assert!(filenames.contains(&"midtown-project-lead.md"));
    assert!(filenames.contains(&"midtown-channel-lead.md"));
}
