//! Tests for agent commands (attach, detach, target parsing, CLI arg construction).

use super::*;

/// Return the project root directory (a git repo).
///
/// Uses CARGO_MANIFEST_DIR which is set at compile time, making this
/// independent of the process CWD (which other tests may change).
fn find_project_root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn git_head(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn attach_options<'a>(
    profile: Option<&'a str>,
    coworker_type: Option<&'a str>,
    channel: Option<&'a str>,
    include_detach: bool,
) -> AttachShellOptions<'a> {
    AttachShellOptions {
        launch: attach_launch_options(profile, coworker_type, channel),
        include_detach,
    }
}

fn attach_launch_options<'a>(
    profile: Option<&'a str>,
    coworker_type: Option<&'a str>,
    channel: Option<&'a str>,
) -> AttachLaunchOptions<'a> {
    AttachLaunchOptions {
        profile,
        coworker_type,
        channel,
    }
}
// ── Target normalization ──────────────────────────────────────────────

#[test]
fn normalize_attach_target_accepts_two_token_name() {
    let args = AttachArgs {
        target: "name".to_string(),
        value: Some("Park".to_string()),
    };
    assert_eq!(normalize_attach_target(&args).unwrap(), "name/Park");
}

#[test]
fn normalize_attach_target_accepts_provider_alias() {
    let args = AttachArgs {
        target: "openai".to_string(),
        value: Some("thread-1".to_string()),
    };
    assert_eq!(normalize_attach_target(&args).unwrap(), "codex/thread-1");
}

#[test]
fn normalize_attach_target_rejects_zai_platform_selector() {
    let args = AttachArgs {
        target: "zai".to_string(),
        value: Some("abc-123".to_string()),
    };
    assert!(normalize_attach_target(&args).is_err());
}

#[test]
fn normalize_single_target_defaults_to_name() {
    assert_eq!(normalize_single_target("madison").unwrap(), "name/madison");
}

#[test]
fn normalize_single_target_platform_only() {
    assert_eq!(normalize_single_target("codex").unwrap(), "codex");
    assert_eq!(normalize_single_target("openai").unwrap(), "codex");
    assert_eq!(normalize_single_target("claude").unwrap(), "claude");
}

#[test]
fn normalize_single_target_supports_slash_syntax() {
    assert_eq!(normalize_single_target("task/42").unwrap(), "task/42");
    assert_eq!(
        normalize_single_target("ANTHROPIC/abc").unwrap(),
        "claude/abc"
    );
}

#[test]
fn normalize_single_target_rejects_missing_value() {
    assert!(normalize_single_target("task/").is_err());
    assert!(normalize_single_target("name:").is_err());
}

// ── Provider / target helpers ─────────────────────────────────────────

#[test]
fn parse_provider_accepts_aliases() {
    assert_eq!(
        parse_provider("anthropic"),
        midtown::auth::AuthProvider::Claude
    );
    assert_eq!(parse_provider("openai"), midtown::auth::AuthProvider::Codex);
    assert_eq!(parse_provider("z.ai"), midtown::auth::AuthProvider::Zai);
}

#[test]
fn platform_session_target_detection() {
    assert!(is_platform_session_target("claude/abc"));
    assert!(is_platform_session_target("codex/thread-1"));
    assert!(!is_platform_session_target("name/park"));
    assert!(!is_platform_session_target("task/42"));
    assert!(!is_platform_session_target("codex"));
}

#[test]
fn provider_from_target_platform() {
    assert_eq!(
        provider_from_target("codex"),
        midtown::auth::AuthProvider::Codex
    );
    assert_eq!(
        provider_from_target("codex/anything"),
        midtown::auth::AuthProvider::Codex
    );
    assert_eq!(
        provider_from_target("name/madison"),
        midtown::auth::AuthProvider::Claude
    );
}

// ── Misc helpers ──────────────────────────────────────────────────────

