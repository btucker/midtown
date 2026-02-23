//! Platform abstraction for coding agent CLI argument construction.
//!
//! Separates two orthogonal concerns that were previously conflated:
//! - **AuthProvider** (how you authenticate): Claude config dir, Codex home, z.ai API key
//! - **Platform** (which binary and CLI protocol to use): `claude` vs `codex`
//!
//! The shared arg builders (`build_claude_common_args`, `build_claude_headless_args`,
//! `build_claude_headed_args`) are the single source of truth for CLI flag construction.
//! Both headed (interactive terminal) and headless (JSON streaming) launch paths
//! delegate to these builders, eliminating the sync burden that previously caused bugs.

use std::path::{Path, PathBuf};

use crate::auth::AuthProvider;
use crate::headless::HeadlessConfig;
use crate::launch::{LaunchConfig, SessionMode};

/// The coding agent platform — which binary and CLI protocol to use.
///
/// Separate from `AuthProvider` (which handles authentication). Multiple auth
/// providers can map to the same platform (e.g., both `Claude` and `Zai`
/// providers use the `Claude` platform's `claude` binary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Platform {
    /// Claude Code CLI (`claude`).
    Claude,
    /// Codex CLI (`codex` / `codex app-server`).
    Codex,
}

impl Platform {
    /// Map an auth provider to its corresponding platform.
    ///
    /// z.ai uses Claude's binary with different auth env vars.
    pub fn from_provider(provider: AuthProvider) -> Self {
        match provider {
            AuthProvider::Claude | AuthProvider::Zai => Platform::Claude,
            AuthProvider::Codex => Platform::Codex,
        }
    }

    /// The executable name for this platform.
    pub fn binary_name(&self) -> &'static str {
        match self {
            Platform::Claude => "claude",
            Platform::Codex => "codex",
        }
    }
}

// ============================================================================
// Claude platform arg builders
// ============================================================================

/// Args shared by ALL Claude sessions (headed + headless).
///
/// Always produces:
/// - `--dangerously-skip-permissions`
/// - `--model <model>`
/// - `--setting-sources project,local`
///
/// Conditionally adds:
/// - `--agent-id`, `--agent-name`, `--team-name` (when team is set)
/// - `--add-dir` (for each additional directory)
fn build_claude_common_args(
    model: &str,
    team_name: Option<&str>,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
    additional_dirs: &[PathBuf],
) -> Vec<String> {
    let mut args = vec!["--dangerously-skip-permissions".to_string()];

    // Additional directories (multi-repo)
    for dir in additional_dirs {
        if let Some(d) = dir.to_str() {
            args.push("--add-dir".to_string());
            args.push(d.to_string());
        }
    }

    // Setting sources — unconditional for all sessions
    args.push("--setting-sources".to_string());
    args.push("project,local".to_string());

    // Model selection
    args.push("--model".to_string());
    args.push(model.to_string());

    // Agent teams flags (enables mailbox-based message delivery)
    if let Some(team) = team_name {
        if let Some(id) = agent_id {
            args.push("--agent-id".to_string());
            args.push(id.to_string());
        }
        if let Some(name) = agent_name {
            args.push("--agent-name".to_string());
            args.push(name.to_string());
        }
        args.push("--team-name".to_string());
        args.push(team.to_string());
    }

    args
}

