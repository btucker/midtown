//! Tests for lead system prompt persistence on attach and channel lead model selection

use crate::launch::{
    LaunchConfig, SessionMode, expand_session_id_in_prompt, inject_session_id_env,
};
use crate::paths;
use std::fs;

fn test_paths(dir_key: &str, project_name: &str) -> paths::ProjectPaths {
    paths::ProjectPaths::with_project_name(dir_key, project_name)
}

#[test]
fn test_launch_config_ops_channel_lead_model() {
    let config = LaunchConfig::channel_lead("ops", "myrepo", SessionMode::Fresh, "", None);
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
        agent_type: "midtown-project-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        auth_profile_dir: None,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
    };

    // Convert to headless config (this should save the system prompt)
    let headless = config.to_headless_config(&test_paths("test-repo", "test-repo"));

    // Verify the system prompt file was created
    let prompt_file = paths::lead_system_prompt_file("test-repo");
    assert!(
        prompt_file.exists(),
        "Lead system prompt file should be created at {}",
        prompt_file.display()
    );

    // Verify the file contains the full system prompt (all layers) for attach resumption
    let saved_prompt = fs::read_to_string(&prompt_file).unwrap();
    assert!(
        saved_prompt.contains("# Project Lead"),
        "Saved prompt should contain Layer 1 lead-specific content"
    );

    // The headless config's system_prompt is the append prompt (Layers 2+3 only),
    // while the saved file has the full prompt (all layers) for attach resumption.
    // These should differ — the saved prompt includes Layer 1 content.
    assert_ne!(
        saved_prompt, headless.system_prompt,
        "Saved full prompt should differ from append-only system_prompt"
    );
    assert!(
        !headless.system_prompt.contains("# Project Lead"),
        "Append prompt (Layers 2+3) should not contain Layer 1 content"
    );
}

#[test]
fn test_lead_system_prompt_file_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let _guard = paths::set_test_midtown_base_dir(temp_dir.path().to_path_buf());

    let prompt_file = paths::lead_system_prompt_file("myrepo");
    let expected = temp_dir
        .path()
        .join("projects")
        .join("myrepo")
        .join("lead-system-prompt.txt");

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

#[test]
fn test_shell_command_codex_fresh_uses_codex_binary() {
    let mut config = LaunchConfig::coworker(
        "park",
        "myrepo",
        SessionMode::Fresh,
        Some("Investigate failing tests".to_string()),
        None,
    );
    config.auth_provider = crate::auth::AuthProvider::Codex;
    config.model = "gpt-5.3-codex".to_string();
    let result = config.to_shell_command(
        std::path::Path::new("/tmp/settings.json"),
        std::path::Path::new("/tmp/prompt.md"),
        None,
        std::path::Path::new("/tmp/test-repo"),
        "midtown",
    );
    assert!(result.shell_command.contains(" codex "));
    assert!(!result.shell_command.contains(" claude "));
    assert!(
        result
            .shell_command
            .contains("--dangerously-bypass-approvals-and-sandbox")
    );
    assert!(result.shell_command.contains("--model"));
    assert!(result.shell_command.contains("gpt-5.3-codex"));
    assert!(result.shell_command.contains("Investigate failing tests"));
    assert!(result.session_id.is_none());
}

#[test]
fn test_shell_command_codex_fresh_reads_initial_prompt_file() {
    let prompt_dir = tempfile::tempdir().unwrap();
    let initial_prompt_path = prompt_dir.path().join("initial-prompt.txt");
    std::fs::write(&initial_prompt_path, "Prompt from file").unwrap();

    let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    config.auth_provider = crate::auth::AuthProvider::Codex;
    config.model = "gpt-5.3-codex".to_string();

    let result = config.to_shell_command(
        std::path::Path::new("/tmp/settings.json"),
        std::path::Path::new("/tmp/prompt.md"),
        Some(&initial_prompt_path),
        std::path::Path::new("/tmp/test-repo"),
        "midtown",
    );

    assert!(
        result.shell_command.contains("Prompt from file"),
        "Codex fresh launch should forward the file-backed prompt, got: {}",
        result.shell_command
    );
}