#[test]
fn format_age_ms_compacts_units() {
    assert_eq!(format_age_ms(999), "999ms");
    assert_eq!(format_age_ms(1_500), "1.5s");
    assert_eq!(format_age_ms(90_000), "1.5m");
}

// ── LaunchConfig::to_cli_args ─────────────────────────────────────────

#[test]
fn test_to_cli_args_resume_includes_all_flags() {
    let config = midtown::launch::LaunchConfig {
        name: "lead".to_string(),
        session_mode: midtown::launch::SessionMode::Resume,
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "opus".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
        avatar_badge: None,
    };

    let settings = std::env::temp_dir().join("test-cli-args-settings.json");
    let prompt = std::env::temp_dir().join("test-cli-args-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test prompt");

    let (args, session_id) = config.to_cli_args(&settings, &prompt, None);

    assert!(
        args.len() >= 7,
        "Should have at least 7 arguments, got: {:?}",
        args
    );
    assert_eq!(args[0], "claude");
    assert!(
        args.contains(&"--continue".to_string()),
        "Should include --continue flag"
    );
    assert!(
        args.contains(&"--dangerously-skip-permissions".to_string()),
        "Should include --dangerously-skip-permissions flag"
    );
    assert!(
        args.contains(&"--settings".to_string()),
        "Should include --settings flag"
    );
    assert!(
        args.contains(&"--append-system-prompt".to_string()),
        "Should include --append-system-prompt flag"
    );
    assert!(session_id.is_none());
}

#[test]
fn test_to_cli_args_fresh_generates_session_id() {
    let config = midtown::launch::LaunchConfig {
        name: "park".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
        avatar_badge: None,
    };

    let settings = std::env::temp_dir().join("test-cli-args-settings2.json");
    let prompt = std::env::temp_dir().join("test-cli-args-prompt2.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test prompt");

    let (args, session_id) = config.to_cli_args(&settings, &prompt, None);

    assert!(
        args.contains(&"--session-id".to_string()),
        "Fresh mode should include --session-id"
    );
    assert!(
        session_id.is_some(),
        "Fresh mode should return a session ID"
    );
    assert!(
        args.contains(&"--setting-sources".to_string()),
        "Coworker should include --setting-sources"
    );
}

#[test]
fn test_to_cli_args_coworker_restricts_settings() {
    let config = midtown::launch::LaunchConfig {
        name: "park".to_string(),
        session_mode: midtown::launch::SessionMode::Resume,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
        avatar_badge: None,
    };

    let settings = std::env::temp_dir().join("test-settings.json");
    let prompt = std::env::temp_dir().join("test-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test");

    let (args, _) = config.to_cli_args(&settings, &prompt, None);

    assert!(args.contains(&"--setting-sources".to_string()));
    assert!(args.contains(&"project,local".to_string()));
}

// ── build_attach_shell_command ────────────────────────────────────────

#[test]
fn test_build_attach_launch_spec_returns_program_and_args() {
    let cwd = find_project_root();
    let spec = build_attach_launch_spec(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Codex,
        "thread-xyz",
        attach_launch_options(None, Some("lead"), None),
    )
    .expect("build_attach_launch_spec should succeed");

    assert!(
        !spec.program.is_empty(),
        "attach launch program should not be empty"
    );
    assert!(
        spec.args.iter().any(|arg| arg == "thread-xyz"),
        "attach launch args should include the resumed session id, got: {:?}",
        spec.args
    );
}

#[test]
fn test_build_attach_launch_spec_includes_agent_env() {
    let cwd = find_project_root();
    let spec = build_attach_launch_spec(
        &cwd,
        "park",
        midtown::auth::AuthProvider::Codex,
        "thread-abc",
        attach_launch_options(None, None, None),
    )
    .expect("build_attach_launch_spec should succeed");

    assert_eq!(spec.env.get("MIDTOWN_AGENT"), Some(&"park".to_string()));
    assert_eq!(spec.env.get("DISABLE_AUTOUPDATER"), Some(&"1".to_string()));
}

