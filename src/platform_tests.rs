//! Tests for platform arg builders.
//!
//! These tests verify the single source of truth for CLI argument construction
//! across headed (interactive) and headless (JSON streaming) launch paths.

use super::*;
use crate::auth::AuthProvider;
use crate::launch::{LaunchConfig, SessionMode};

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

#[test]
fn test_codex_common_args_include_bypass_and_model() {
    let args = build_codex_common_args(Some("gpt-5.4"), &[]);
    assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"gpt-5.4".to_string()));
}

// ── Common args ───────────────────────────────────────────────────────

#[test]
fn test_claude_common_args_always_has_skip_permissions() {
    let args = build_claude_common_args("sonnet", &[]);
    assert!(
        args.contains(&"--dangerously-skip-permissions".to_string()),
        "Common args must always include --dangerously-skip-permissions"
    );
}

#[test]
fn test_claude_common_args_always_has_model() {
    let args = build_claude_common_args("opus", &[]);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"opus".to_string()));
}

/// `--setting-sources` is NOT in common args — it's added by callers conditionally
/// because fork sessions (`--resume --fork-session`) are incompatible with it.
#[test]
fn test_claude_common_args_does_not_include_setting_sources() {
    let args = build_claude_common_args("sonnet", &[]);
    assert!(
        !args.contains(&"--setting-sources".to_string()),
        "Common args must NOT include --setting-sources (callers add it conditionally)"
    );
}

#[test]
fn test_claude_common_args_includes_add_dir() {
    let dirs = vec![PathBuf::from("/tmp/repo1"), PathBuf::from("/tmp/repo2")];
    let args = build_claude_common_args("sonnet", &dirs);
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
        settings_path: None,
        setting_sources: None, // Will be removed from HeadlessConfig in a later step
        auth_provider: AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
        disallowed_tools: vec![],
        agent_name: None,
        additional_dirs: vec![],
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
fn test_claude_headless_args_disallowed_tools_when_set() {
    let config = HeadlessConfig {
        disallowed_tools: vec!["Edit".to_string(), "Write".to_string(), "Bash".to_string()],
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        args.contains(&"--disallowedTools".to_string()),
        "Non-empty disallowed_tools should add --disallowedTools flag"
    );
    let idx = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(
        args[idx + 1],
        "Edit,Write,Bash",
        "Should pass comma-separated tool names"
    );
}

#[test]
fn test_claude_headless_args_disallowed_tools_empty_omitted() {
    let config = HeadlessConfig {
        disallowed_tools: vec![],
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);

    assert!(
        !args.contains(&"--disallowedTools".to_string()),
        "Empty disallowed_tools should not add --disallowedTools flag"
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
    let config = test_headless_config();
    let args = build_claude_headless_args(&config);

    // Common flags should be present
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--model".to_string()));
    // --setting-sources is added by build_claude_headless_args (not common), verified here
    assert!(args.contains(&"--setting-sources".to_string()));
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
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
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
        ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None)
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
        ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None)
    };
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--resume".to_string()));
    assert!(args.contains(&"abc-123".to_string()));
}

#[test]
fn test_claude_headed_args_always_has_settings() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
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
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
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
        None,
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
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    let settings = std::path::Path::new("/tmp/settings.json");
    let prompt = std::path::Path::new("/tmp/prompt.md");

    let (args, _) = build_claude_headed_args(&config, settings, prompt, None);

    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"--setting-sources".to_string()));
    assert!(args.contains(&"project,local".to_string()));
}

// ── Codex args ────────────────────────────────────────────────────────

#[test]
fn test_codex_headless_args_is_app_server() {
    let args = build_codex_headless_args();
    assert_eq!(args, vec!["app-server"]);
}

#[test]
fn test_codex_headed_args_has_resume() {
    let config = LaunchConfig {
        name: "lead".to_string(),
        session_mode: SessionMode::ResumeSession("thread-123".to_string()),
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "gpt-5.4".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: AuthProvider::Codex,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
    };
    let (args, session_id) = build_codex_headed_args(&config, "system prompt", None);
    assert_eq!(session_id, None);
    assert_eq!(args[0], "codex");
    assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert!(
        !args.contains(&"--model".to_string()),
        "ResumeSession should not override the persisted Codex thread model"
    );
    let resume_idx = args.iter().position(|arg| arg == "resume").unwrap();
    assert_eq!(args[resume_idx + 1], "thread-123");
    let config_idx = args.iter().position(|arg| arg == "-c").unwrap();
    assert_eq!(args[config_idx], "-c");
    assert!(
        args[config_idx + 1].starts_with("developer_instructions="),
        "Expected developer_instructions override, got: {}",
        args[config_idx + 1]
    );
}

#[test]
fn test_codex_headed_args_omits_override_when_prompt_empty() {
    let config = LaunchConfig {
        name: "lead".to_string(),
        session_mode: SessionMode::ResumeSession("thread-123".to_string()),
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "gpt-5.4".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: AuthProvider::Codex,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
    };
    let (args, _) = build_codex_headed_args(&config, "", None);
    assert!(args.contains(&"resume".to_string()));
    assert!(args.contains(&"thread-123".to_string()));
    assert!(!args.contains(&"-c".to_string()));
}

