//! Tests for lead system prompt persistence on attach and channel lead model selection

use crate::launch::{CoworkerRole, LaunchConfig, SessionMode, inject_session_id_env};
use crate::paths;
use std::fs;

#[test]
fn test_launch_config_ops_channel_lead_model() {
    let config = LaunchConfig::channel_lead("ops", "myrepo", SessionMode::Fresh, "");
    let execution_fallback = crate::config::get_channel_lead_model_fallback("myrepo");
    let expected = crate::config::get_channel_leads_config("myrepo")
        .model_for_channel_with_fallback("ops", execution_fallback);
    assert_eq!(
        config.model, expected,
        "ops channel lead model should match config resolution"
    );
}

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
        persisted_initial_prompt: None,
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
        saved_prompt.contains("# Project Lead"),
        "Expected Project Lead system prompt content"
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

#[test]
fn test_inject_session_id_env_sets_midtown_session_id() {
    let mut env = std::collections::BTreeMap::new();
    assert!(
        !env.contains_key("MIDTOWN_SESSION_ID"),
        "MIDTOWN_SESSION_ID must not be present before injection"
    );
    inject_session_id_env(&mut env, "test-uuid-abc123");
    assert_eq!(
        env.get("MIDTOWN_SESSION_ID").map(String::as_str),
        Some("test-uuid-abc123"),
        "inject_session_id_env must insert MIDTOWN_SESSION_ID into the env map"
    );
}

#[test]
fn test_inject_session_id_env_overwrites_existing_value() {
    let mut env = std::collections::BTreeMap::new();
    env.insert("MIDTOWN_SESSION_ID".to_string(), "old-uuid".to_string());
    inject_session_id_env(&mut env, "new-uuid-xyz");
    assert_eq!(
        env.get("MIDTOWN_SESSION_ID").map(String::as_str),
        Some("new-uuid-xyz"),
    );
}
