//! Unified launch configuration for Claude Code sessions.
//!
//! `LaunchConfig` is the single source of truth for how to launch a Claude CLI
//! process as a headless session. All spawn paths construct a `LaunchConfig`
//! and call `to_headless_config()` to produce a `HeadlessConfig`.

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
    /// Lead — uses lead.md + common.md, unrestricted settings
    Lead,
    /// Channel lead — uses channel-lead.md with channel name injected.
    /// Read-only: brainstorming and domain expertise for a topic channel.
    ChannelLead {
        /// The channel this lead is responsible for.
        channel_name: String,
        /// Domain context injected at startup from channel notes files.
        domain_context: String,
    },
}

impl CoworkerRole {
    /// Map to the corresponding config `ExecutionRole` for model/provider lookups.
    pub fn execution_role(&self) -> crate::config::ExecutionRole {
        match self {
            CoworkerRole::Lead => crate::config::ExecutionRole::Lead,
            CoworkerRole::Reviewer => crate::config::ExecutionRole::Reviewer,
            CoworkerRole::ChannelLead { .. } => crate::config::ExecutionRole::ChannelLead,
            CoworkerRole::Coworker => crate::config::ExecutionRole::Coworker,
        }
    }
}

/// All configuration needed to launch a Claude CLI process.
///
/// This is the single source of truth for how Claude gets launched. All spawn
/// paths (fresh coworker, resumed coworker, reviewer, lead) construct one of
/// these and pass it to `to_headless_config()` for headless spawn.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Coworker name (or the repo name for the lead instance).
    pub name: String,
    /// How to start or resume the session.
    pub session_mode: SessionMode,
    /// The coworker's role (determines which system prompt to use).
    pub role: CoworkerRole,
    /// Optional prompt to pre-fill at startup (task instructions, review prompt, etc.).
    pub initial_prompt: Option<String>,
    /// Additional repo directories for multi-repo projects.
    pub additional_dirs: Vec<PathBuf>,
    /// PR number for reviewer coworkers.
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
    /// Defaults to "sonnet" for standard coworkers, "opus" for the Lead, reviewers,
    /// PR handoff coworkers, and review feedback responders.
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
    /// Override for the `initial_prompt` stored in `SessionRecord` at spawn time.
    ///
    /// When `Some`, `spawn_coworker` persists this value instead of `initial_prompt`.
    /// Use this when the actual message sent to Claude (in `initial_prompt`) differs
    /// from the canonical prompt to store — for example, `session clear` sends a
    /// decorated "fresh restart" message but wants to persist the *original* prompt
    /// so repeated clears don't accumulate the decoration.
    ///
    /// When `None`, `spawn_coworker` falls back to persisting `initial_prompt`.
    pub persisted_initial_prompt: Option<String>,
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

/// Build environment variables common to all agent sessions (headless and interactive).
///
/// Returns a BTreeMap of env var name -> value that should be set for any Claude Code
/// agent session. This includes:
/// - `MIDTOWN_AGENT`: Agent name
/// - `DISABLE_AUTOUPDATER`: Always set to "1"
/// - Auth provider env vars (CLAUDE_CONFIG_DIR, ANTHROPIC_AUTH_TOKEN, etc.)
/// - `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`: Set when team_name is present
/// - `CLAUDE_CODE_TASK_LIST_ID`: Set for Lead sessions to share task list
/// - `MIDTOWN_CHANNEL`: Set when channel is specified
///
/// Callers should add mode-specific env vars on top of these as needed.
pub fn build_agent_env_vars(
    name: &str,
    role: &CoworkerRole,
    team_name: &Option<String>,
    channel: &Option<String>,
    auth_provider: crate::auth::AuthProvider,
    auth_profile_dir: &std::path::Path,
) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();

    env.insert("MIDTOWN_AGENT".to_string(), name.to_string());
    env.insert("DISABLE_AUTOUPDATER".to_string(), "1".to_string());

    // Set auth-provider-specific env vars
    match auth_provider {
        crate::auth::AuthProvider::Zai => {
            // z.ai uses API key + base URL, no config dir
            match zai_env_vars(auth_profile_dir) {
                Ok((api_key, base_url)) => {
                    env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), api_key);
                    env.insert("ANTHROPIC_BASE_URL".to_string(), base_url);
                    // Anthropic alias -> GLM mapping for z.ai provider.
                    env.insert(
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                        "GLM-4.5-Air".to_string(),
                    );
                    env.insert(
                        "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                        "GLM-4.7".to_string(),
                    );
                    env.insert(
                        "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                        "GLM-5".to_string(),
                    );
                }
                Err(e) => {
                    eprintln!("Warning: failed to load z.ai credentials: {}", e);
                }
            }
        }
        _ => {
            // Claude and Codex use config dir env var
            let env_var = auth_provider.env_var();
            if !env_var.is_empty() {
                env.insert(
                    env_var.to_string(),
                    auth_profile_dir.to_string_lossy().to_string(),
                );
            }
        }
    }

    // Set default channel for routing coworker messages
    if let Some(ch) = channel {
        env.insert("MIDTOWN_CHANNEL".to_string(), ch.clone());
    }

    // Must be a real shell env var — Claude Code blocklists this from settings.json
    if team_name.is_some() {
        env.insert(
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".to_string(),
            "1".to_string(),
        );
    }

    // Lead shares the project task list with coworkers via this env var
    if *role == CoworkerRole::Lead
        && let Some(team) = team_name
    {
        env.insert("CLAUDE_CODE_TASK_LIST_ID".to_string(), team.clone());
    }

    env
}