#[test]
fn test_shell_command_codex_resume_uses_resume_subcommand() {
    let mut config = LaunchConfig::lead("myrepo", None);
    config.auth_provider = crate::auth::AuthProvider::Codex;
    config.model = "gpt-5.3-codex".to_string();
    config.session_mode = SessionMode::ResumeSession("thread-123".to_string());
    let result = config.to_shell_command(
        std::path::Path::new("/tmp/settings.json"),
        std::path::Path::new("/tmp/prompt.md"),
        None,
        std::path::Path::new("/tmp/test-repo"),
        "midtown",
    );
    assert!(result.shell_command.contains(" codex "));
    assert!(result.shell_command.contains(" resume "));
    assert!(result.shell_command.contains("thread-123"));
    assert!(result.shell_command.contains("developer_instructions="));
    assert!(
        !result.shell_command.contains(" --model "),
        "Codex resume should preserve the thread's existing model, got: {}",
        result.shell_command
    );
    assert!(result.session_id.is_none());
}

// --- Disallowed tools tests ---

#[test]
fn test_channel_lead_disallowed_tools_contains_code_modification_tools() {
    use crate::launch::channel_lead_disallowed_tools;

    let tools = channel_lead_disallowed_tools();
    assert!(tools.contains(&"Write".to_string()));
    assert!(tools.contains(&"NotebookEdit".to_string()));
    // Edit is intentionally NOT blocked — channel leads need it for
    // maintaining notes and workflow files.
    assert!(!tools.contains(&"Edit".to_string()));
    // Bash is intentionally NOT blocked — channel leads need it for
    // coordination commands (midtown task create, midtown channel post, etc.)
    assert!(!tools.contains(&"Bash".to_string()));
}

#[test]
fn test_channel_lead_fork_disallowed_tools_includes_edit() {
    use crate::launch::channel_lead_fork_disallowed_tools;

    let tools = channel_lead_fork_disallowed_tools();
    // Fork sessions re-add Edit to the hard-block list because forks have
    // narrower context and historically ignored prompt-based restrictions
    // (see PR #1667).
    assert!(tools.contains(&"Edit".to_string()));
    assert!(tools.contains(&"Write".to_string()));
    assert!(tools.contains(&"NotebookEdit".to_string()));
    assert!(!tools.contains(&"Bash".to_string()));
}

#[test]
fn test_channel_lead_headless_config_has_disallowed_tools() {
    let config = LaunchConfig::channel_lead("auth", "myrepo", SessionMode::Fresh, "", None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert!(
        !headless.disallowed_tools.is_empty(),
        "Channel lead should have disallowed tools"
    );
    assert!(
        !headless.disallowed_tools.contains(&"Edit".to_string()),
        "Channel lead should NOT disallow Edit (needed for notes/workflow files)"
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
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert!(
        headless.disallowed_tools.is_empty(),
        "Coworker should not have disallowed tools"
    );
}

#[test]
fn test_reviewer_headless_config_has_no_disallowed_tools() {
    use crate::auth::AuthProvider;

    let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, AuthProvider::Claude);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert!(
        headless.disallowed_tools.is_empty(),
        "Reviewer should not have disallowed tools"
    );
}

#[test]
fn test_lead_headless_config_has_no_disallowed_tools() {
    let config = LaunchConfig::lead("myrepo", None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert!(
        headless.disallowed_tools.is_empty(),
        "Lead should not have disallowed tools"
    );
}

#[test]
fn test_codex_channel_lead_skips_disallowed_tools() {
    use crate::auth::AuthProvider;

    // Construct a channel lead config with Codex provider directly,
    // since LaunchConfig::channel_lead() reads provider from repo config.
    let config = LaunchConfig {
        name: "ops".to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-channel-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: Some("ops".to_string()),
        auth_provider: AuthProvider::Codex,
        auth_profile_dir: None,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
    };

    let headless = config.to_headless_config(&test_paths("myrepo", "myrepo"));
    assert!(
        headless.disallowed_tools.is_empty(),
        "Codex channel lead should NOT have disallowed_tools (Codex doesn't support them; \
         prompt-based enforcement is used instead)"
    );
}

#[test]
fn test_claude_channel_lead_still_has_disallowed_tools() {
    use crate::auth::AuthProvider;

    let config = LaunchConfig {
        name: "ops".to_string(),
        session_mode: SessionMode::Fresh,
        agent_type: "midtown-channel-lead".to_string(),
        initial_prompt: None,
        additional_dirs: vec![],
        pr_number: None,
        working_dir: None,
        model: "sonnet".to_string(),
        channel: Some("ops".to_string()),
        auth_provider: AuthProvider::Claude,
        auth_profile_dir: None,
        escalation_target: None,
        task_id: None,
        persisted_initial_prompt: None,
        cwd_subdir: None,
        system_prompt_extra: None,
    };

    let headless = config.to_headless_config(&test_paths("myrepo", "myrepo"));
    assert!(
        !headless.disallowed_tools.is_empty(),
        "Claude channel lead should still have disallowed_tools"
    );
    assert!(!headless.disallowed_tools.contains(&"Edit".to_string()));
    assert!(headless.disallowed_tools.contains(&"Write".to_string()));
}

#[test]
fn test_to_headless_config_reviewer_uses_agent_definition() {
    // For Claude, the system_prompt in HeadlessConfig is Layers 2+3 (append prompt).
    // Layer 1 (agent definition with reviewer instructions) is loaded via agent_name.
    let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, crate::auth::AuthProvider::Claude);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));

    // agent_name should point to the code reviewer agent definition
    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-code-reviewer"),
        "Reviewer should use the midtown-code-reviewer agent definition"
    );

    // The append prompt (system_prompt) should contain Layer 2 operational rules
    assert!(
        headless.system_prompt.contains("GitHub Etiquette"),
        "Reviewer append prompt should contain common.md content (Layer 2)"
    );
}