/// Build CLI args for a headless Claude session (JSON streaming mode).
///
/// Calls `build_claude_common_args()` for shared flags, then adds
/// headless-specific flags. Does NOT include the binary name — the caller
/// handles sandbox prefix + binary separately.
///
/// Fresh sessions get: `-p`, `--append-system-prompt`, `--json-schema`,
///   `--session-id` (when pre-assigned), `--settings`, `--setting-sources` (from common).
///
/// Resume sessions get: `--resume <id>` and skip `--settings`/`--setting-sources`
///   to avoid "Tool names must be unique" errors.
pub fn build_claude_headless_args(config: &HeadlessConfig) -> Vec<String> {
    // Exhaustive destructure so new HeadlessConfig fields force explicit
    // handling decisions in this platform mapper.
    let HeadlessConfig {
        model,
        system_prompt,
        json_schema,
        cwd: _cwd,
        project_name: _project_name,
        max_budget_usd,
        allow_tools,
        persist_session,
        resume_session_id,
        inactivity_timeout: _inactivity_timeout,
        team_name,
        agent_id,
        agent_name,
        settings_path,
        setting_sources: _setting_sources,
        auth_provider: _auth_provider,
        session_id,
        env: _env,
        fork_session,
    } = config;

    let is_resume = resume_session_id.is_some();

    let mut args = build_claude_common_args(
        model,
        team_name.as_deref(),
        agent_id.as_deref(),
        agent_name.as_deref(),
        &[], // headless sessions don't use additional_dirs
    );

    if is_resume {
        // Resume mode: --resume <id>, no -p flag, no system prompt/schema
        args.push("--resume".to_string());
        args.push(resume_session_id.as_ref().unwrap().clone());
        // Fork mode: add --fork-session to create an independent fork with inherited context
        if *fork_session {
            args.push("--fork-session".to_string());
        }
    } else {
        // Fresh mode: -p with system prompt
        args.push("-p".to_string());
        args.push("--append-system-prompt".to_string());
        args.push(system_prompt.clone());

        if let Some(schema) = json_schema
            && let Ok(schema_str) = serde_json::to_string(schema)
        {
            args.push("--json-schema".to_string());
            args.push(schema_str);
        }

        // Pass pre-assigned session ID so the daemon knows the session ID immediately
        // at spawn time, eliminating the race window before the init event arrives.
        if let Some(sid) = session_id {
            args.push("--session-id".to_string());
            args.push(sid.clone());
        }
    }

    // Streaming protocol flags
    args.push("--verbose".to_string());
    args.push("--output-format".to_string());
    args.push("stream-json".to_string());
    args.push("--input-format".to_string());
    args.push("stream-json".to_string());

    // Session persistence
    if !*persist_session {
        args.push("--no-session-persistence".to_string());
    }

    // Budget limit (used by specialized coworkers and headless RPC)
    if let Some(budget) = max_budget_usd {
        args.push("--max-budget-usd".to_string());
        args.push(budget.to_string());
    }

    // Tool access
    if !*allow_tools {
        args.push("--tools".to_string());
        args.push(String::new());
    }

    // Settings file — skip on resume to avoid duplicate tool registrations.
    // Resumed sessions already have their plugins loaded from saved state;
    // passing --settings again causes "Tool names must be unique" API errors.
    if !is_resume && let Some(settings) = settings_path {
        args.push("--settings".to_string());
        args.push(settings.clone());
    }

    args
}

/// Build CLI args for a headed (interactive terminal) Claude session.
///
/// Calls `build_claude_common_args()` for shared flags, then adds
/// headed-specific flags. The returned Vec starts with `"claude"`.
///
/// Returns `(args, session_id)` where `session_id` is `Some(uuid)` for fresh sessions.
///
/// Does NOT include sandbox prefix or env vars — callers add those.
pub fn build_claude_headed_args(
    config: &LaunchConfig,
    settings_file: &Path,
    prompt_file: &Path,
    initial_prompt_file: Option<&Path>,
) -> (Vec<String>, Option<String>) {
    // Generate agent teams IDs from name + team
    let (agent_id, agent_name) = if let Some(ref team) = config.team_name {
        (
            Some(crate::mailbox::agent_id(&config.name, team)),
            Some(config.name.clone()),
        )
    } else {
        (None, None)
    };

    let mut args = vec!["claude".to_string()];

    args.extend(build_claude_common_args(
        &config.model,
        config.team_name.as_deref(),
        agent_id.as_deref(),
        agent_name.as_deref(),
        &config.additional_dirs,
    ));

    // Session mode — exactly one of these
    let session_id = match &config.session_mode {
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

    // Settings file — always included for headed sessions
    args.push("--settings".to_string());
    args.push(settings_file.display().to_string());

    // System prompt file — use "$(cat ...)" pattern for shell interpretation.
    // The double quotes inside the string allow shell command substitution.
    // When shell-quoted, this becomes: '"$(cat /path)"' which the shell interprets correctly.
    args.push("--append-system-prompt".to_string());
    args.push(format!("\"$(cat {})\"", prompt_file.display()));

    // Initial prompt as bare positional arg (NOT -p/--print).
    // Written to temp file by caller; path passed in here.
    // This MUST be the last argument. See PR #447 for why -p is forbidden.
    if let Some(path) = initial_prompt_file {
        args.push(format!("\"$(cat {})\"", path.display()));
    }

    (args, session_id)
}

// ============================================================================
// Codex platform arg builders
// ============================================================================

/// Build CLI args for a headless Codex session (JSON-RPC app-server).
///
/// Codex app-server is a persistent JSON-RPC stdio server. Thread management
/// (start/resume) happens via JSON-RPC requests after launch, not CLI flags.
pub fn build_codex_headless_args() -> Vec<String> {
    vec!["app-server".to_string()]
}

/// Build CLI args for a headed (interactive terminal) Codex session.
///
/// Codex accepts developer instructions as a config override, so we inject
/// Midtown's role prompt via `-c developer_instructions=<TOML string>`.
pub fn build_codex_headed_args(session_id: &str, system_prompt: &str) -> Vec<String> {
    let mut args = vec!["--resume".to_string(), session_id.to_string()];
    if !system_prompt.is_empty() {
        let prompt_toml = toml::Value::String(system_prompt.to_string()).to_string();
        args.push("-c".to_string());
        args.push(format!("developer_instructions={prompt_toml}"));
    }
    args
}

#[path = "platform_tests.rs"]
#[cfg(test)]
mod tests;
