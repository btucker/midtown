//! Tests for platform arg builders.
//!
//! These tests verify the single source of truth for CLI argument construction
//! across headed (interactive) and headless (JSON streaming) launch paths.

use super::*;
use crate::auth::AuthProvider;

// ── Platform enum ─────────────────────────────────────────────────────

#[test]
fn test_platform_from_provider_claude_is_claude() {
    assert_eq!(
        Platform::from_provider(AuthProvider::Claude),
        Platform::Claude
    );
}

#[test]
fn test_platform_from_provider_zai_is_claude() {
    // z.ai uses the same claude binary, just different auth
    assert_eq!(Platform::from_provider(AuthProvider::Zai), Platform::Claude);
}

#[test]
fn test_platform_from_provider_codex_is_codex() {
    assert_eq!(
        Platform::from_provider(AuthProvider::Codex),
        Platform::Codex
    );
}

#[test]
fn test_binary_name_claude() {
    assert_eq!(Platform::Claude.binary_name(), "claude");
}

#[test]
fn test_binary_name_codex() {
    assert_eq!(Platform::Codex.binary_name(), "codex");
}

// ── Common args ───────────────────────────────────────────────────────

#[test]
fn test_claude_common_args_always_has_skip_permissions() {
    let args = build_claude_common_args("sonnet", None, None, None, &[]);
    assert!(
        args.contains(&"--dangerously-skip-permissions".to_string()),
        "Common args must always include --dangerously-skip-permissions"
    );
}

#[test]
fn test_claude_common_args_always_has_model() {
    let args = build_claude_common_args("opus", None, None, None, &[]);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"opus".to_string()));
}

#[test]
fn test_claude_common_args_always_has_setting_sources() {
    let args = build_claude_common_args("sonnet", None, None, None, &[]);
    assert!(
        args.contains(&"--setting-sources".to_string()),
        "Common args must always include --setting-sources"
    );
    assert!(
        args.contains(&"project,local".to_string()),
        "Setting sources must be project,local"
    );
}

#[test]
fn test_claude_common_args_includes_agent_teams_when_set() {
    let args = build_claude_common_args(
        "sonnet",
        Some("midtown-myrepo"),
        Some("park@midtown-myrepo"),
        Some("park"),
        &[],
    );
    assert!(args.contains(&"--agent-id".to_string()));
    assert!(args.contains(&"park@midtown-myrepo".to_string()));
    assert!(args.contains(&"--agent-name".to_string()));
    assert!(args.contains(&"park".to_string()));
    assert!(args.contains(&"--team-name".to_string()));
    assert!(args.contains(&"midtown-myrepo".to_string()));
}

#[test]
fn test_claude_common_args_omits_agent_teams_when_no_team() {
    let args = build_claude_common_args("sonnet", None, None, None, &[]);
    assert!(!args.contains(&"--agent-id".to_string()));
    assert!(!args.contains(&"--agent-name".to_string()));
    assert!(!args.contains(&"--team-name".to_string()));
}

#[test]
fn test_claude_common_args_includes_add_dir() {
    let dirs = vec![PathBuf::from("/tmp/repo1"), PathBuf::from("/tmp/repo2")];
    let args = build_claude_common_args("sonnet", None, None, None, &dirs);
    let add_dir_count = args.iter().filter(|a| *a == "--add-dir").count();
    assert_eq!(add_dir_count, 2, "Should have --add-dir for each directory");
    assert!(args.contains(&"/tmp/repo1".to_string()));
    assert!(args.contains(&"/tmp/repo2".to_string()));
}

// ── Headless args ─────────────────────────────────────────────────────

fn test_headless_config() -> HeadlessConfig {
    HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "You are a test assistant.".to_string(),
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        allow_tools: true,
        persist_session: false,
        resume_session_id: None,
        session_id: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        settings_path: None,
        setting_sources: None, // Will be removed from HeadlessConfig in a later step
        auth_provider: AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
    }
}

#[test]
fn test_claude_headless_args_fresh_has_pipe_mode_and_stream_json() {
    let config = test_headless_config();
    let args = build_claude_headless_args(&config);

    assert!(
        args.contains(&"-p".to_string()),
        "Fresh session should use -p flag"
    );
    assert!(args.contains(&"--verbose".to_string()));
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(args.contains(&"--input-format".to_string()));
}