#[test]
fn test_lead_attach_includes_system_prompt() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        command.contains("--append-system-prompt"),
        "Lead attach command should include --append-system-prompt flag, got: {}",
        command
    );
    assert!(
        command.contains("$(cat") && command.contains("midtown-attach-"),
        "Should use temp file pattern $(cat .../midtown-attach-...), got: {}",
        command
    );
}

#[test]
fn test_coworker_attach_includes_system_prompt() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "park",
        midtown::auth::AuthProvider::Claude,
        "session-456",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        command.contains("--append-system-prompt"),
        "Coworker attach command should include --append-system-prompt flag, got: {}",
        command
    );
    assert!(
        command.contains("$(cat") && command.contains("midtown-attach-"),
        "Should use temp file pattern $(cat .../midtown-attach-...), got: {}",
        command
    );
}

// ── Model flag in to_cli_args ─────────────────────────────────────────

#[test]
fn test_to_cli_args_includes_model_flag() {
    let config = midtown::launch::LaunchConfig {
        name: "lead".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "opus".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
        avatar_badge: None,
    };

    let settings = std::env::temp_dir().join("test-model-settings.json");
    let prompt = std::env::temp_dir().join("test-model-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test");

    let (args, _) = config.to_cli_args(&settings, &prompt, None);

    assert!(
        args.contains(&"--model".to_string()),
        "to_cli_args should include --model flag, got: {:?}",
        args
    );
    assert!(
        args.contains(&"opus".to_string()),
        "to_cli_args should pass the configured model value, got: {:?}",
        args
    );
}

#[test]
fn test_to_cli_args_coworker_gets_sonnet_model() {
    let config = midtown::launch::LaunchConfig {
        name: "park".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        agent_type: "midtown-code-author".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
        suppress_auto_output: false,
        color: None,
        icon: None,
        avatar_badge: None,
    };

    let settings = std::env::temp_dir().join("test-model-settings2.json");
    let prompt = std::env::temp_dir().join("test-model-prompt2.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test");

    let (args, _) = config.to_cli_args(&settings, &prompt, None);

    assert!(
        args.contains(&"--model".to_string()),
        "to_cli_args should include --model flag"
    );
    assert!(
        args.contains(&"sonnet".to_string()),
        "Coworker should use sonnet model, got: {:?}",
        args
    );
}

// ── Reviewer attach role ──────────────────────────────────────────────

#[test]
fn test_reviewer_attach_gets_reviewer_system_prompt() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "york",
        midtown::auth::AuthProvider::Claude,
        "session-789",
        attach_options(None, Some("reviewer"), None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        command.contains("--setting-sources"),
        "Reviewer attach should restrict setting sources, got: {}",
        command
    );
}

#[test]
fn test_lead_attach_gets_opus_model() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, Some("lead"), None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // The model should be resolved from config (e.g., "large", "opus")
    // and passed as --model. Accept any model that's appropriate for the lead role.
    assert!(
        command.contains("--model"),
        "Lead attach should include --model flag, got: {}",
        command
    );
}

// ── Channel lead attach role ──────────────────────────────────────────

#[test]
fn test_channel_lead_attach_gets_channel_lead_role() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "ops",
        midtown::auth::AuthProvider::Claude,
        "session-ops-123",
        attach_options(None, Some("channel-lead"), Some("ops"), true),
    );

    let command = result.expect("build_attach_shell_command should succeed for channel lead");

    assert!(
        command.contains("--append-system-prompt"),
        "Channel lead attach should include --append-system-prompt flag, got: {}",
        command
    );

    assert!(
        command.contains("--model") && command.contains("haiku"),
        "ops channel lead attach should use haiku model, got: {}",
        command
    );

    let cat_start = command
        .find("$(cat ")
        .expect("Command should contain $(cat pattern");
    let path_start = cat_start + "$(cat ".len();
    let path_end = command[path_start..]
        .find(')')
        .expect("$(cat ...) should be closed");
    let prompt_file_path = command[path_start..path_start + path_end].trim_matches('"');

    let prompt_contents = std::fs::read_to_string(prompt_file_path).unwrap_or_else(|e| {
        panic!(
            "Should be able to read system prompt temp file at '{}': {}",
            prompt_file_path, e
        )
    });

    assert!(
        prompt_contents.contains("ops"),
        "Channel lead system prompt should contain the channel name 'ops', got: {}",
        prompt_contents
    );
}

