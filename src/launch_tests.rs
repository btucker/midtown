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

// --- Disallowed tools tests ---

#[test]
fn test_channel_lead_disallowed_tools_contains_code_modification_tools() {
    use crate::launch::channel_lead_disallowed_tools;

    let tools = channel_lead_disallowed_tools();
    assert!(tools.contains(&"Edit".to_string()));
    assert!(tools.contains(&"Write".to_string()));
    assert!(tools.contains(&"NotebookEdit".to_string()));
    // Bash is intentionally NOT blocked — channel leads need it for
    // coordination commands (midtown task create, midtown channel post, etc.)
    assert!(!tools.contains(&"Bash".to_string()));
}

#[test]
fn test_channel_lead_headless_config_has_disallowed_tools() {
    let config = LaunchConfig::channel_lead("auth", "myrepo", SessionMode::Fresh, "");
    let headless = config.to_headless_config("midtown");
    assert!(
        !headless.disallowed_tools.is_empty(),
        "Channel lead should have disallowed tools"
    );
    assert!(
        headless.disallowed_tools.contains(&"Edit".to_string()),
        "Channel lead should disallow Edit"
    );
    assert!(
        headless.disallowed_tools.contains(&"Write".to_string()),
        "Channel lead should disallow Write"
    );
    assert!(
        !headless.disallowed_tools.contains(&"Bash".to_string()),
        "Channel lead should NOT disallow Bash (needed for midtown CLI commands)"
    );
    assert!(
        headless
            .disallowed_tools
            .contains(&"NotebookEdit".to_string()),
        "Channel lead should disallow NotebookEdit"
    );
}

#[test]
fn test_coworker_headless_config_has_no_disallowed_tools() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let headless = config.to_headless_config("midtown");
    assert!(
        headless.disallowed_tools.is_empty(),
        "Coworker should not have disallowed tools"
    );
}

#[test]
fn test_reviewer_headless_config_has_no_disallowed_tools() {
    use crate::auth::AuthProvider;

    let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, AuthProvider::Claude);
    let headless = config.to_headless_config("midtown");
    assert!(
        headless.disallowed_tools.is_empty(),
        "Reviewer should not have disallowed tools"
    );
}

#[test]
fn test_lead_headless_config_has_no_disallowed_tools() {
    let config = LaunchConfig::lead("myrepo", None);
    let headless = config.to_headless_config("midtown");
    assert!(
        headless.disallowed_tools.is_empty(),
        "Lead should not have disallowed tools"
    );
}