#[test]
fn test_claude_headless_args_fresh_has_system_prompt() {
    let config = test_headless_config();
    let args = build_claude_headless_args(&config);

    assert!(
        args.contains(&"--append-system-prompt".to_string()),
        "Fresh session should use --append-system-prompt"
    );
    assert!(
        args.contains(&"You are a test assistant.".to_string()),
        "Should include the system prompt text"
    );
}

#[test]
fn test_claude_headless_args_resume_skips_settings() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-123".to_string()),
        persist_session: true,
        settings_path: Some("/tmp/settings.json".to_string()),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        !args.contains(&"--settings".to_string()),
        "Resume session should NOT have --settings flag"
    );
    assert!(
        !args.contains(&"--append-system-prompt".to_string()),
        "Resume session should NOT have --append-system-prompt"
    );
    assert!(
        !args.contains(&"-p".to_string()),
        "Resume session should NOT have -p flag"
    );
}

#[test]
fn test_claude_headless_args_resume_has_resume_flag() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-123".to_string()),
        persist_session: true,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(args.contains(&"--resume".to_string()));
    assert!(args.contains(&"session-123".to_string()));
}

#[test]
fn test_claude_headless_args_no_session_persistence() {
    let config = HeadlessConfig {
        persist_session: false,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        args.contains(&"--no-session-persistence".to_string()),
        "Non-persistent session should have --no-session-persistence"
    );
}

#[test]
fn test_claude_headless_args_persistent_session_omits_flag() {
    let config = HeadlessConfig {
        persist_session: true,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        !args.contains(&"--no-session-persistence".to_string()),
        "Persistent session should NOT have --no-session-persistence"
    );
}

#[test]
fn test_claude_headless_args_tools_disabled() {
    let config = HeadlessConfig {
        allow_tools: false,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        args.contains(&"--tools".to_string()),
        "Tools disabled should add --tools flag"
    );
    // --tools is followed by empty string
    let tools_idx = args.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(
        args[tools_idx + 1],
        "",
        "Should pass empty string to --tools"
    );
}

#[test]
fn test_claude_headless_args_budget_when_set() {
    let config = HeadlessConfig {
        max_budget_usd: Some(0.50),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(args.contains(&"--max-budget-usd".to_string()));
    assert!(args.contains(&"0.5".to_string()));
}

#[test]
fn test_claude_headless_args_fresh_includes_settings_path() {
    let config = HeadlessConfig {
        settings_path: Some("/tmp/test-settings.json".to_string()),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(args.contains(&"--settings".to_string()));
    assert!(args.contains(&"/tmp/test-settings.json".to_string()));
}

#[test]
fn test_claude_headless_args_fresh_includes_json_schema() {
    let config = HeadlessConfig {
        json_schema: Some(serde_json::json!({"type": "object"})),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(args.contains(&"--json-schema".to_string()));
}

#[test]
fn test_claude_headless_args_has_common_flags() {
    let config = HeadlessConfig {
        team_name: Some("midtown-myrepo".to_string()),
        agent_id: Some("park@midtown-myrepo".to_string()),
        agent_name: Some("park".to_string()),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    // Common flags should be present
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"--setting-sources".to_string()));
    assert!(args.contains(&"--agent-id".to_string()));
    assert!(args.contains(&"--team-name".to_string()));
}

#[test]
fn test_claude_headless_args_no_duplicate_flags() {
    let config = HeadlessConfig {
        settings_path: Some("/tmp/settings.json".to_string()),
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    let settings_count = args.iter().filter(|a| *a == "--settings").count();
    assert_eq!(
        settings_count, 1,
        "Should have --settings exactly once, found {}",
        settings_count
    );

    let setting_sources_count = args.iter().filter(|a| *a == "--setting-sources").count();
    assert_eq!(
        setting_sources_count, 1,
        "Should have --setting-sources exactly once, found {}",
        setting_sources_count
    );
}

// ── Headed args ───────────────────────────────────────────────────────

#[test]
fn test_claude_headed_args_fresh_has_session_id() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, session_id) = build_claude_headed_args(&config, settings, prompt, None);

    assert_eq!(args[0], "claude", "First arg should be binary name");
    assert!(args.contains(&"--session-id".to_string()));
    assert!(session_id.is_some());
}

#[test]
fn test_claude_headed_args_resume_has_continue() {
    let config = LaunchConfig {
        session_mode: SessionMode::Resume,
        ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
    };
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, session_id) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--continue".to_string()));
    assert!(session_id.is_none());
}