#[test]
fn test_codex_lead_attach_includes_developer_instructions_override() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Codex,
        "thread-123",
        attach_options(None, Some("lead"), None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        command.contains("codex"),
        "Codex attach command should invoke codex, got: {}",
        command
    );
    assert!(
        command.contains(" resume ") && command.contains("thread-123"),
        "Codex attach should include resume thread id, got: {}",
        command
    );
    assert!(
        command.contains("--dangerously-bypass-approvals-and-sandbox"),
        "Codex attach should bypass Codex-managed approvals inside Midtown, got: {}",
        command
    );
    assert!(
        command.contains("developer_instructions="),
        "Codex attach should pass developer_instructions override, got: {}",
        command
    );
}

#[test]
fn test_codex_attach_does_not_use_claude_system_prompt_flag() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "park",
        midtown::auth::AuthProvider::Codex,
        "thread-456",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");
    assert!(
        !command.contains("--append-system-prompt"),
        "Codex attach should not use Claude-specific --append-system-prompt, got: {}",
        command
    );
    assert!(
        !command.contains(" --model "),
        "Codex attach should preserve the resumed thread model instead of overriding it, got: {}",
        command
    );
}

#[test]
fn test_attach_uses_explicit_profile_when_provided() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Codex,
        "thread-123",
        attach_options(Some("work@example.com"), Some("lead"), None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");
    let expected =
        midtown::auth::profile_dir_for(midtown::auth::AuthProvider::Codex, "work@example.com");
    assert!(
        command.contains(expected.to_string_lossy().as_ref()),
        "Attach command should use the persisted profile path, got: {}",
        command
    );
}

// ── Shell quoting ──────────────────────────────────────────────────────

#[test]
fn test_shell_quote_does_not_double_escape_dollar_command() {
    let raw_arg = "$(cat /tmp/file.txt)";
    let quoted = shell_quote(raw_arg);

    let output = std::process::Command::new("sh")
        .args(["-lc", &format!("echo {}", quoted)])
        .output()
        .expect("shell should parse");

    let _stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !quoted.contains("\"$"),
        "Quoted arg should not have escaped $ with backslash, got: {}",
        quoted
    );
}

#[test]
fn test_build_attach_command_uses_shell_command_substitution() {
    let result = build_attach_shell_command(
        &find_project_root(),
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        command.contains("$(cat "),
        "Command should contain $(cat pattern for file reading"
    );

    let correct_pattern = "'\"$(cat ";
    assert!(
        command.contains(correct_pattern),
        "Command should have correct quoting pattern '{}', got: {}",
        correct_pattern,
        command
    );

    assert!(
        command.contains(")\"'"),
        "Command should have closing quote pattern ')\"\\'', got: {}",
        command
    );
}

#[test]
fn test_shell_quote_handles_single_quotes() {
    let quoted = shell_quote("it's a test");
    assert!(
        quoted.contains("'"),
        "Quoted string should contain quotes: {}",
        quoted
    );
    let output = std::process::Command::new("sh")
        .args(["-lc", &format!("printf '%s' {}", quoted)])
        .output()
        .expect("shell should parse quoted string");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "it's a test",
        "Shell should unquote back to original"
    );
}

#[test]
fn test_shell_quote_handles_paths_with_spaces() {
    let path = "/var/folders/My Documents/test file.txt";
    let quoted = shell_quote(path);
    let output = std::process::Command::new("sh")
        .args(["-lc", &format!("printf '%s' {}", quoted)])
        .output()
        .expect("shell should parse quoted string");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        path,
        "Shell should unquote back to original path"
    );
}

