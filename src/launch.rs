//! Unified launch configuration for Claude Code sessions.
//!
//! `LaunchConfig` is the single source of truth for how to launch a Claude CLI
//! process, whether in a tmux window (Lead) or as a headless session (coworkers).
//!
//! All spawn paths construct a `LaunchConfig`, then call either:
//! - `to_shell_command()` — for tmux-based launch (Lead, legacy path)
//! - `to_headless_config()` — for headless launch (coworkers, v2 path)

use std::path::PathBuf;

use crate::headless::HeadlessConfig;

/// How to establish the Claude Code session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMode {
    /// Brand new session with a generated UUID.
    Fresh,
    /// Resume the most recent session in this worktree (`--continue`).
    Resume,
    /// Resume a specific saved session (`--resume <id>`).
    ResumeSession(String),
}

/// The role of a coworker, which determines their system prompt.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CoworkerRole {
    /// Standard coworker — uses coworker.md + common.md
    #[default]
    Coworker,
    /// PR reviewer — uses coworker.md + common.md + reviewer.md
    Reviewer,
}

/// All configuration needed to launch a Claude CLI process.
///
/// This is the single source of truth for how Claude gets launched. All spawn
/// paths (fresh coworker, resumed coworker, reviewer, lead) construct one of
/// these and pass it to either `spawn_claude()` (tmux) or headless spawn.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Coworker name (or "lead" for the lead instance).
    pub name: String,
    /// How to start or resume the session.
    pub session_mode: SessionMode,
    /// The coworker's role (determines which system prompt to use).
    pub role: CoworkerRole,
    /// Optional prompt to pre-fill at startup (task instructions, review prompt, etc.).
    pub initial_prompt: Option<String>,
    /// Additional repo directories for multi-repo projects.
    pub additional_dirs: Vec<PathBuf>,
    /// If true, pass `--setting-sources project,local` to restrict settings.
    /// Coworkers use this to exclude user-level settings; the lead does not.
    pub restrict_setting_sources: bool,
    /// PR number for reviewer coworkers. Used to set the initial tmux window
    /// name to "review#PR" so reviewers are visually distinct from developers.
    pub pr_number: Option<u64>,
    /// Agent teams team name. When set, adds `--agent-id`, `--agent-name`,
    /// and `--team-name` CLI flags to enable the Claude Code agent teams
    /// mailbox system for message delivery.
    pub team_name: Option<String>,
    /// Optional working directory override for task-based worktrees.
    /// When set, the spawn path will use this directory instead of creating
    /// a coworker-named worktree. Used by the WorktreeRegistry system for
    /// task-based worktrees at ~/.midtown/worktrees/<repo>/task-<id>-<slug>/.
    pub working_dir: Option<PathBuf>,
    /// The Claude model to use for this session (e.g., "sonnet", "opus", "haiku").
    /// Defaults to "sonnet" for standard coworkers, "opus" for reviewers, PR handoff
    /// coworkers, and review feedback responders.
    pub model: String,
    /// Optional channel for routing coworker messages. When set, coworkers will
    /// post to this channel by default instead of the main channel.
    pub channel: Option<String>,
    /// Auth profile directory to use as `CLAUDE_CONFIG_DIR`.
    /// When set, overrides the default `current_profile_dir()` resolution.
    /// Callers should resolve this from project config before constructing.
    pub auth_profile_dir: Option<PathBuf>,
    /// Auth provider for this session. Determines which auth env var is set.
    pub auth_provider: crate::auth::AuthProvider,
}

/// The shell command string and generated session ID (if fresh).
pub struct LaunchCommand {
    pub shell_command: String,
    pub session_id: Option<String>,
}