#[test]
fn test_channel_lead_cwd_subdir_defaults_to_none() {
    let config = LaunchConfig::channel_lead("auth", "myrepo", SessionMode::Fresh, "", None);
    assert!(
        config.cwd_subdir.is_none(),
        "Channel lead should have no cwd_subdir by default"
    );
}

#[test]
fn test_channel_lead_cwd_subdir_can_be_set() {
    let mut config = LaunchConfig::channel_lead("auth", "myrepo", SessionMode::Fresh, "", None);
    config.cwd_subdir = Some("packages/auth".to_string());
    assert_eq!(
        config.cwd_subdir.as_deref(),
        Some("packages/auth"),
        "cwd_subdir should be settable on channel lead config"
    );
}

// --- execution_role_for_agent_type tests ---

#[test]
fn test_execution_role_for_code_author() {
    assert_eq!(
        crate::config::execution_role_for_agent_type("midtown-code-author"),
        crate::config::ExecutionRole::Coworker
    );
}

#[test]
fn test_execution_role_for_code_reviewer() {
    assert_eq!(
        crate::config::execution_role_for_agent_type("midtown-code-reviewer"),
        crate::config::ExecutionRole::Reviewer
    );
}

#[test]
fn test_execution_role_for_project_lead() {
    assert_eq!(
        crate::config::execution_role_for_agent_type("midtown-project-lead"),
        crate::config::ExecutionRole::Lead
    );
}

#[test]
fn test_execution_role_for_channel_lead() {
    assert_eq!(
        crate::config::execution_role_for_agent_type("midtown-channel-lead"),
        crate::config::ExecutionRole::ChannelLead
    );
}

#[test]
fn test_to_headless_config_sets_agent_name_for_coworker() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-code-author"),
        "Coworker headless config should have agent_name set"
    );
}

#[test]
fn test_to_headless_config_sets_agent_name_for_reviewer() {
    let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, crate::auth::AuthProvider::Claude);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-code-reviewer"),
        "Reviewer headless config should have agent_name set"
    );
}

#[test]
fn test_to_headless_config_sets_agent_name_for_lead() {
    let config = LaunchConfig::lead("myrepo", None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-project-lead"),
        "Lead headless config should have agent_name set"
    );
}

#[test]
fn test_to_headless_config_sets_agent_name_for_channel_lead() {
    let config = LaunchConfig::channel_lead("ops", "myrepo", SessionMode::Fresh, "", None);
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));
    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-channel-lead"),
        "Channel lead headless config should have agent_name set"
    );
}

#[test]
fn test_render_append_prompt_excludes_layer1() {
    // render_append_prompt() should return only Layers 2+3, not the agent definition (Layer 1)
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    let append_prompt = config.render_append_prompt("midtown");

    // Layer 1 (agent definition, e.g. midtown-code-author) is loaded via --agent,
    // not included in the append prompt. The append prompt should only contain
    // Layer 2 content (common.md operational rules) + Layer 3 runtime context.
    let full_prompt = config.render_system_prompt("midtown");
    assert!(
        append_prompt.len() < full_prompt.len(),
        "Append prompt (Layers 2+3) should be shorter than full prompt (all layers)"
    );
}

#[test]
fn test_to_headless_config_codex_uses_full_prompt_no_agent_name() {
    let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    config.auth_provider = crate::auth::AuthProvider::Codex;

    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));

    // Codex doesn't support --agent, so agent_name must be None
    assert!(
        headless.agent_name.is_none(),
        "Codex headless config must not set agent_name"
    );

    // Codex system_prompt should be the full prompt (all layers), not just Layers 2+3
    let full_prompt = config.render_system_prompt("midtown");
    assert_eq!(
        headless.system_prompt, full_prompt,
        "Codex system_prompt should include all layers (full prompt)"
    );
}

