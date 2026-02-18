//! Tests for session commands (attach, detach, target parsing, CLI arg construction).

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

// ── Pane host detection ───────────────────────────────────────────────

#[test]
fn detect_host_prefers_zellij_over_tmux() {
    let host = detect_pane_host_from(|k| match k {
        "ZELLIJ" => Some("1".to_string()),
        "TMUX" => Some("/tmp/tmux-1/default,123,0".to_string()),
        _ => None,
    });
    assert_eq!(host, PaneHost::Zellij);
}

#[test]
fn detect_host_tmux_from_env() {
    let host = detect_pane_host_from(|k| match k {
        "TMUX" => Some("/tmp/tmux-1/default,123,0".to_string()),
        _ => None,
    });
    assert_eq!(host, PaneHost::Tmux);
}

#[test]
fn detect_host_ghostty_from_term_program() {
    let host = detect_pane_host_from(|k| match k {
        "TERM_PROGRAM" => Some("ghostty".to_string()),
        _ => None,
    });
    assert_eq!(host, PaneHost::Ghostty);
}

#[test]
fn detect_host_iterm_from_lc_terminal() {
    let host = detect_pane_host_from(|k| match k {
        "LC_TERMINAL" => Some("iTerm2".to_string()),
        _ => None,
    });
    assert_eq!(host, PaneHost::ITerm);
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

// ── Ghostty keybind parsing ───────────────────────────────────────────

#[test]
fn parse_ghostty_keybind_for_action_finds_binding() {
    let output = "keybind = super+shift+d=new_split:down\nkeybind = super+d=new_split:right\n";
    assert_eq!(
        parse_ghostty_keybind_for_action(output, "new_split:right"),
        Some("super+d".to_string())
    );
}

#[test]
fn parse_ghostty_keybind_for_action_returns_none_when_missing() {
    let output = "keybind = super+shift+d=new_split:down\n";
    assert_eq!(
        parse_ghostty_keybind_for_action(output, "new_split:right"),
        None
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
        role: midtown::launch::CoworkerRole::Lead,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: Some("test-team".to_string()),
        working_dir: None,
        model: "opus".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
    };

    let settings = std::env::temp_dir().join("test-cli-args-settings.json");
    let prompt = std::env::temp_dir().join("test-cli-args-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test prompt");

    let (args, session_id) = config.to_cli_args(&settings, &prompt, None);

    // Should have at least 7 args
    assert!(
        args.len() >= 7,
        "Should have at least 7 arguments, got: {:?}",
        args
    );

    // First arg should be 'claude'
    assert_eq!(args[0], "claude");

    // Should include --continue (resume mode)
    assert!(
        args.contains(&"--continue".to_string()),
        "Should include --continue flag"
    );

    // Should include --dangerously-skip-permissions
    assert!(
        args.contains(&"--dangerously-skip-permissions".to_string()),
        "Should include --dangerously-skip-permissions flag"
    );

    // Should include --settings
    assert!(
        args.contains(&"--settings".to_string()),
        "Should include --settings flag"
    );

    // Should include --append-system-prompt
    assert!(
        args.contains(&"--append-system-prompt".to_string()),
        "Should include --append-system-prompt flag"
    );

    // Should include agent teams flags
    assert!(
        args.contains(&"--agent-id".to_string()),
        "Should include --agent-id flag for agent teams"
    );
    assert!(
        args.contains(&"--team-name".to_string()),
        "Should include --team-name flag for agent teams"
    );

    // Resume mode → no session ID
    assert!(session_id.is_none());
}

#[test]
fn test_to_cli_args_fresh_generates_session_id() {
    let config = midtown::launch::LaunchConfig {
        name: "park".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        role: midtown::launch::CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
    };

    let settings = std::env::temp_dir().join("test-cli-args-settings2.json");
    let prompt = std::env::temp_dir().join("test-cli-args-prompt2.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test prompt");

    let (args, session_id) = config.to_cli_args(&settings, &prompt, None);

    // Fresh mode → should have session-id
    assert!(
        args.contains(&"--session-id".to_string()),
        "Fresh mode should include --session-id"
    );
    assert!(
        session_id.is_some(),
        "Fresh mode should return a session ID"
    );

    // Coworker should have --setting-sources
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
        role: midtown::launch::CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
    };

    let settings = std::env::temp_dir().join("test-settings.json");
    let prompt = std::env::temp_dir().join("test-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test");

    let (args, _) = config.to_cli_args(&settings, &prompt, None);

    // Coworker should have --setting-sources
    assert!(args.contains(&"--setting-sources".to_string()));
    assert!(args.contains(&"project,local".to_string()));
    // No agent teams without team_name
    assert!(!args.contains(&"--agent-id".to_string()));
}

// ── build_attach_shell_command ────────────────────────────────────────