#[test]
fn test_claude_headed_args_resume_session_has_resume() {
    let config = LaunchConfig {
        session_mode: SessionMode::ResumeSession("abc-123".to_string()),
        ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
    };
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--resume".to_string()));
    assert!(args.contains(&"abc-123".to_string()));
}

#[test]
fn test_claude_headed_args_always_has_settings() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(
        args.contains(&"--settings".to_string()),
        "Headed sessions always include --settings"
    );
}

#[test]
fn test_claude_headed_args_always_has_system_prompt() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--append-system-prompt".to_string()));
}

#[test]
fn test_claude_headed_args_initial_prompt_is_last() {
    let config = LaunchConfig::coworker(
        "park",
        "myrepo",
        SessionMode::Fresh,
        Some("Do the thing".to_string()),
    );
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");
    let initial = std::path::Path::new("/tmp/initial.txt");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, Some(initial));

    let last = args.last().unwrap();
    assert!(
        last.contains("initial.txt"),
        "Initial prompt should be the last argument, got: {}",
        last
    );
}

#[test]
fn test_claude_headed_args_has_common_flags() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"--setting-sources".to_string()));
    assert!(args.contains(&"project,local".to_string()));
}

#[test]
fn test_claude_headed_args_with_agent_teams() {
    let config = LaunchConfig::coworker("lexington", "myrepo", SessionMode::Fresh, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--agent-id".to_string()));
    assert!(args.contains(&"lexington@midtown-myrepo".to_string()));
    assert!(args.contains(&"--agent-name".to_string()));
    assert!(args.contains(&"lexington".to_string()));
    assert!(args.contains(&"--team-name".to_string()));
    assert!(args.contains(&"midtown-myrepo".to_string()));
}

// ── Codex args ────────────────────────────────────────────────────────

#[test]
fn test_codex_headless_args_is_app_server() {
    let args = build_codex_headless_args();
    assert_eq!(args, vec!["app-server"]);
}

#[test]
fn test_codex_headed_args_has_resume() {
    let args = build_codex_headed_args("thread-123");
    assert_eq!(args, vec!["--resume", "thread-123"]);
}

// ── Fork session flag ──────────────────────────────────────────────────

/// Verify that `--fork-session` is added when `fork_session: true` in a resume config.
#[test]
fn test_claude_headless_args_fork_session_adds_flag() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-abc".to_string()),
        persist_session: true,
        fork_session: true,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);
    assert!(
        args.contains(&"--resume".to_string()),
        "--resume should be present when resume_session_id is set"
    );
    assert!(
        args.contains(&"--fork-session".to_string()),
        "--fork-session should be added when fork_session is true"
    );
}

/// Verify that `--fork-session` is NOT added when `fork_session: false`.
#[test]
fn test_claude_headless_args_no_fork_session_without_flag() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-abc".to_string()),
        persist_session: true,
        fork_session: false,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);
    assert!(
        args.contains(&"--resume".to_string()),
        "--resume should be present when resume_session_id is set"
    );
    assert!(
        !args.contains(&"--fork-session".to_string()),
        "--fork-session should NOT be present when fork_session is false"
    );
}

/// Verify that `--fork-session` is NOT added for fresh (non-resume) sessions
/// even if fork_session is true — forking only applies to resumes.
#[test]
fn test_claude_headless_args_fork_session_ignored_for_fresh() {
    let config = HeadlessConfig {
        resume_session_id: None,
        fork_session: true, // ignored — no resume session
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);
    assert!(
        !args.contains(&"--fork-session".to_string()),
        "--fork-session should not appear for fresh (non-resume) sessions"
    );
}