/// Read z.ai environment variables from profile directory.
///
/// Returns (ANTHROPIC_AUTH_TOKEN, ANTHROPIC_BASE_URL).
/// Base URL defaults to https://api.z.ai/api/anthropic if not configured.
pub fn zai_env_vars(profile_dir: &std::path::Path) -> std::io::Result<(String, String)> {
    let api_key_file = profile_dir.join("api_key.txt");
    let api_key = std::fs::read_to_string(&api_key_file)
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to read z.ai API key from {}: {}",
                    api_key_file.display(),
                    e
                ),
            )
        })?
        .trim()
        .to_string();

    let base_url_file = profile_dir.join("base_url.txt");
    let base_url = if base_url_file.exists() {
        std::fs::read_to_string(&base_url_file)
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to read z.ai base URL from {}: {}",
                        base_url_file.display(),
                        e
                    ),
                )
            })?
            .trim()
            .to_string()
    } else {
        "https://api.z.ai/api/anthropic".to_string()
    };

    Ok((api_key, base_url))
}

impl LaunchConfig {
    /// Create a config for a standard coworker.
    ///
    /// Coworkers each have isolated task lists. The daemon bakes the task
    /// description into the initial prompt and tracks assignment internally.
    pub fn coworker(
        name: impl Into<String>,
        repo_name: impl Into<String>,
        session_mode: SessionMode,
        initial_prompt: Option<String>,
    ) -> Self {
        let repo = repo_name.into();
        let team = crate::mailbox::team_name_for_repo(&repo);
        LaunchConfig {
            name: name.into(),
            session_mode,
            role: CoworkerRole::Coworker,
            initial_prompt,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
            model: "sonnet".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        }
    }

    /// Create a config for a reviewer coworker.
    ///
    /// Reviewers get a specialized system prompt that merges coworker.md +
    /// common.md + reviewer.md, ensuring they follow reviewer instructions
    /// as behavioral rules rather than just task descriptions.
    pub fn reviewer(name: impl Into<String>, pr_number: u64) -> Self {
        LaunchConfig {
            name: name.into(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Reviewer,
            initial_prompt: Some(crate::agents::reviewer_launch_prompt(pr_number)),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: Some(pr_number),
            team_name: None, // Reviewers don't need mailbox (short-lived)
            working_dir: None,
            model: "opus".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        }
    }

    /// Create a config for PR handoff — a coworker taking over another's PR.
    ///
    /// This resumes the original author's Claude session to preserve full context
    /// (code understanding, decisions made, etc.) while having a different coworker
    /// continue the work. Used when the original PR author is unavailable.
    pub fn pr_handoff(
        name: impl Into<String>,
        repo_name: impl Into<String>,
        session_id: String,
        pr_number: u64,
        branch: &str,
        original_author: &str,
    ) -> Self {
        let repo = repo_name.into();
        let team = crate::mailbox::team_name_for_repo(&repo);
        let initial_prompt = format!(
            "You're taking over PR #{} from {}.\n\n\
            First, checkout the branch:\n\
            ```bash\n\
            git fetch origin {}\n\
            git checkout {}\n\
            ```\n\n\
            Then continue where {} left off. This is their PR, so you have their full context \
            from the resumed session. Address any review feedback, fix any issues, and push \
            your changes to the branch.\n\n\
            When done, post to the channel that you've addressed the feedback on PR #{}.",
            pr_number, original_author, branch, branch, original_author, pr_number
        );

        LaunchConfig {
            name: name.into(),
            session_mode: SessionMode::ResumeSession(session_id),
            role: CoworkerRole::Coworker,
            initial_prompt: Some(initial_prompt),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
            model: "opus".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        }
    }

    /// Convert to a `HeadlessConfig` for headless session spawn.
    ///
    /// Generates the system prompt based on the coworker's role, and maps
    /// session mode to `persist_session` / `resume_session_id` fields.
    ///
    /// `project_name` is used to load sandbox configuration.
    pub fn to_headless_config(&self, project_name: &str) -> HeadlessConfig {
        let system_prompt = match self.role {
            CoworkerRole::Reviewer => crate::agents::reviewer_system_prompt(&self.name),
            CoworkerRole::Coworker => crate::agents::coworker_system_prompt(&self.name),
        };

        let (persist_session, resume_session_id) = match &self.session_mode {
            SessionMode::Fresh => (true, None),
            SessionMode::Resume => (true, None), // --continue not supported in headless; treat as fresh
            SessionMode::ResumeSession(id) => (true, Some(id.clone())),
        };

        // Generate agent teams IDs from name + team
        let (agent_id, agent_name) = if let Some(ref team) = self.team_name {
            (
                Some(crate::mailbox::agent_id(&self.name, team)),
                Some(self.name.clone()),
            )
        } else {
            (None, None)
        };

        // Build env vars for the coworker process
        let mut env = std::collections::HashMap::new();
        env.insert("MIDTOWN_AGENT".to_string(), self.name.clone());

        // Set auth-provider-specific env vars
        let config_dir = self
            .auth_profile_dir
            .clone()
            .unwrap_or_else(|| crate::auth::current_profile_dir_for(self.auth_provider));

        match self.auth_provider {
            crate::auth::AuthProvider::Zai => {
                // z.ai uses API key + base URL, no config dir
                match zai_env_vars(&config_dir) {
                    Ok((api_key, base_url)) => {
                        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), api_key);
                        env.insert("ANTHROPIC_BASE_URL".to_string(), base_url);
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to load z.ai credentials: {}", e);
                    }
                }
            }
            _ => {
                // Claude and Codex use config dir env var
                let env_var = self.auth_provider.env_var();
                if !env_var.is_empty() {
                    env.insert(
                        env_var.to_string(),
                        config_dir.to_string_lossy().to_string(),
                    );
                }
            }
        }

