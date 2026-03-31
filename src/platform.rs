//! Platform abstraction for coding agent CLI argument construction.
//!
//! Separates two orthogonal concerns that were previously conflated:
//! - **AuthProvider** (how you authenticate): Claude config dir, Codex home
//! - **Platform** (which binary and CLI protocol to use): `claude` vs `codex`
//!
//! The shared arg builders (`build_claude_common_args`, `build_claude_headless_args`,
//! `build_claude_interactive_args`) are the single source of truth for CLI flag construction.
//! Both interactive (terminal) and headless (JSON streaming) launch paths
//! delegate to these builders, eliminating the sync burden that previously caused bugs.

use std::path::{Path, PathBuf};

use crate::auth::AuthProvider;
use crate::headless::HeadlessConfig;
use crate::launch::{LaunchConfig, SessionMode};

/// The coding agent platform — which binary and CLI protocol to use.
///
/// Separate from `AuthProvider` (which handles authentication).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Platform {
    /// Claude Code CLI (`claude`).
    Claude,
    /// Codex CLI (`codex` / `codex app-server`).
    Codex,
}

impl Platform {
    /// Map an auth provider to its corresponding platform.
    pub fn from_provider(provider: AuthProvider) -> Self {
        match provider {
            AuthProvider::Claude => Platform::Claude,
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

/// Args shared by ALL Claude sessions (interactive + headless).
///
/// Always produces:
/// - `--dangerously-skip-permissions`
/// - `--model <model>`
///
/// Conditionally adds:
/// - `--add-dir` (for each additional directory)
///
/// Note: `--setting-sources` is NOT included here. It must be added by callers
/// that need it. Legacy fork-resume sessions (`--resume --fork-session`) are
/// incompatible with `--setting-sources`, so callers must decide based on their
/// session type. (New fork sessions launch as fresh sessions and always include
/// `--setting-sources`.)
fn build_claude_common_args(model: &str, additional_dirs: &[PathBuf]) -> Vec<String> {
    let mut args = vec!["--dangerously-skip-permissions".to_string()];

    // Additional directories (multi-repo)
    for dir in additional_dirs {
        if let Some(d) = dir.to_str() {
            args.push("--add-dir".to_string());
            args.push(d.to_string());
        }
    }

    // Model selection
    args.push("--model".to_string());
    args.push(model.to_string());

    args
}

/// Args shared by all interactive Codex sessions.
///
/// Codex does not have a direct equivalent to Claude's `--dangerously-skip-permissions`,
/// so Midtown relies on an external sandbox (when available) and tells Codex to bypass
/// its own approval/sandbox layer to avoid double-prompting.
fn build_codex_common_args(model: Option<&str>, additional_dirs: &[PathBuf]) -> Vec<String> {
    let mut args = vec!["--dangerously-bypass-approvals-and-sandbox".to_string()];

    for dir in additional_dirs {
        if let Some(d) = dir.to_str() {
            args.push("--add-dir".to_string());
            args.push(d.to_string());
        }
    }

    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
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
///   `--session-id` (when pre-assigned), `--settings`, `--setting-sources`.
///
/// Resume sessions get: `--resume <id>`, `--setting-sources`, `--agent` (when set),
///   and skip `--settings` to avoid "Tool names must be unique" errors.
///
/// Fork-resume sessions (legacy two-step path) get: `--resume <id>`,
///   `--fork-session`, `--session-id <uuid>`, and skip both `--settings` and
///   `--setting-sources` (the latter is incompatible with `--fork-session`).
///   Note: `build_fork_config` now launches forks as fresh sessions instead,
///   so this path is only used by explicit `fork_session: true` configs.
///
/// Both modes may include `--disallowedTools` (comma-separated) for hard tool
/// restrictions that the LLM cannot bypass (e.g., channel lead code-edit blocks).
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
        settings_path,
        setting_sources: _setting_sources,
        auth_provider: _auth_provider,
        session_id,
        env: _env,
        fork_session,
        disallowed_tools,
        agent_name,
        additional_dirs,
        output_notify: _output_notify,
    } = config;

    let is_resume = resume_session_id.is_some();
    let is_fork = is_resume && *fork_session;

    let mut args = build_claude_common_args(model, additional_dirs);

    // Setting sources — skip for legacy fork-resume sessions because
    // --setting-sources is incompatible with --resume --fork-session in the
    // Claude CLI. New fork sessions launch as fresh sessions (fork_session=false)
    // and always get --setting-sources.
    if !is_fork {
        args.push("--setting-sources".to_string());
        args.push("project,local".to_string());
    }

    // Agent definition: --agent is passed for both fresh and resume sessions.
    // On fresh sessions, Layer 1 comes from the agent definition file.
    // On resume sessions, --agent ensures the agent identity is available for
    // task handoff via --resume --agent.
    if let Some(name) = agent_name {
        args.push("--agent".to_string());
        args.push(name.clone());
    }

    if is_resume {
        // Resume mode: --resume <id>, no -p flag, no system prompt/schema
        args.push("--resume".to_string());
        args.push(resume_session_id.as_ref().unwrap().clone());
        // Fork mode: add --fork-session to create an independent fork with inherited context
        if *fork_session {
            args.push("--fork-session".to_string());
            // Pre-assign session_id for fork sessions so the daemon controls the fork's
            // session ID immediately at spawn time. Forked sessions (which use --resume
            // under the hood) don't emit the system/init event, so the daemon cannot
            // discover the session_id from the event stream. Passing --session-id here
            // mirrors the fresh-session approach and eliminates the 30s init timeout.
            if let Some(sid) = session_id {
                args.push("--session-id".to_string());
                args.push(sid.clone());
            }
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

    // Tool restrictions — hard enforcement via CLI flag, not bypassable by LLM
    if !disallowed_tools.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(disallowed_tools.join(","));
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

/// Build CLI args for an interactive (terminal) Claude session.
///
/// Calls `build_claude_common_args()` for shared flags, then adds
/// interactive-specific flags. The returned Vec starts with `"claude"`.
///
/// Returns `(args, session_id)` where `session_id` is `Some(uuid)` for fresh sessions.
///
/// Does NOT include sandbox prefix or env vars — callers add those.
pub fn build_claude_interactive_args(
    config: &LaunchConfig,
    settings_file: &Path,
    prompt_file: &Path,
    initial_prompt_file: Option<&Path>,
) -> (Vec<String>, Option<String>) {
    let mut args = vec!["claude".to_string()];

    args.extend(build_claude_common_args(
        &config.model,
        &config.additional_dirs,
    ));

    // Setting sources — always included for interactive sessions
    args.push("--setting-sources".to_string());
    args.push("project,local".to_string());

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

    // Settings file — always included for interactive sessions
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

/// Build CLI args for an interactive (terminal) Codex session.
///
/// Midtown injects the role prompt via `-c developer_instructions=<TOML string>`.
/// Fresh sessions set `--model` explicitly, but resume flows intentionally omit
/// `--model` so Codex keeps the saved thread's original model instead of being
/// rewritten to Midtown role defaults during attach or `resume --last`.
///
/// Returns `(args, session_id)`. Codex's interactive CLI does not expose a way
/// to pre-assign the eventual thread/session ID, so the second tuple element is
/// always `None`.
pub fn build_codex_interactive_args(
    config: &LaunchConfig,
    system_prompt: &str,
    initial_prompt: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let mut args = vec!["codex".to_string()];
    let model = match config.session_mode {
        SessionMode::Fresh => Some(config.model.as_str()),
        SessionMode::Resume | SessionMode::ResumeSession(_) => None,
    };
    args.extend(build_codex_common_args(model, &config.additional_dirs));

    if !system_prompt.is_empty() {
        let prompt_toml = toml::Value::String(system_prompt.to_string()).to_string();
        args.push("-c".to_string());
        args.push(format!("developer_instructions={prompt_toml}"));
    }

    match &config.session_mode {
        SessionMode::Fresh => {}
        SessionMode::Resume => {
            args.push("resume".to_string());
            args.push("--last".to_string());
        }
        SessionMode::ResumeSession(id) => {
            args.push("resume".to_string());
            args.push(id.clone());
        }
    }

    if let Some(prompt) = initial_prompt {
        args.push(prompt.to_string());
    }

    (args, None)
}

#[path = "platform_tests.rs"]
#[cfg(test)]
mod tests;