#[test]
fn test_render_append_prompt_contains_common_md() {
    // render_append_prompt() should include Layer 2 (common.md shared operational rules)
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None, None);
    let append_prompt = config.render_append_prompt("midtown");

    // The append prompt should contain the shared prompt content
    assert!(
        !append_prompt.is_empty(),
        "Append prompt should not be empty"
    );
    // Verify it contains some of the common content (after template substitution)
    // common.md includes patterns like "midtown" references after substitution
    assert!(
        append_prompt.contains("midtown"),
        "Append prompt should contain project name from template substitution"
    );
}

#[test]
fn test_channel_lead_append_prompt_includes_domain_context() {
    // When using --agent (non-Codex), domain_context must appear in the append prompt
    // (Layers 2+3), not just in the Layer 1 agent definition file.
    let config = LaunchConfig::channel_lead(
        "auth",
        "myrepo",
        SessionMode::Fresh,
        "Auth module handles JWT tokens and session management",
        None,
    );
    let headless = config.to_headless_config(&test_paths("myrepo", "midtown"));

    // For non-Codex, agent_name is set and system_prompt is the append-only content
    assert!(
        headless.agent_name.is_some(),
        "Should use --agent for non-Codex"
    );
    assert!(
        headless
            .system_prompt
            .contains("Auth module handles JWT tokens"),
        "Append prompt must include domain_context for --agent path. Got: {}",
        &headless.system_prompt[..headless.system_prompt.len().min(500)]
    );
}

/// Changing agent_type directly controls the agent name in to_headless_config.
#[test]
fn test_agent_type_controls_headless_agent_name() {
    let mut config = LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        SessionMode::ResumeSession("session-123".to_string()),
        None,
        Some("42".to_string()),
    );
    config.agent_type = "midtown-code-reviewer".to_string();

    let headless = config.to_headless_config(&test_paths("test-repo", "test-repo"));

    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-code-reviewer"),
        "agent_type should control the agent name in headless config"
    );
}

/// Without agent_name_override, the role's default agent name is used.
#[test]
fn test_default_agent_name_without_override() {
    let config = LaunchConfig::coworker(
        "park".to_string(),
        "test-repo".to_string(),
        SessionMode::ResumeSession("session-123".to_string()),
        None,
        Some("42".to_string()),
    );

    let headless = config.to_headless_config(&test_paths("test-repo", "test-repo"));

    assert_eq!(
        headless.agent_name.as_deref(),
        Some("midtown-code-author"),
        "Without override, coworker role should use midtown-code-author agent"
    );
}

#[test]
fn test_expand_session_id_replaces_env_var_in_prompt() {
    let prompt = "<!-- midtown session:$MIDTOWN_SESSION_ID --> some text $MIDTOWN_SESSION_ID end";
    let expanded = expand_session_id_in_prompt(prompt, "abc-123-def");
    assert_eq!(
        expanded, "<!-- midtown session:abc-123-def --> some text abc-123-def end",
        "All occurrences of $MIDTOWN_SESSION_ID should be replaced"
    );
}

#[test]
fn test_expand_session_id_noop_when_no_placeholder() {
    let prompt = "no env var references here";
    let expanded = expand_session_id_in_prompt(prompt, "abc-123");
    assert_eq!(
        expanded, prompt,
        "Prompt without $MIDTOWN_SESSION_ID should be unchanged"
    );
}

#[test]
fn test_reviewer_system_prompt_contains_unexpanded_session_id() {
    // This test documents the pre-expansion state: the template contains
    // the literal $MIDTOWN_SESSION_ID before spawn_coworker() expands it.
    let prompt = crate::agents::reviewer_system_prompt(
        "york",
        "midtown",
        "midtown",
        crate::auth::AuthProvider::Claude,
        Some(42),
    );
    assert!(
        prompt.contains("$MIDTOWN_SESSION_ID"),
        "Reviewer system prompt template should contain $MIDTOWN_SESSION_ID before expansion"
    );

    // After expansion, the literal should be gone
    let expanded = expand_session_id_in_prompt(&prompt, "test-uuid-999");
    assert!(
        !expanded.contains("$MIDTOWN_SESSION_ID"),
        "After expand_session_id_in_prompt, no literal $MIDTOWN_SESSION_ID should remain"
    );
    assert!(
        expanded.contains("test-uuid-999"),
        "After expansion, the actual session ID should appear in the prompt"
    );
}
