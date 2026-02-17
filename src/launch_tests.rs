//! Tests for lead system prompt persistence on attach

use crate::launch::{CoworkerRole, LaunchConfig, SessionMode};
use crate::paths;
use std::fs;

#[test]
fn test_lead_system_prompt_saved_on_spawn() {
    // Set up test environment
    let temp_dir = tempfile::tempdir().unwrap();
    let _guard = paths::set_test_midtown_base_dir(temp_dir.path().to_path_buf());

    // Create a lead launch config
    let config = LaunchConfig {
        name: "lead".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Lead,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        auth_profile_dir: None,
    };

    // Convert to headless config (this should save the system prompt)
    let headless = config.to_headless_config("test-repo");

    // Verify the system prompt file was created
    let prompt_file = paths::lead_system_prompt_file("test-repo");
    assert!(
        prompt_file.exists(),
        "Lead system prompt file should be created at {}",
        prompt_file.display()
    );

    // Verify the file contains the system prompt
    let saved_prompt = fs::read_to_string(&prompt_file).unwrap();
    assert_eq!(saved_prompt, headless.system_prompt);
    // Lead system prompt should contain lead-specific content
    assert!(
        saved_prompt.contains("# Lead System Prompt"),
        "Expected lead system prompt content"
    );
}

#[test]
fn test_lead_system_prompt_file_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let _guard = paths::set_test_midtown_base_dir(temp_dir.path().to_path_buf());

    let prompt_file = paths::lead_system_prompt_file("myrepo");
    let expected = temp_dir
        .path()
        .join("lead")
        .join("myrepo")
        .join("system-prompt.txt");

    assert_eq!(prompt_file, expected);
}