#[test]
fn test_codex_headed_args_resume_last_uses_last_without_model_override() {
    let config = LaunchConfig {
        name: "lead".to_string(),
        session_mode: SessionMode::Resume,
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "gpt-5.4".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: AuthProvider::Codex,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
    };

    let (args, session_id) = build_codex_headed_args(&config, "system prompt", None);

    assert_eq!(session_id, None);
    assert!(args.contains(&"resume".to_string()));
    assert!(args.contains(&"--last".to_string()));
    assert!(
        !args.contains(&"--model".to_string()),
        "resume --last should preserve the last session's saved model"
    );
}

#[test]
fn test_codex_headed_args_fresh_uses_positional_prompt() {
    let config = LaunchConfig {
        name: "park".to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: Some("ship it".to_string()),
        additional_dirs: vec![PathBuf::from("/tmp/repo2")],
        pr_number: None,
        working_dir: None,
        model: "gpt-5.1-codex-mini".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: AuthProvider::Codex,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
    };
    let (args, session_id) = build_codex_headed_args(&config, "system prompt", Some("ship it"));
    assert_eq!(session_id, None);
    assert_eq!(args[0], "codex");
    assert!(args.contains(&"--add-dir".to_string()));
    assert!(args.contains(&"/tmp/repo2".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"gpt-5.1-codex-mini".to_string()));
    assert!(
        !args.contains(&"resume".to_string()),
        "Fresh launches should not use the resume subcommand"
    );
    assert_eq!(args.last().map(String::as_str), Some("ship it"));
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

/// Verify that fork sessions do NOT include `--setting-sources`, which is
/// incompatible with `--resume --fork-session` in the Claude CLI.
#[test]
fn test_claude_headless_args_fork_session_skips_setting_sources() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-abc".to_string()),
        persist_session: true,
        fork_session: true,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);
    assert!(
        !args.contains(&"--setting-sources".to_string()),
        "Fork sessions must NOT include --setting-sources (incompatible with --fork-session)"
    );
}

/// Verify that normal (non-fork) resume sessions still include `--setting-sources`.
#[test]
fn test_claude_headless_args_normal_resume_has_setting_sources() {
    let config = HeadlessConfig {
        resume_session_id: Some("session-abc".to_string()),
        persist_session: true,
        fork_session: false,
        ..test_headless_config()
    };
    let args = build_claude_headless_args(&config);
    assert!(
        args.contains(&"--setting-sources".to_string()),
        "Normal resume sessions should include --setting-sources"
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

/// Simulate the two-step fork launch and verify args differ between steps.
///
/// Step 1 (fork): `--resume <parent> --fork-session --session-id <uuid>`,
///   NO `--setting-sources`, NO `--settings`.
/// Step 2 (resume): `--resume <fork-session-id>`, WITH `--setting-sources`,
///   NO `--fork-session`, NO `--session-id`.
///
/// This mirrors `spawn_fork()` in sessions.rs where the fork process is
/// killed after step 1 and relaunched with modified config for step 2.
#[test]
fn test_two_step_fork_launch_args_differ() {
    let parent_session_id = "parent-session-abc";
    let fork_session_id = "fork-uuid-12345678";

    // Step 1: fork config (fork_session=true, resume parent, pre-assigned session_id)
    let step1_config = HeadlessConfig {
        resume_session_id: Some(parent_session_id.to_string()),
        fork_session: true,
        session_id: Some(fork_session_id.to_string()),
        persist_session: true,
        settings_path: Some("/tmp/settings.json".to_string()),
        ..test_headless_config()
    };
    let step1_args = build_claude_headless_args(&step1_config);

    // Step 2: resume config (fork_session=false, resume fork session, no session_id)
    let step2_config = HeadlessConfig {
        resume_session_id: Some(fork_session_id.to_string()),
        fork_session: false,
        session_id: None,
        persist_session: true,
        settings_path: Some("/tmp/settings.json".to_string()),
        ..test_headless_config()
    };
    let step2_args = build_claude_headless_args(&step2_config);

    // Step 1 assertions
    assert!(
        step1_args.contains(&"--fork-session".to_string()),
        "Step 1 must have --fork-session"
    );
    assert!(
        step1_args.contains(&"--session-id".to_string()),
        "Step 1 must have --session-id for pre-assigned UUID"
    );
    assert!(
        !step1_args.contains(&"--setting-sources".to_string()),
        "Step 1 must NOT have --setting-sources (incompatible with --fork-session)"
    );
    assert!(
        !step1_args.contains(&"--settings".to_string()),
        "Step 1 must NOT have --settings (resume mode skips it)"
    );
    assert!(
        step1_args.contains(&parent_session_id.to_string()),
        "Step 1 resumes the PARENT session"
    );

    // Step 2 assertions
    assert!(
        !step2_args.contains(&"--fork-session".to_string()),
        "Step 2 must NOT have --fork-session (normal resume)"
    );
    assert!(
        step2_args.contains(&"--setting-sources".to_string()),
        "Step 2 MUST have --setting-sources (safe for normal resume)"
    );
    assert!(
        !step2_args.contains(&"--session-id".to_string()),
        "Step 2 must NOT have --session-id (resuming known session)"
    );
    assert!(
        step2_args.contains(&fork_session_id.to_string()),
        "Step 2 resumes the FORK session (not the parent)"
    );
}