#[test]
fn test_build_attach_command_no_double_quoting() {
    let temp = tempfile::TempDir::new().unwrap();
    let spaced_dir = temp.path().join("my test dir with spaces");
    std::fs::create_dir_all(&spaced_dir).unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&spaced_dir)
        .output()
        .expect("git init should succeed");

    let result = build_attach_shell_command(
        &spaced_dir.to_string_lossy(),
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    assert!(
        !command.contains("''\"'\"'"),
        "Command should not contain double-escaped quotes (old pattern): {}",
        command
    );
    assert!(
        !command.contains("'\\''\\''"),
        "Command should not contain double-escaped quotes (new pattern): {}",
        command
    );

    if let Some(sh_lc_start) = command.find("sh -lc ") {
        let after_sh_lc = &command[sh_lc_start + 7..];
        assert!(
            after_sh_lc.starts_with("'") || after_sh_lc.starts_with("\""),
            "sh -lc argument should be quoted, got: {}",
            after_sh_lc
        );
    }
}

#[test]
fn test_build_attach_command_shell_parseable() {
    let result = build_attach_shell_command(
        &find_project_root(),
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, true),
    );

    let command = result.expect("build_attach_shell_command should succeed");

    let parse_result = std::process::Command::new("sh")
        .args(["-n", "-c", &command])
        .status();

    assert!(
        parse_result.is_ok() && parse_result.unwrap().success(),
        "Command should be parseable by shell, got error for: {}",
        command
    );
}

// ── include_detach flag (fix for dual-lead bug #1428) ────────────────

#[test]
fn test_view_attach_command_omits_agent_detach() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, false),
    );
    let command = result.expect("build_attach_shell_command should succeed");
    assert!(
        !command.contains("agent detach"),
        "midtown view shell command must NOT contain `agent detach` (dual-lead bug #1428), got: {}",
        command
    );
}

#[test]
fn test_standalone_attach_command_includes_agent_detach() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        attach_options(None, None, None, true),
    );
    let command = result.expect("build_attach_shell_command should succeed");
    assert!(
        command.contains("agent detach"),
        "standalone attach shell command must contain `agent detach`, got: {}",
        command
    );
}

// ── Worktree management ───────────────────────────────────────────────

#[test]
fn ensure_attach_worktree_lead_updates_to_head() {
    use std::process::Command as TestCmd;
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let repo = temp.path();

    TestCmd::new("git")
        .args(["init"])
        .current_dir(repo)
        .output()
        .unwrap();
    TestCmd::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(repo)
        .output()
        .unwrap();
    TestCmd::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo)
        .output()
        .unwrap();
    TestCmd::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(repo)
        .output()
        .unwrap();

    let manager =
        midtown::worktree::WorktreeManager::new(repo.to_path_buf()).expect("create manager");

    let wt = manager.create_lead_worktree().expect("create lead wt");
    let initial_head = git_head(repo);
    assert_eq!(git_head(&wt), initial_head);

    TestCmd::new("git")
        .args(["commit", "--allow-empty", "-m", "second"])
        .current_dir(repo)
        .output()
        .unwrap();
    let new_head = git_head(repo);
    assert_ne!(initial_head, new_head);

    let result = ensure_attach_worktree("lead", &wt.to_string_lossy(), true);
    assert!(result.is_ok());
    assert_eq!(
        git_head(&wt),
        new_head,
        "ensure_attach_worktree should update lead to HEAD"
    );
}

#[test]
fn ensure_attach_worktree_coworker_falls_back_to_daemon_cwd() {
    let result = ensure_attach_worktree("park", "/tmp/some-cwd", false);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "/tmp/some-cwd");
}

// ── AgentCommand::Fork CLI parsing ────────────────────────────────

