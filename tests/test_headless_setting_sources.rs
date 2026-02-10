//! Test that headless coworkers properly propagate setting_sources flag.
//!
//! This ensures coworkers don't load plugins from both auth profile settings
//! and coworker-settings.json, which causes duplicate tool names after idle→resume.

use midtown::headless::HeadlessConfig;
use midtown::launch::{CoworkerRole, LaunchConfig, SessionMode};

#[test]
fn test_coworker_config_propagates_setting_sources() {
    // Create a coworker config with restrict_setting_sources=true
    let launch_config = LaunchConfig {
        name: "test-coworker".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
        team_name: Some("midtown-test".to_string()),
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
    };

    // Convert to headless config
    let headless_config = launch_config.to_headless_config();

    // Verify setting_sources is set to "project,local"
    assert_eq!(
        headless_config.setting_sources,
        Some("project,local".to_string()),
        "Coworker with restrict_setting_sources=true should set setting_sources to 'project,local'"
    );
}

#[test]
fn test_coworker_config_without_restriction_has_no_setting_sources() {
    // Create a coworker config with restrict_setting_sources=false
    let launch_config = LaunchConfig {
        name: "test-coworker".to_string(),
        session_mode: SessionMode::Fresh,
        role: CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        restrict_setting_sources: false,
        pr_number: None,
        team_name: Some("midtown-test".to_string()),
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
    };

    // Convert to headless config
    let headless_config = launch_config.to_headless_config();

    // Verify setting_sources is None
    assert_eq!(
        headless_config.setting_sources, None,
        "Coworker with restrict_setting_sources=false should not set setting_sources"
    );
}

#[test]
fn test_reviewer_config_propagates_setting_sources() {
    // Reviewers also use restrict_setting_sources=true
    let launch_config = LaunchConfig::reviewer("test-reviewer", 123);

    // Convert to headless config
    let headless_config = launch_config.to_headless_config();

    // Verify setting_sources is set
    assert_eq!(
        headless_config.setting_sources,
        Some("project,local".to_string()),
        "Reviewer should set setting_sources to 'project,local'"
    );
}

#[test]
fn test_headless_config_serialization_with_setting_sources() {
    // Verify setting_sources serializes correctly
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test".to_string(),
        json_schema: None,
        cwd: None,
        max_budget_usd: None,
        allow_tools: false,
        persist_session: false,
        resume_session_id: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        settings_path: None,
        setting_sources: Some("project,local".to_string()),
        env: std::collections::HashMap::new(),
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.setting_sources, Some("project,local".to_string()));
}