        // Set default channel for routing coworker messages
        if let Some(ref ch) = self.channel {
            env.insert("MIDTOWN_CHANNEL".to_string(), ch.clone());
        }

        HeadlessConfig {
            model: self.model.clone(),
            system_prompt,
            json_schema: None,
            cwd: None, // Set by caller (worktree path)
            project_name: Some(project_name.to_string()),
            max_budget_usd: None,
            allow_tools: true, // Coworkers need full tool access
            persist_session,
            resume_session_id,
            inactivity_timeout: None,
            team_name: self.team_name.clone(),
            agent_id,
            agent_name,
            settings_path: None, // Set by caller
            setting_sources: if self.restrict_setting_sources {
                Some("project,local".to_string())
            } else {
                None
            },
            auth_provider: self.auth_provider,
            env,
        }
    }

    /// Build the full shell command string for launching Claude in a tmux pane.
    ///
    /// `settings_file` and `prompt_file` are pre-written files containing the
    /// Claude settings JSON and system prompt markdown. `initial_prompt_file`
    /// is the optional pre-written file containing the initial task/review prompt.
    /// `primary_repo` is the project root directory, used to compute the
    /// filesystem sandbox profile (writable directories).
    ///
    /// `project_name` is used to load sandbox configuration (allowed_paths).
    ///
    /// Returns a `LaunchCommand` with the shell command and the session ID
    /// (if a fresh session was created).
    pub fn to_shell_command(
        &self,
        settings_file: &std::path::Path,
        prompt_file: &std::path::Path,
        initial_prompt_file: Option<&std::path::Path>,
        primary_repo: &std::path::Path,
        project_name: &str,
    ) -> LaunchCommand {
        // -- Environment variables --
        let mut env_parts = vec![
            format!("MIDTOWN_AGENT='{}'", self.name),
            "DISABLE_AUTOUPDATER=1".to_string(),
        ];

        // Set auth-provider-specific env vars
        let config_dir = self
            .auth_profile_dir
            .clone()
            .unwrap_or_else(|| crate::auth::current_profile_dir_for(self.auth_provider));

        match self.auth_provider {
            crate::auth::AuthProvider::Zai => {
                // z.ai uses API key + base URL, no config dir
                match zai_env_vars(&config_dir) {
                    Ok((api_key, base_url)) => {
                        env_parts.push(format!("ANTHROPIC_AUTH_TOKEN='{}'", api_key));
                        env_parts.push(format!("ANTHROPIC_BASE_URL='{}'", base_url));
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to load z.ai credentials: {}", e);
                    }
                }
            }
            _ => {
                // Claude and Codex use config dir env var
                let env_var = self.auth_provider.env_var();
                if !env_var.is_empty() {
                    env_parts.push(format!("{}='{}'", env_var, config_dir.display()));
                }
            }
        }

        // Must be a real shell env var — Claude Code blocklists this from settings.json
        if self.team_name.is_some() {
            env_parts.push("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1".to_string());
        }
        // Set default channel for routing coworker messages
        if let Some(ref ch) = self.channel {
            env_parts.push(format!("MIDTOWN_CHANNEL='{}'", ch));
        }
        let env_export = format!("export {}", env_parts.join(" "));

        // -- Claude CLI arguments (as structured Vec, not format! interpolation) --
        // On macOS, prepend sandbox-exec to restrict filesystem writes.
        // On Linux with bwrap available, prepend bwrap.
        // Falls back to no sandboxing if profile creation fails.
        let sandbox_config = crate::config::get_project_sandbox_config(project_name);
        let writable = crate::sandbox::writable_dirs(
            primary_repo,
            &self.additional_dirs,
            &sandbox_config.allowed_paths,
        );
        let mut args: Vec<String> = if cfg!(target_os = "macos") {
            match crate::sandbox::sandbox_exec_prefix(&writable) {
                Ok((_profile_path, prefix)) => {
                    let mut a = vec!["sandbox-exec".to_string()];
                    a.extend(prefix);
                    a.push("claude".to_string());
                    a.push("--dangerously-skip-permissions".to_string());
                    a
                }
                Err(e) => {
                    eprintln!(
                        "Warning: sandbox setup failed, running without sandbox: {}",
                        e
                    );
                    vec![
                        "claude".to_string(),
                        "--dangerously-skip-permissions".to_string(),
                    ]
                }
            }
        } else if cfg!(target_os = "linux") && crate::sandbox::bwrap_available() {
            // On Linux, we prepend bwrap. Build the full bwrap command after
            // all claude args are assembled (see below).
            vec![
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
            ]
        } else {
            vec![
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
            ]
        };

        // Session mode — exactly one of these
        let session_id = match &self.session_mode {
            SessionMode::Fresh => {
                let id = uuid::Uuid::new_v4().to_string();
                args.push("--session-id".to_string());
                args.push(id.clone());
                Some(id)
            }
            SessionMode::Resume => {
                args.push("--continue".to_string());
                None
            }
            SessionMode::ResumeSession(id) => {
                args.push("--resume".to_string());
                args.push(id.clone());
                None
            }
        };

        // Additional directories (multi-repo)
        for dir in &self.additional_dirs {
            if let Some(d) = dir.to_str() {
                args.push("--add-dir".to_string());
                args.push(d.to_string());
            }
        }

        // Settings source restriction (coworkers only — lead uses all sources)
        if self.restrict_setting_sources {
            args.push("--setting-sources".to_string());
            args.push("project,local".to_string());
        }

        // Agent teams flags (enables mailbox-based message delivery)
        if let Some(ref team) = self.team_name {
            let agent_id = crate::mailbox::agent_id(&self.name, team);
            args.push("--agent-id".to_string());
            args.push(agent_id);
            args.push("--agent-name".to_string());
            args.push(self.name.clone());
            args.push("--team-name".to_string());
            args.push(team.clone());
        }

        args.push("--settings".to_string());
        args.push(settings_file.display().to_string());

        // System prompt file
        args.push("--append-system-prompt".to_string());
        args.push(format!("\"$(cat {})\"", prompt_file.display()));

        // Initial prompt as bare positional arg (NOT -p/--print).
        // Written to temp file by caller; path passed in here.
        // This MUST be the last argument. See PR #447 for why -p is forbidden.
        if let Some(path) = initial_prompt_file {
            args.push(format!("\"$(cat {})\"", path.display()));
        }

        let shell_command = format!("{}; exec {}", env_export, args.join(" "));

        LaunchCommand {
            shell_command,
            session_id,
        }
    }

    /// Create a fresh-session variant of this config (for retry after failure).
    pub fn as_fresh_retry(&self) -> Self {
        LaunchConfig {
            session_mode: SessionMode::Fresh,
            ..self.clone()
        }
    }

    /// Apply task model from WorldSnapshot to this LaunchConfig.
    ///
    /// Extracts provider and model from "provider/model" format (e.g., "claude/opus")
    /// and sets both `config.auth_provider` and `config.model`.
    ///
    /// This is used at all spawn sites to apply task-specific model preferences.
    pub fn apply_task_model(
        &mut self,
        task_model_map: &std::collections::HashMap<String, String>,
        task_id: &str,
    ) {
        if let Some(full_model) = task_model_map.get(task_id)
            && let Some((provider, model_alias)) = parse_task_model(full_model)
        {
            self.model = model_alias.to_string();
            self.auth_provider = provider;
        }
    }
}