#[test]
fn test_lead_attach_includes_system_prompt() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // Should include --append-system-prompt flag
    assert!(
        command.contains("--append-system-prompt"),
        "Lead attach command should include --append-system-prompt flag, got: {}",
        command
    );

    // Should reference a temp file with $(cat ...)
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
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // Should include --append-system-prompt flag
    assert!(
        command.contains("--append-system-prompt"),
        "Coworker attach command should include --append-system-prompt flag, got: {}",
        command
    );

    // Should reference a temp file with $(cat ...)
    assert!(
        command.contains("$(cat") && command.contains("midtown-attach-"),
        "Should use temp file pattern $(cat .../midtown-attach-...), got: {}",
        command
    );
}

#[test]
fn test_lead_attach_sets_task_list_id() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        None,
        true,
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
    let command = result.unwrap();

    // Lead should have CLAUDE_CODE_TASK_LIST_ID set
    assert!(
        command.contains("CLAUDE_CODE_TASK_LIST_ID="),
        "Lead attach command should set CLAUDE_CODE_TASK_LIST_ID env var, got: {}",
        command
    );
}

#[test]
fn test_coworker_attach_no_task_list_id() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "park",
        midtown::auth::AuthProvider::Claude,
        "session-456",
        None,
        true,
    );

    assert!(result.is_ok(), "build_attach_shell_command should succeed");
    let command = result.unwrap();

    // Coworkers should NOT have CLAUDE_CODE_TASK_LIST_ID set
    assert!(
        !command.contains("CLAUDE_CODE_TASK_LIST_ID="),
        "Coworker attach command should not set CLAUDE_CODE_TASK_LIST_ID env var, got: {}",
        command
    );
}

#[test]
fn test_lead_attach_includes_agent_teams_flags() {
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // Attach should include agent teams flags (previously missing)
    assert!(
        command.contains("--agent-id"),
        "Lead attach should include --agent-id flag, got: {}",
        command
    );
    assert!(
        command.contains("--team-name"),
        "Lead attach should include --team-name flag, got: {}",
        command
    );
    assert!(
        command.contains("--agent-name"),
        "Lead attach should include --agent-name flag, got: {}",
        command
    );
}

// ── Model flag in to_cli_args ─────────────────────────────────────────

#[test]
fn test_to_cli_args_includes_model_flag() {
    let config = midtown::launch::LaunchConfig {
        name: "lead".to_string(),
        session_mode: midtown::launch::SessionMode::Fresh,
        role: midtown::launch::CoworkerRole::Lead,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "opus".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
    };

    let settings = std::env::temp_dir().join("test-model-settings.json");
    let prompt = std::env::temp_dir().join("test-model-prompt.txt");
    let _ = std::fs::write(&settings, "{}");
    let _ = std::fs::write(&prompt, "test");

    let (args, _) = config.to_cli_args(&settings, &prompt, None);

    // Should pass --model flag so all launch paths set the model explicitly
    assert!(
        args.contains(&"--model".to_string()),
        "to_cli_args should include --model flag, got: {:?}",
        args
    );
    // Should use the configured model
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
        role: midtown::launch::CoworkerRole::Coworker,
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        team_name: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: midtown::auth::AuthProvider::Claude,
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
        Some("reviewer"),
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // Reviewer attach should include --setting-sources (coworker behavior)
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
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // Lead should always get opus model
    assert!(
        command.contains("--model") && command.contains("opus"),
        "Lead attach should use opus model, got: {}",
        command
    );
}

// ── Shell quoting note: channel-lead attach tests removed ─────────────
// Channel lead attach no longer reconstructs role from coworker_type in
// build_attach_shell_command; channel leads are identified by their channel
// name directly (e.g., "auth") tracked in channel_lead_sessions persistent state.

// ── Shell quoting ──────────────────────────────────────────────────────

#[test]
fn test_shell_quote_does_not_double_escape_dollar_command() {
    // The $(cat ...) pattern is used for passing large prompts to Claude.
    // It should NOT be double-escaped.
    let raw_arg = "$(cat /tmp/file.txt)";
    let quoted = shell_quote(raw_arg);

    // When the shell interprets the quoted string, it should see $(cat ...)
    // as command substitution, not as a literal string.
    // Test by having the shell echo it back.
    let output = std::process::Command::new("sh")
        .args(["-lc", &format!("echo {}", quoted)])
        .output()
        .expect("shell should parse");

    let _stdout = String::from_utf8_lossy(&output.stdout);
    // The output should contain the literal text since /tmp/file.txt doesn't exist
    // (shell would fail the cat and produce empty or error)
    // What matters is that the $ is interpreted as command substitution
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
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // The $(cat ...) pattern should appear and be properly quoted for shell interpretation.
    // The correct pattern is: '"$(cat /path/to/file)"' which breaks down as:
    //   ' - end single quote
    //   "$(cat /path/to/file)" - double-quoted command substitution (shell interprets this)
    //   ' - start single quote
    // This allows the shell to actually execute the cat command.

    assert!(
        command.contains("$(cat "),
        "Command should contain $(cat pattern for file reading"
    );

    // The pattern should be: '"$(cat ...)"' (single quote, double quote, $(cat, double quote, single quote)
    // This allows shell interpretation of the command substitution
    let correct_pattern = "'\"$(cat ";
    assert!(
        command.contains(correct_pattern),
        "Command should have correct quoting pattern '{}', got: {}",
        correct_pattern,
        command
    );

    // Verify the closing pattern too: ...)"'
    assert!(
        command.contains(")\"'"),
        "Command should have closing quote pattern ')\"\\'', got: {}",
        command
    );
}