/// Minimal wrapper to parse `agent <subcommand>` via clap.
#[derive(clap::Parser)]
struct TestAgentCli {
    #[command(subcommand)]
    command: AgentCommand,
}

#[test]
fn fork_parses_thread_id_flag() {
    use clap::Parser;
    let cli = TestAgentCli::try_parse_from(["test", "fork", "--thread-id", "msg-123"]).unwrap();
    match cli.command {
        AgentCommand::Fork {
            thread_id,
            session_id,
            name,
            initial_message,
            color: _,
            icon: _,
        } => {
            assert_eq!(thread_id, "msg-123");
            assert!(session_id.is_none());
            assert!(name.is_none());
            assert!(initial_message.is_none());
        }
        other => panic!("Expected Fork, got {:?}", other),
    }
}

#[test]
fn fork_rejects_positional_thread_id() {
    use clap::Parser;
    let result = TestAgentCli::try_parse_from(["test", "fork", "msg-123"]);
    assert!(
        result.is_err(),
        "Positional thread ID should be rejected after rename to --thread-id flag"
    );
}

#[test]
fn fork_parses_all_flags() {
    use clap::Parser;
    let cli = TestAgentCli::try_parse_from([
        "test",
        "fork",
        "--thread-id",
        "thread-abc",
        "--session-id",
        "sess-456",
        "--name",
        "investigate auth bug",
    ])
    .unwrap();
    match cli.command {
        AgentCommand::Fork {
            thread_id,
            session_id,
            name,
            initial_message,
            color: _,
            icon: _,
        } => {
            assert_eq!(thread_id, "thread-abc");
            assert_eq!(session_id.as_deref(), Some("sess-456"));
            assert_eq!(name.as_deref(), Some("investigate auth bug"));
            assert!(initial_message.is_none());
        }
        other => panic!("Expected Fork, got {:?}", other),
    }
}

#[test]
fn fork_parses_initial_message_flag() {
    use clap::Parser;
    let cli = TestAgentCli::try_parse_from([
        "test",
        "fork",
        "--thread-id",
        "thread-xyz",
        "--initial-message",
        "Investigate the auth bug in login.rs",
    ])
    .unwrap();
    match cli.command {
        AgentCommand::Fork {
            thread_id,
            initial_message,
            ..
        } => {
            assert_eq!(thread_id, "thread-xyz");
            assert_eq!(
                initial_message.as_deref(),
                Some("Investigate the auth bug in login.rs")
            );
        }
        other => panic!("Expected Fork, got {:?}", other),
    }
}

#[test]
fn fork_requires_thread_id_flag() {
    use clap::Parser;
    let result = TestAgentCli::try_parse_from(["test", "fork"]);
    assert!(
        result.is_err(),
        "Fork without --thread-id should fail (required arg)"
    );
}

// ── List / ls alias ───────────────────────────────────────────────────

#[test]
fn parse_agent_list() {
    use clap::Parser;
    let cli = TestAgentCli::try_parse_from(["test", "list"]).unwrap();
    assert!(matches!(cli.command, AgentCommand::List));
}

#[test]
fn parse_agent_ls_alias() {
    use clap::Parser;
    let cli = TestAgentCli::try_parse_from(["test", "ls"]).unwrap();
    assert!(matches!(cli.command, AgentCommand::List));
}

// ── Upload image tests ────────────────────────────────────────────────

#[test]
fn upload_image_fails_for_missing_file() {
    let result = handle_upload_image("/tmp/nonexistent-midtown-test-image.png", "screenshot");
    assert!(result.is_err(), "Should fail for missing file");
    let err = result.unwrap_err();
    assert!(
        err.contains("File not found"),
        "Error should mention file not found, got: {}",
        err
    );
}

#[test]
fn upload_to_github_fails_gracefully_in_test_env() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"fake image data").unwrap();

    let result = upload_to_github(tmp.path(), "png");
    assert!(
        result.is_err(),
        "upload_to_github should fail gracefully in test environment"
    );

    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Error message should be non-empty");
}