/// Extract (auth_provider, model_alias) from "provider/model" format.
///
/// Valid examples: "claude/opus" → (Claude, "opus"), "codex/o3" → (Codex, "o3")
/// Invalid: "claude-opus" (no slash), "claude/" (empty model), "/opus" (empty provider)
///
/// Returns None if the format is invalid or the provider is unsupported.
fn parse_task_model(full_model: &str) -> Option<(crate::auth::AuthProvider, &str)> {
    let (provider_str, model_alias) = full_model.split_once('/')?;

    // Reject empty provider or empty model
    if provider_str.is_empty() || model_alias.is_empty() {
        return None;
    }

    let provider = provider_str.parse::<crate::auth::AuthProvider>().ok()?;
    Some((provider, model_alias))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_headless_config_fresh_coworker() {
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let headless = config.to_headless_config("midtown");

        assert!(headless.persist_session);
        assert!(headless.resume_session_id.is_none());
        assert!(headless.allow_tools);
        assert_eq!(headless.team_name, Some("midtown-myrepo".to_string()));
        assert_eq!(headless.agent_id, Some("park@midtown-myrepo".to_string()));
        assert_eq!(headless.agent_name, Some("park".to_string()));
        assert!(!headless.system_prompt.is_empty());
        assert_eq!(headless.model, "sonnet");
    }

    #[test]
    fn test_to_headless_config_resume_session() {
        let config = LaunchConfig {
            session_mode: SessionMode::ResumeSession("abc-123".to_string()),
            ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
        };
        let headless = config.to_headless_config("midtown");

        assert!(headless.persist_session);
        assert_eq!(headless.resume_session_id, Some("abc-123".to_string()));
    }

    #[test]
    fn test_to_headless_config_reviewer_has_no_teams() {
        let config = LaunchConfig::reviewer("york", 42);
        let headless = config.to_headless_config("midtown");

        assert!(headless.team_name.is_none());
        assert!(headless.agent_id.is_none());
        assert!(headless.agent_name.is_none());
        assert_eq!(headless.model, "opus");
    }

    #[test]
    fn test_to_headless_config_reviewer_has_tools() {
        let config = LaunchConfig::reviewer("york", 42);
        let headless = config.to_headless_config("midtown");
        assert!(headless.allow_tools);
    }

    #[test]
    fn test_to_headless_config_includes_setting_sources() {
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let headless = config.to_headless_config("midtown");

        assert_eq!(
            headless.setting_sources,
            Some("project,local".to_string()),
            "Coworkers must restrict setting sources to avoid duplicate plugin loading"
        );
    }

    #[test]
    fn test_to_headless_config_reviewer_includes_setting_sources() {
        let config = LaunchConfig::reviewer("york", 42);
        let headless = config.to_headless_config("midtown");

        assert_eq!(
            headless.setting_sources,
            Some("project,local".to_string()),
            "Reviewers must restrict setting sources to avoid duplicate plugin loading"
        );
    }

    #[test]
    fn test_launch_config_coworker_factory() {
        let config = LaunchConfig::coworker(
            "park".to_string(),
            "myrepo".to_string(),
            SessionMode::Fresh,
            Some("Do the thing".to_string()),
        );
        assert_eq!(config.name, "park");
        assert_eq!(config.session_mode, SessionMode::Fresh);
        assert_eq!(config.role, CoworkerRole::Coworker);
        assert_eq!(config.initial_prompt, Some("Do the thing".to_string()));
        assert!(config.restrict_setting_sources);
        assert!(config.pr_number.is_none());
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
        assert_eq!(config.model, "sonnet");
    }

    #[test]
    fn test_launch_config_reviewer_factory() {
        let config = LaunchConfig::reviewer("york".to_string(), 42);
        assert_eq!(config.name, "york");
        assert_eq!(config.pr_number, Some(42));
        assert_eq!(config.role, CoworkerRole::Reviewer);
        assert!(config.team_name.is_none());
        assert_eq!(config.model, "opus");
    }

    #[test]
    fn test_launch_config_pr_handoff_factory() {
        let config = LaunchConfig::pr_handoff(
            "york".to_string(),
            "myrepo",
            "session-123".to_string(),
            42,
            "feature/branch",
            "original-author",
        );
        assert_eq!(config.name, "york");
        assert_eq!(
            config.session_mode,
            SessionMode::ResumeSession("session-123".to_string())
        );
        assert!(config.initial_prompt.is_some());
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
        assert!(config.pr_number.is_none()); // Handoff is not a reviewer
        assert_eq!(config.model, "opus");
    }

    #[test]
    fn test_launch_config_as_fresh_retry() {
        let config = LaunchConfig::coworker(
            "park",
            "myrepo",
            SessionMode::Resume,
            Some("task prompt".to_string()),
        );
        let retry = config.as_fresh_retry();
        assert_eq!(retry.session_mode, SessionMode::Fresh);
        assert_eq!(retry.name, "park");
        assert_eq!(retry.initial_prompt, Some("task prompt".to_string()));
    }

    // --- Shell command tests (tmux path) ---

    #[test]
    fn test_shell_command_fresh_session() {
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
            std::path::Path::new("/tmp/test-repo"),
            "midtown",
        );
        assert!(result.shell_command.contains("--session-id "));
        assert!(!result.shell_command.contains("--continue"));
        assert!(!result.shell_command.contains("--resume "));
        assert!(result.session_id.is_some());
    }

    #[test]
    fn test_shell_command_resume_session() {
        let config = LaunchConfig {
            session_mode: SessionMode::ResumeSession("abc-123".to_string()),
            ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
            std::path::Path::new("/tmp/test-repo"),
            "midtown",
        );
        assert!(result.shell_command.contains("--resume abc-123"));
        assert!(result.session_id.is_none());
    }

    #[test]
    fn test_shell_command_agent_teams_flags() {
        let config = LaunchConfig::coworker("lexington", "myrepo", SessionMode::Fresh, None);
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
            std::path::Path::new("/tmp/test-repo"),
            "midtown",
        );
        assert!(
            result
                .shell_command
                .contains("--agent-id lexington@midtown-myrepo")
        );
        assert!(result.shell_command.contains("--agent-name lexington"));
        assert!(result.shell_command.contains("--team-name midtown-myrepo"));
        assert!(
            result
                .shell_command
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1")
        );
    }

    #[test]
    fn test_shell_command_no_agent_teams_without_team() {
        let config = LaunchConfig {
            team_name: None,
            ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
        };
        let result = config.to_shell_command(
            std::path::Path::new("/tmp/settings.json"),
            std::path::Path::new("/tmp/prompt.md"),
            None,
            std::path::Path::new("/tmp/test-repo"),
            "midtown",
        );
        assert!(!result.shell_command.contains("--agent-id"));
        assert!(
            !result
                .shell_command
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS")
        );
    }

    #[test]
    fn test_channel_routing_env_var() {
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        config.channel = Some("task-42".to_string());
        let headless = config.to_headless_config("midtown");

        // Verify MIDTOWN_CHANNEL env var is set
        assert_eq!(
            headless.env.get("MIDTOWN_CHANNEL"),
            Some(&"task-42".to_string())
        );
    }

    #[test]
    fn test_no_channel_routing_when_none() {
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let headless = config.to_headless_config("midtown");

        // Verify MIDTOWN_CHANNEL env var is not set when channel is None
        assert!(!headless.env.contains_key("MIDTOWN_CHANNEL"));
    }

    #[test]
    fn test_to_headless_config_uses_codex_home_for_codex_provider() {
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        config.auth_provider = crate::auth::AuthProvider::Codex;
        config.auth_profile_dir = Some(std::path::PathBuf::from("/tmp/midtown-codex-profile"));

        let headless = config.to_headless_config("midtown");

        assert_eq!(
            headless.env.get("CODEX_HOME"),
            Some(&"/tmp/midtown-codex-profile".to_string())
        );
        assert_eq!(headless.auth_provider, crate::auth::AuthProvider::Codex);
        assert!(
            !headless.env.contains_key("CLAUDE_CONFIG_DIR"),
            "Codex provider should not inject CLAUDE_CONFIG_DIR"
        );
    }

    // Tests for parse_task_model function
    #[test]
    fn test_parse_task_model_valid_claude() {
        let result = parse_task_model("claude/opus");
        assert_eq!(result, Some((crate::auth::AuthProvider::Claude, "opus")));
    }

    #[test]
    fn test_parse_task_model_valid_codex() {
        let result = parse_task_model("codex/o3");
        assert_eq!(result, Some((crate::auth::AuthProvider::Codex, "o3")));
    }

    #[test]
    fn test_parse_task_model_empty_model_returns_none() {
        // Bug fix: "claude/" should return None, not Some((Claude, ""))
        let result = parse_task_model("claude/");
        assert_eq!(result, None, "Empty model string should be rejected");
    }

    #[test]
    fn test_parse_task_model_empty_provider_returns_none() {
        let result = parse_task_model("/opus");
        assert_eq!(result, None, "Empty provider should be rejected");
    }

    #[test]
    fn test_parse_task_model_no_slash_returns_none() {
        let result = parse_task_model("claude-opus");
        assert_eq!(result, None, "Format without slash should be rejected");
    }

    #[test]
    fn test_parse_task_model_unknown_provider_returns_none() {
        let result = parse_task_model("unknown/opus");
        assert_eq!(result, None, "Unknown provider should be rejected");
    }

    #[test]
    fn test_parse_task_model_whitespace_in_model() {
        let result = parse_task_model("claude/ opus");
        // Should return Some but with whitespace in model (validation happens elsewhere)
        assert_eq!(result, Some((crate::auth::AuthProvider::Claude, " opus")));
    }

    // Tests for apply_task_model method
    #[test]
    fn test_apply_task_model_sets_both_model_and_provider() {
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let mut map = std::collections::HashMap::new();
        map.insert("42".to_string(), "codex/o3".to_string());

        config.apply_task_model(&map, "42");

        assert_eq!(config.model, "o3", "Model alias should be extracted");
        assert_eq!(
            config.auth_provider,
            crate::auth::AuthProvider::Codex,
            "Auth provider should be set to Codex"
        );
    }

    #[test]
    fn test_apply_task_model_no_change_when_task_not_in_map() {
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let original_model = config.model.clone();
        let original_provider = config.auth_provider;
        let map = std::collections::HashMap::new();

        config.apply_task_model(&map, "42");

        assert_eq!(config.model, original_model, "Model should be unchanged");
        assert_eq!(
            config.auth_provider, original_provider,
            "Auth provider should be unchanged"
        );
    }

    #[test]
    fn test_apply_task_model_no_change_on_invalid_format() {
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let original_model = config.model.clone();
        let original_provider = config.auth_provider;
        let mut map = std::collections::HashMap::new();
        map.insert("42".to_string(), "invalid-format".to_string());

        config.apply_task_model(&map, "42");

        assert_eq!(
            config.model, original_model,
            "Model should be unchanged when format is invalid"
        );
        assert_eq!(
            config.auth_provider, original_provider,
            "Auth provider should be unchanged when format is invalid"
        );
    }

    #[test]
    fn test_apply_task_model_no_change_on_empty_model_string() {
        // Bug fix test: "claude/" should not update config
        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let original_model = config.model.clone();
        let original_provider = config.auth_provider;
        let mut map = std::collections::HashMap::new();
        map.insert("42".to_string(), "claude/".to_string());

        config.apply_task_model(&map, "42");

        assert_eq!(
            config.model, original_model,
            "Model should be unchanged when model string is empty"
        );
        assert_eq!(
            config.auth_provider, original_provider,
            "Auth provider should be unchanged when model string is empty"
        );
    }
}