/// Inject `MIDTOWN_SESSION_ID` into a coworker env map.
///
/// Called by `spawn_coworker()` after the session UUID is pre-generated so
/// coworkers can call `midtown session fork --thread-id <id>` without
/// needing to pass `--session-id` on every invocation.
pub fn inject_session_id_env(
    env: &mut std::collections::BTreeMap<String, String>,
    session_id: &str,
) {
    env.insert("MIDTOWN_SESSION_ID".to_string(), session_id.to_string());
}

/// Compute the session name for a channel lead.
///
/// Channel lead sessions are named after their channel directly (e.g., "web" for
/// the "web" channel). The daemon identifies channel leads via `channel_lead_sessions`
/// in persistent state rather than by name prefix.
pub fn channel_lead_session_name(channel_name: &str) -> String {
    channel_name.to_string()
}

/// Tools that channel leads (and their forks) are not allowed to use.
///
/// Channel leads are coordinators and domain experts — they scope work and create
/// tasks, but never implement code. This list is passed as `--disallowedTools` to
/// the Claude CLI, providing hard enforcement that the LLM cannot bypass.
///
/// Note: `Bash` is intentionally NOT included because channel leads need it for
/// coordination commands (`midtown task create`, `midtown channel post`, etc.).
/// The existing soft restriction in `channel-lead.md` covers "do not use Bash to
/// modify code", which is sufficient since Edit/Write are hard-blocked here.
pub fn channel_lead_disallowed_tools() -> Vec<String> {
    ["Edit", "Write", "NotebookEdit"]
        .iter()
        .map(|s| s.to_string())
        .collect()
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
        let auth_provider = crate::config::get_execution_provider_for_role(
            &repo,
            crate::config::ExecutionRole::Coworker,
        );
        let model =
            crate::config::get_model_for_role(&repo, crate::config::ExecutionRole::Coworker)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "sonnet".to_string());
        LaunchConfig {
            name: name.into(),
            session_mode,
            role: CoworkerRole::Coworker,
            initial_prompt,
            additional_dirs: vec![],
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
            model,
            channel: None,
            auth_profile_dir: None,
            auth_provider,
            persisted_initial_prompt: None,
        }
    }

    /// Create a config for a reviewer coworker.
    ///
    /// Reviewers get a specialized system prompt that merges coworker.md +
    /// common.md + reviewer.md, ensuring they follow reviewer instructions
    /// as behavioral rules rather than just task descriptions.
    ///
    /// `restart_count` is 0 for first launch, >0 for respawns. When >0, the
    /// initial prompt includes context about the previous failed attempt and
    /// instructs the reviewer to update the existing placeholder comment.
    pub fn reviewer(
        name: impl Into<String>,
        repo_name: impl Into<String>,
        pr_number: u64,
        restart_count: u32,
        auth_provider: crate::auth::AuthProvider,
    ) -> Self {
        let repo = repo_name.into();
        let model =
            crate::config::get_model_for_role(&repo, crate::config::ExecutionRole::Reviewer)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        LaunchConfig {
            name: name.into(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Reviewer,
            initial_prompt: Some(crate::agents::reviewer_launch_prompt(
                pr_number,
                restart_count,
                auth_provider,
            )),
            additional_dirs: vec![],
            pr_number: Some(pr_number),
            team_name: None, // Reviewers don't need mailbox (short-lived)
            working_dir: None,
            model,
            channel: None,
            auth_profile_dir: None,
            auth_provider,
            persisted_initial_prompt: None,
        }
    }

    /// Create a config for the Project Lead session.
    ///
    /// When `channel` is None, creates the Project Lead (uses `main_lead_system_prompt()`).
    /// When `channel` is Some, creates a channel lead for that channel
    /// (uses `channel_lead_system_prompt()`).
    ///
    /// The Project Lead uses unrestricted setting sources and runs as a headless session
    /// that can be attached/detached like coworkers.
    pub fn lead(repo_name: impl Into<String>, channel: Option<&str>) -> Self {
        let repo = repo_name.into();
        let team = crate::mailbox::team_name_for_repo(&repo);

        if let Some(channel_name) = channel {
            // Channel lead — delegate to channel_lead factory
            // Note: domain_context is empty here; callers that need notes
            // should load them via load_channel_notes() and pass directly
            // to channel_lead() to keep this function I/O-free.
            LaunchConfig::channel_lead(channel_name, &repo, SessionMode::Fresh, "")
        } else {
            // Project Lead
            let auth_provider = crate::config::get_execution_provider_for_role(
                &repo,
                crate::config::ExecutionRole::Lead,
            );
            let model =
                crate::config::get_model_for_role(&repo, crate::config::ExecutionRole::Lead)
                    .map(|s| s.as_model_str().to_string())
                    .unwrap_or_else(|| "opus".to_string());
            LaunchConfig {
                name: repo.clone(),
                session_mode: SessionMode::Fresh,
                role: CoworkerRole::Lead,
                initial_prompt: Some(crate::agents::main_lead_initial_prompt(&repo, &repo)),
                additional_dirs: vec![],
                pr_number: None,
                team_name: Some(team),
                working_dir: None,
                model,
                channel: None,
                auth_profile_dir: None,
                auth_provider,
                persisted_initial_prompt: None,
            }
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
        let auth_provider = crate::config::get_execution_provider_for_role(
            &repo,
            crate::config::ExecutionRole::Coworker,
        );
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

        // PR handoff uses the coworker model config but falls back to "opus" (not "sonnet")
        // because handoffs deal with complex PR context that benefits from a larger model.
        let model =
            crate::config::get_model_for_role(&repo, crate::config::ExecutionRole::Coworker)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        LaunchConfig {
            name: name.into(),
            session_mode: SessionMode::ResumeSession(session_id),
            role: CoworkerRole::Coworker,
            initial_prompt: Some(initial_prompt),
            additional_dirs: vec![],
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
            model,
            channel: None,
            auth_profile_dir: None,
            auth_provider,
            persisted_initial_prompt: None,
        }
    }

    /// Create a config for a channel lead session.
    ///
    /// Channel leads are long-lived conversational sessions that accumulate
    /// domain expertise for a topic channel. They use the channel-lead.md
    /// system prompt, run with read-only tool access, and post responses to
    /// their channel via `midtown channel post --channel {name}`.
    ///
    /// The `domain_context` is injected into the system prompt at spawn time.
    /// Callers load it from channel notes files via `load_channel_notes()`.
    ///
    /// The session name equals the channel name directly (e.g., "auth" for channel "auth").
    /// Channel leads are identified via `channel_lead_sessions` in persistent state,
    /// not by a name prefix.
    pub fn channel_lead(
        channel_name: impl Into<String>,
        repo_name: impl Into<String>,
        session_mode: SessionMode,
        domain_context: impl Into<String>,
    ) -> Self {
        let channel_name_str = channel_name.into();
        let session_name = channel_lead_session_name(&channel_name_str);
        let repo = repo_name.into();
        let team = crate::mailbox::team_name_for_repo(&repo);
        let auth_provider = crate::config::get_execution_provider_for_role(
            &repo,
            crate::config::ExecutionRole::ChannelLead,
        );
        let domain_ctx = domain_context.into();
        let execution_fallback = crate::config::get_channel_lead_model_fallback(&repo);
        LaunchConfig {
            name: session_name,
            session_mode,
            role: CoworkerRole::ChannelLead {
                channel_name: channel_name_str.clone(),
                domain_context: domain_ctx,
            },
            initial_prompt: Some(crate::agents::channel_lead_initial_prompt(
                &channel_name_str,
            )),
            additional_dirs: vec![],
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
            model: crate::config::get_channel_leads_config(&repo)
                .model_for_channel_with_fallback(&channel_name_str, execution_fallback),
            channel: Some(channel_name_str),
            auth_profile_dir: None,
            auth_provider,
            persisted_initial_prompt: None,
        }
    }

    /// Convert to a `HeadlessConfig` for headless session spawn.
    ///
    /// Generates the system prompt based on the coworker's role, and maps
    /// session mode to `persist_session` / `resume_session_id` fields.
    ///
    /// `project_name` is used to load sandbox configuration.
    ///
    /// For Lead role: saves the system prompt to `~/.midtown/lead/<repo>/system-prompt.txt`
    /// so it can be re-applied when attaching to the headless session.
    pub fn to_headless_config(&self, project_name: &str) -> HeadlessConfig {
        let system_prompt = match &self.role {
            CoworkerRole::Reviewer => crate::agents::reviewer_system_prompt(
                &self.name,
                project_name,
                self.auth_provider,
                self.pr_number,
            ),
            CoworkerRole::Lead => crate::agents::main_lead_system_prompt(project_name),
            CoworkerRole::Coworker => {
                crate::agents::coworker_system_prompt(&self.name, project_name)
            }
            CoworkerRole::ChannelLead {
                channel_name,
                domain_context,
            } => crate::agents::channel_lead_system_prompt(
                channel_name,
                domain_context,
                project_name,
            ),
        };

        // Save the lead system prompt to disk for attach resumption
        if matches!(self.role, CoworkerRole::Lead) {
            let prompt_file = crate::paths::lead_system_prompt_file(project_name);
            if let Some(parent) = prompt_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&prompt_file, &system_prompt);
        }

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

        // Build env vars for the coworker process using the shared function
        let config_dir = self
            .auth_profile_dir
            .clone()
            .unwrap_or_else(|| crate::auth::current_profile_dir_for(self.auth_provider));

        let env = build_agent_env_vars(
            &self.name,
            &self.role,
            &self.team_name,
            &self.channel,
            self.auth_provider,
            &config_dir,
        );

        // Channel leads get hard tool restrictions — they are coordinators,
        // not implementers. This is enforced at the CLI level via --disallowedTools
        // so the LLM cannot bypass it.
        let disallowed_tools = if matches!(self.role, CoworkerRole::ChannelLead { .. }) {
            channel_lead_disallowed_tools()
        } else {
            vec![]
        };

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
            session_id: None, // Set by spawn_coworker for fresh sessions
            inactivity_timeout: None,
            team_name: self.team_name.clone(),
            agent_id,
            agent_name,
            settings_path: None,   // Set by caller
            setting_sources: None, // Handled by platform arg builder (always project,local)
            auth_provider: self.auth_provider,
            env,
            fork_session: false,
            disallowed_tools,
        }
    }

    /// Build the Claude CLI argument vector.
    ///
    /// Delegates to `crate::platform::build_claude_headed_args()` — the single
    /// source of truth for headed CLI arg construction.
    ///
    /// Returns `(args, session_id)` where `args` starts with `"claude"` and
    /// includes all flags, and `session_id` is `Some(uuid)` for fresh sessions.
    ///
    /// Does NOT include sandbox prefix or env vars — callers add those.
    pub fn to_cli_args(
        &self,
        settings_file: &std::path::Path,
        prompt_file: &std::path::Path,
        initial_prompt_file: Option<&std::path::Path>,
    ) -> (Vec<String>, Option<String>) {
        crate::platform::build_claude_headed_args(
            self,
            settings_file,
            prompt_file,
            initial_prompt_file,
        )
    }

    /// Build the full shell command string for launching Claude in a terminal pane.
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
        // Get common env vars from shared function
        let config_dir = self
            .auth_profile_dir
            .clone()
            .unwrap_or_else(|| crate::auth::current_profile_dir_for(self.auth_provider));

        let env_map = build_agent_env_vars(
            &self.name,
            &self.role,
            &self.team_name,
            &self.channel,
            self.auth_provider,
            &config_dir,
        );

        // Convert env map to shell export format (key='value')
        let env_parts: Vec<String> = env_map
            .iter()
            .map(|(k, v)| format!("{}='{}'", k, v))
            .collect();

        let env_export = format!("export {}", env_parts.join(" "));

        // -- Sandbox prefix --
        // On macOS, prepend sandbox-exec to restrict filesystem writes.
        // On Linux with bwrap available, prepend bwrap.
        // Falls back to no sandboxing if profile creation fails.
        let sandbox_config = crate::config::get_project_sandbox_config(project_name);
        let writable = crate::sandbox::writable_dirs(
            primary_repo,
            &self.additional_dirs,
            &sandbox_config.allowed_paths,
        );
        let sandbox_prefix: Vec<String> = if cfg!(target_os = "macos") {
            match crate::sandbox::sandbox_exec_prefix(&writable) {
                Ok((_profile_path, prefix)) => {
                    let mut a = vec!["sandbox-exec".to_string()];
                    a.extend(prefix);
                    a
                }
                Err(e) => {
                    eprintln!(
                        "Warning: sandbox setup failed, running without sandbox: {}",
                        e
                    );
                    vec![]
                }
            }
        } else {
            // On Linux, bwrap support is TODO — no sandbox prefix for now.
            vec![]
        };

        // -- CLI arguments (from shared method) --
        let (cli_args, session_id) =
            self.to_cli_args(settings_file, prompt_file, initial_prompt_file);

        // Combine: sandbox prefix + CLI args
        let mut all_args = sandbox_prefix;
        all_args.extend(cli_args);

        let shell_command = format!("{}; exec {}", env_export, all_args.join(" "));

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
    use crate::auth::AuthProvider;

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
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Coworker)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "sonnet".to_string());
        assert_eq!(headless.model, expected);
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
        let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, AuthProvider::Claude);
        let headless = config.to_headless_config("midtown");

        assert!(headless.team_name.is_none());
        assert!(headless.agent_id.is_none());
        assert!(headless.agent_name.is_none());
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Reviewer)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        assert_eq!(headless.model, expected);
    }

    #[test]
    fn test_to_headless_config_reviewer_has_tools() {
        let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, AuthProvider::Claude);
        let headless = config.to_headless_config("midtown");
        assert!(headless.allow_tools);
    }

    #[test]
    fn test_to_headless_config_setting_sources_handled_by_platform() {
        // setting_sources is now always None on HeadlessConfig — the platform
        // arg builder unconditionally adds --setting-sources project,local.
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let headless = config.to_headless_config("midtown");
        assert_eq!(
            headless.setting_sources, None,
            "setting_sources should be None — handled by platform arg builder"
        );

        let config = LaunchConfig::reviewer("york", "myrepo", 42, 0, AuthProvider::Claude);
        let headless = config.to_headless_config("midtown");
        assert_eq!(
            headless.setting_sources, None,
            "setting_sources should be None — handled by platform arg builder"
        );

        let config = LaunchConfig::lead("myrepo", None);
        let headless = config.to_headless_config("midtown");
        assert_eq!(
            headless.setting_sources, None,
            "setting_sources should be None — handled by platform arg builder"
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
        assert!(config.pr_number.is_none());
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Coworker)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "sonnet".to_string());
        assert_eq!(config.model, expected);
    }

    #[test]
    fn test_launch_config_reviewer_factory() {
        let config =
            LaunchConfig::reviewer("york".to_string(), "myrepo", 42, 0, AuthProvider::Claude);
        assert_eq!(config.name, "york");
        assert_eq!(config.pr_number, Some(42));
        assert_eq!(config.role, CoworkerRole::Reviewer);
        assert!(config.team_name.is_none());
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Reviewer)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        assert_eq!(config.model, expected);
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
        // pr_handoff reads coworker config with "opus" as fallback
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Coworker)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        assert_eq!(config.model, expected);
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

    // --- Shell command tests ---

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
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS='1'"),
            "Shell command should contain CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS='1', got: {}",
            result.shell_command
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

    #[test]
    fn test_to_headless_config_uses_zai_env_model_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("api_key.txt"), "test-key\n").expect("write api key");
        std::fs::write(
            dir.path().join("base_url.txt"),
            "https://api.z.ai/api/anthropic\n",
        )
        .expect("write base url");

        let mut config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        config.auth_provider = crate::auth::AuthProvider::Zai;
        config.auth_profile_dir = Some(dir.path().to_path_buf());
        config.model = "haiku".to_string();

        let headless = config.to_headless_config("midtown");

        assert_eq!(
            headless.env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            Some(&"GLM-4.5-Air".to_string())
        );
        assert_eq!(
            headless.env.get("ANTHROPIC_DEFAULT_SONNET_MODEL"),
            Some(&"GLM-4.7".to_string())
        );
        assert_eq!(
            headless.env.get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
            Some(&"GLM-5".to_string())
        );
        assert_eq!(headless.model, "haiku");
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

    // --- Lead role tests ---

    #[test]
    fn test_headless_config_lead_role_uses_lead_prompt() {
        let config = LaunchConfig::lead("myrepo", None);
        let headless = config.to_headless_config("midtown");

        // Lead should use main_lead_system_prompt (not coworker)
        assert!(
            !headless.system_prompt.is_empty(),
            "Lead should have a non-empty system prompt"
        );
        // Verify it's the lead prompt by checking it doesn't contain coworker-specific text
        // (lead prompt and coworker prompt are structurally different)
        assert_eq!(config.role, CoworkerRole::Lead);
    }

    #[test]
    fn test_lead_config_setting_sources_handled_by_platform_builder() {
        // All sessions now get --setting-sources project,local via the platform
        // arg builder. The HeadlessConfig.setting_sources field is always None.
        let config = LaunchConfig::lead("myrepo", None);
        let headless = config.to_headless_config("midtown");
        assert_eq!(
            headless.setting_sources, None,
            "setting_sources should be None — handled by platform arg builder"
        );
    }

    #[test]
    fn test_launch_config_lead_factory() {
        let config = LaunchConfig::lead("myrepo", None);
        assert_eq!(
            config.name, "myrepo",
            "Lead session name should be the repo name"
        );
        assert_eq!(config.role, CoworkerRole::Lead);
        assert!(
            config.initial_prompt.is_some(),
            "Lead should have an initial prompt for session_id init"
        );
        assert!(config.pr_number.is_none());
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::Lead)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "opus".to_string());
        assert_eq!(config.model, expected, "Lead model should respect config");
    }

    #[test]
    fn test_lead_config_has_team_name() {
        let config = LaunchConfig::lead("myrepo", None);
        let headless = config.to_headless_config("midtown");

        assert_eq!(headless.team_name, Some("midtown-myrepo".to_string()));
        assert_eq!(headless.agent_id, Some("myrepo@midtown-myrepo".to_string()));
        assert_eq!(headless.agent_name, Some("myrepo".to_string()));
    }

    #[test]
    fn test_launch_config_channel_lead_factory() {
        let config =
            LaunchConfig::channel_lead("daemon-architecture", "myrepo", SessionMode::Fresh, "");
        // Session name is the channel name directly
        assert_eq!(config.name, "daemon-architecture");
        assert_eq!(
            config.role,
            CoworkerRole::ChannelLead {
                channel_name: "daemon-architecture".to_string(),
                domain_context: "".to_string(),
            }
        );
        let expected =
            crate::config::get_model_for_role("myrepo", crate::config::ExecutionRole::ChannelLead)
                .map(|s| s.as_model_str().to_string())
                .unwrap_or_else(|| "sonnet".to_string());
        assert_eq!(config.model, expected);
        assert_eq!(config.channel, Some("daemon-architecture".to_string()));
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
        assert!(config.initial_prompt.is_some());
        assert!(config.pr_number.is_none());
    }

    #[test]
    fn test_channel_lead_session_name() {
        assert_eq!(channel_lead_session_name("auth"), "auth");
        assert_eq!(channel_lead_session_name("web-interface"), "web-interface");
        assert_eq!(channel_lead_session_name("park"), "park");
    }

    #[test]
    fn test_channel_lead_headless_config_has_system_prompt() {
        let config = LaunchConfig::channel_lead("tui", "myrepo", SessionMode::Fresh, "");
        let headless = config.to_headless_config("midtown");
        // Channel lead system prompt references the channel name
        assert!(
            headless.system_prompt.contains("tui"),
            "System prompt should reference the channel name"
        );
    }
}

#[path = "launch_tests.rs"]
#[cfg(test)]
mod launch_tests;