#[test]
fn test_shell_quote_handles_single_quotes() {
    // Single quotes inside the string should be properly escaped
    let quoted = shell_quote("it's a test");
    // The quoted result should be valid when parsed by a shell
    // It should wrap in single quotes and escape internal quotes
    assert!(
        quoted.contains("'"),
        "Quoted string should contain quotes: {}",
        quoted
    );
    // Verify it round-trips correctly via shell
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
    // Verify it round-trips correctly via shell
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
    // Use a temp directory with spaces in the name to exercise quoting logic.
    // Must be a real git repo so detect_repo_name_from_dir succeeds.
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
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // The command should NOT have double-escaped quotes like ''"'"'...'"'"'
    // This pattern indicates double-quoting which breaks shell parsing
    assert!(
        !command.contains("''\"'\"'"),
        "Command should not contain double-escaped quotes (old pattern): {}",
        command
    );

    // The command should also not have the '\''\\'''\'' pattern from double-escaping
    // with the new shell_escape crate
    assert!(
        !command.contains("'\\''\\''"),
        "Command should not contain double-escaped quotes (new pattern): {}",
        command
    );

    // Most importantly, the inner sh -lc command should be parseable
    // Extract the sh -lc argument and verify it's valid
    if let Some(sh_lc_start) = command.find("sh -lc ") {
        let after_sh_lc = &command[sh_lc_start + 7..];
        // The argument to sh -lc should be a properly quoted string
        // It should start with a quote character
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
        None,
        true,
    );

    let command = result.expect("build_attach_shell_command should succeed");

    // The entire command should be parseable by a shell
    // If quoting is broken, this will fail
    let parse_result = std::process::Command::new("sh")
        .args(["-n", "-c", &command]) // -n = parse but don't execute
        .status();

    assert!(
        parse_result.is_ok() && parse_result.unwrap().success(),
        "Command should be parseable by shell, got error for: {}",
        command
    );
}

// ── include_detach flag (fix for dual-lead bug #1428) ────────────────

#[test]
fn test_view_attach_command_omits_session_detach() {
    // midtown view passes include_detach=false so the split pane's shell command
    // does NOT call `session detach` when claude exits — preventing the dual-lead
    // bug where the pane's claude process exits before the chat UI exits, causing
    // the headless lead to be re-spawned while the headed session is still active.
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        None,
        false, // include_detach=false: midtown view manages detach explicitly on exit
    );
    let command = result.expect("build_attach_shell_command should succeed");
    assert!(
        !command.contains("session detach"),
        "midtown view shell command must NOT contain `session detach` (dual-lead bug #1428), got: {}",
        command
    );
}

#[test]
fn test_standalone_attach_command_includes_session_detach() {
    // midtown session attach passes include_detach=true so the pane's shell
    // command calls `session detach` when the interactive session ends, resuming
    // the headless lead automatically.
    let cwd = find_project_root();
    let result = build_attach_shell_command(
        &cwd,
        "lead",
        midtown::auth::AuthProvider::Claude,
        "session-123",
        None,
        true, // include_detach=true: standalone attach needs auto-detach
    );
    let command = result.expect("build_attach_shell_command should succeed");
    assert!(
        command.contains("session detach"),
        "standalone attach shell command must contain `session detach`, got: {}",
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

    // Create a git repo with an initial commit
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

    // Create lead worktree at initial commit
    let wt = manager.create_lead_worktree().expect("create lead wt");
    let initial_head = git_head(repo);
    assert_eq!(git_head(&wt), initial_head);

    // Advance HEAD
    TestCmd::new("git")
        .args(["commit", "--allow-empty", "-m", "second"])
        .current_dir(repo)
        .output()
        .unwrap();
    let new_head = git_head(repo);
    assert_ne!(initial_head, new_head);

    // ensure_attach_worktree for "lead" should update worktree
    let result = ensure_attach_worktree("lead", &wt.to_string_lossy());
    assert!(result.is_ok());
    assert_eq!(
        git_head(&wt),
        new_head,
        "ensure_attach_worktree should update lead to HEAD"
    );
}

#[test]
fn ensure_attach_worktree_coworker_falls_back_to_daemon_cwd() {
    // When repo detection fails (no git repo), should return daemon_cwd
    let result = ensure_attach_worktree("park", "/tmp/some-cwd");
    assert!(result.is_ok());
    // Should not error — gracefully falls back
    assert_eq!(result.unwrap(), "/tmp/some-cwd");
}
