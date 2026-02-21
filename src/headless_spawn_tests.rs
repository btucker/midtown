//! Tests for HeadlessSession::spawn() CLI argument construction.
//!
//! These tests verify that the command-line arguments passed to `claude` or `codex`
//! are correct, especially around conditionally-added flags like --settings and --resume.

use super::*;

#[test]
fn test_fresh_session_uses_append_system_prompt() {
    // This test documents that fresh headless sessions should use --append-system-prompt
    // so the system prompt merges with CLAUDE.md rather than replacing it.
    //
    // The extract_spawn_args helper mirrors what HeadlessSession::spawn() does.
    // If this test fails, it means the actual spawn() implementation uses --system-prompt
    // when it should use --append-system-prompt.
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test prompt".to_string(),
        settings_path: None,
        setting_sources: None,
        persist_session: false,
        resume_session_id: None,
        session_id: None,
        allow_tools: true,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    // Should use --append-system-prompt, not --system-prompt
    let append_count = args
        .iter()
        .filter(|a| *a == "--append-system-prompt")
        .count();
    let system_count = args.iter().filter(|a| *a == "--system-prompt").count();

    assert_eq!(
        append_count, 1,
        "Fresh session should use --append-system-prompt exactly once, found {}",
        append_count
    );
    assert_eq!(
        system_count, 0,
        "Fresh session should NOT use --system-prompt (should use --append-system-prompt instead), found {}",
        system_count
    );
}

/// Helper to extract the command args from a HeadlessSession spawn attempt.
///
/// This doesn't actually spawn the process (which would require claude CLI to be available),
/// but instead captures what arguments would have been passed.
fn extract_spawn_args(config: &HeadlessConfig) -> Vec<String> {
    crate::platform::build_claude_headless_args(config)
}

#[test]
fn test_fresh_session_should_not_duplicate_settings_flag() {
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test".to_string(),
        settings_path: Some("/tmp/settings.json".to_string()),
        setting_sources: Some("project,local".to_string()),
        persist_session: false,
        resume_session_id: None,
        session_id: None,
        allow_tools: false,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    // Count occurrences of --settings and --setting-sources
    let settings_count = args.iter().filter(|a| *a == "--settings").count();
    let setting_sources_count = args.iter().filter(|a| *a == "--setting-sources").count();

    // Should appear exactly once each, not twice
    assert_eq!(
        settings_count, 1,
        "Fresh session should have --settings flag exactly once, found {}",
        settings_count
    );
    assert_eq!(
        setting_sources_count, 1,
        "Fresh session should have --setting-sources flag exactly once, found {}",
        setting_sources_count
    );
}

#[test]
fn test_resume_session_should_omit_settings_flag() {
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test".to_string(),
        settings_path: Some("/tmp/settings.json".to_string()),
        setting_sources: Some("project,local".to_string()),
        persist_session: true,
        resume_session_id: Some("session-123".to_string()),
        session_id: None,
        allow_tools: false,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    // Resume sessions should NOT have --settings (file path) to avoid "Tool names must be
    // unique" errors when plugins re-register from a settings file.
    // --setting-sources is always present (added by build_claude_common_args unconditionally).
    let settings_count = args.iter().filter(|a| *a == "--settings").count();
    let setting_sources_count = args.iter().filter(|a| *a == "--setting-sources").count();

    assert_eq!(
        settings_count, 0,
        "Resume session should NOT have --settings flag, found {}",
        settings_count
    );
    assert_eq!(
        setting_sources_count, 1,
        "--setting-sources is always added by build_claude_common_args, found {}",
        setting_sources_count
    );
}

#[test]
fn test_fresh_session_without_settings_path() {
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test".to_string(),
        settings_path: None,
        setting_sources: None,
        persist_session: false,
        resume_session_id: None,
        session_id: None,
        allow_tools: false,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    // --settings should be absent when settings_path is None.
    // --setting-sources is always present (added unconditionally by build_claude_common_args).
    let settings_count = args.iter().filter(|a| *a == "--settings").count();
    let setting_sources_count = args.iter().filter(|a| *a == "--setting-sources").count();

    assert_eq!(settings_count, 0, "Should not add --settings when None");
    assert_eq!(
        setting_sources_count, 1,
        "--setting-sources is always added by build_claude_common_args, found {}",
        setting_sources_count
    );
}

#[test]
fn test_fresh_session_with_preassigned_session_id_includes_session_id_flag() {
    // When a session_id is pre-assigned on a fresh session, the CLI should receive
    // --session-id <uuid> so the daemon controls the session ID immediately without
    // waiting for the init event (eliminating the race window).
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test prompt".to_string(),
        settings_path: None,
        setting_sources: None,
        persist_session: true,
        resume_session_id: None,
        session_id: Some("pre-assigned-uuid-1234".to_string()),
        allow_tools: true,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    let session_id_pos = args.iter().position(|a| a == "--session-id");
    assert!(
        session_id_pos.is_some(),
        "Fresh session with pre-assigned session_id should include --session-id flag"
    );
    if let Some(pos) = session_id_pos {
        assert_eq!(
            args.get(pos + 1).map(|s| s.as_str()),
            Some("pre-assigned-uuid-1234"),
            "--session-id should be followed by the pre-assigned UUID"
        );
    }
}

#[test]
fn test_fresh_session_without_preassigned_session_id_omits_session_id_flag() {
    // Without a pre-assigned session_id, fresh headless sessions should NOT include
    // --session-id (Claude CLI generates its own).
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test prompt".to_string(),
        settings_path: None,
        setting_sources: None,
        persist_session: true,
        resume_session_id: None,
        session_id: None,
        allow_tools: true,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    let has_session_id = args.iter().any(|a| a == "--session-id");
    assert!(
        !has_session_id,
        "Fresh session without pre-assigned session_id should NOT include --session-id flag"
    );
}

#[test]
fn test_resume_session_does_not_use_session_id_flag() {
    // Resume sessions use --resume <id>, not --session-id.
    // Even if session_id is set, it should be ignored for resume sessions.
    let config = HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "test".to_string(),
        settings_path: None,
        setting_sources: None,
        persist_session: true,
        resume_session_id: Some("existing-session-456".to_string()),
        session_id: Some("should-be-ignored".to_string()),
        allow_tools: true,
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    };

    let args = extract_spawn_args(&config);

    assert!(
        !args.iter().any(|a| a == "--session-id"),
        "Resume session should use --resume, not --session-id"
    );
    assert!(
        args.iter().any(|a| a == "--resume"),
        "Resume session should use --resume flag"
    );
}
