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

/// Whether this coworker shares the team task list or is isolated.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskMode {
    /// Shares the team task list via CLAUDE_CODE_TASK_LIST_ID env var.
    Shared { repo_name: String },
    /// Private task list — no shared env var (used for reviewers).
    Isolated,
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
    /// Shared vs isolated task list.
    pub task_mode: TaskMode,
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
}

/// The shell command string and generated session ID (if fresh).
pub struct LaunchCommand {
    pub shell_command: String,
    pub session_id: Option<String>,
}

impl LaunchConfig {
    /// Create a config for a standard coworker with an isolated task list.
    ///
    /// Coworkers don't share the lead's task list. The daemon bakes the task
    /// description into the initial prompt and tracks assignment internally.
    /// The `repo_name` parameter is retained for compatibility but is no longer
    /// used for task list sharing.
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
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::Coworker,
            initial_prompt,
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
        }
    }

    /// Create a config for an isolated reviewer coworker.
    ///
    /// Reviewers get a specialized system prompt that merges coworker.md +
    /// common.md + reviewer.md, ensuring they follow reviewer instructions
    /// as behavioral rules rather than just task descriptions.
    pub fn reviewer(name: impl Into<String>, pr_number: u64) -> Self {
        LaunchConfig {
            name: name.into(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::Reviewer,
            initial_prompt: Some(crate::agents::reviewer_launch_prompt(pr_number)),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: Some(pr_number),
            team_name: None, // Reviewers don't need mailbox (short-lived)
            working_dir: None,
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
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::Coworker,
            initial_prompt: Some(initial_prompt),
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: None,
            team_name: Some(team),
            working_dir: None,
        }
    }

    /// Convert to a `HeadlessConfig` for headless session spawn.
    ///
    /// Generates the system prompt based on the coworker's role, and maps
    /// session mode to `persist_session` / `resume_session_id` fields.
    pub fn to_headless_config(&self) -> HeadlessConfig {
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
        if let TaskMode::Shared { ref repo_name } = self.task_mode {
            let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
            env.insert("CLAUDE_CODE_TASK_LIST_ID".to_string(), task_list_id);
        }
        // Set Claude config directory from the active auth profile
        let config_dir = crate::auth::current_profile_dir();
        env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            config_dir.to_string_lossy().to_string(),
        );

        HeadlessConfig {
            model: "sonnet".to_string(), // Default model for coworkers
            system_prompt,
            json_schema: None,
            cwd: None, // Set by caller (worktree path)
            max_budget_usd: None,
            allow_tools: true, // Coworkers need full tool access
            persist_session,
            resume_session_id,
            inactivity_timeout: None,
            team_name: self.team_name.clone(),
            agent_id,
            agent_name,
            settings_path: None, // Set by caller
            env,
        }
    }

    /// Build the full shell command string for launching Claude in a tmux pane.
    ///
    /// `settings_file` and `prompt_file` are pre-written files containing the
    /// Claude settings JSON and system prompt markdown. `initial_prompt_file`
    /// is the optional pre-written file containing the initial task/review prompt.
    ///
    /// Returns a `LaunchCommand` with the shell command and the session ID
    /// (if a fresh session was created).
    pub fn to_shell_command(
        &self,
        settings_file: &std::path::Path,
        prompt_file: &std::path::Path,
        initial_prompt_file: Option<&std::path::Path>,
    ) -> LaunchCommand {
        // -- Environment variables --
        let mut env_parts = vec![
            format!("MIDTOWN_AGENT='{}'", self.name),
            "DISABLE_AUTOUPDATER=1".to_string(),
        ];
        if let TaskMode::Shared { ref repo_name } = self.task_mode {
            let task_list_id = crate::paths::task_list_id_for_repo(repo_name);
            env_parts.push(format!("CLAUDE_CODE_TASK_LIST_ID='{}'", task_list_id));
        }
        // Set Claude config directory from the active auth profile
        let config_dir = crate::auth::current_profile_dir();
        env_parts.push(format!("CLAUDE_CONFIG_DIR='{}'", config_dir.display()));
        // Must be a real shell env var — Claude Code blocklists this from settings.json
        if self.team_name.is_some() {
            env_parts.push("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1".to_string());
        }
        let env_export = format!("export {}", env_parts.join(" "));

        // -- Claude CLI arguments (as structured Vec, not format! interpolation) --
        let mut args: Vec<String> = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_headless_config_fresh_coworker() {
        let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
        let headless = config.to_headless_config();

        assert!(headless.persist_session);
        assert!(headless.resume_session_id.is_none());
        assert!(headless.allow_tools);
        assert_eq!(headless.team_name, Some("midtown-myrepo".to_string()));
        assert_eq!(headless.agent_id, Some("park@midtown-myrepo".to_string()));
        assert_eq!(headless.agent_name, Some("park".to_string()));
        assert!(!headless.system_prompt.is_empty());
    }

    #[test]
    fn test_to_headless_config_resume_session() {
        let config = LaunchConfig {
            session_mode: SessionMode::ResumeSession("abc-123".to_string()),
            ..LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None)
        };
        let headless = config.to_headless_config();

        assert!(headless.persist_session);
        assert_eq!(headless.resume_session_id, Some("abc-123".to_string()));
    }

    #[test]
    fn test_to_headless_config_reviewer_has_no_teams() {
        let config = LaunchConfig::reviewer("york", 42);
        let headless = config.to_headless_config();

        assert!(headless.team_name.is_none());
        assert!(headless.agent_id.is_none());
        assert!(headless.agent_name.is_none());
    }

    #[test]
    fn test_to_headless_config_reviewer_has_tools() {
        let config = LaunchConfig::reviewer("york", 42);
        let headless = config.to_headless_config();
        assert!(headless.allow_tools);
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
        assert_eq!(config.task_mode, TaskMode::Isolated);
        assert_eq!(config.role, CoworkerRole::Coworker);
        assert_eq!(config.initial_prompt, Some("Do the thing".to_string()));
        assert!(config.restrict_setting_sources);
        assert!(config.pr_number.is_none());
        assert_eq!(config.team_name, Some("midtown-myrepo".to_string()));
    }

    #[test]
    fn test_launch_config_reviewer_factory() {
        let config = LaunchConfig::reviewer("york".to_string(), 42);
        assert_eq!(config.name, "york");
        assert_eq!(config.pr_number, Some(42));
        assert_eq!(config.role, CoworkerRole::Reviewer);
        assert!(config.team_name.is_none());
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
        );
        assert!(!result.shell_command.contains("--agent-id"));
        assert!(
            !result
                .shell_command
                .contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS")
        );
    }
}
