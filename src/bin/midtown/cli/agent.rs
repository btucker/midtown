//! CLI subcommands for `midtown agent` — unified agent management.
//!
//! Merges the former `midtown session`, `midtown coworker`, and `midtown lead`
//! commands into a single `midtown agent` namespace.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use clap::{Args, Subcommand};

use super::Response;
use super::session_render;
use crate::client::DaemonClient;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderArg {
    Claude,
    Codex,
    Zai,
}

impl From<ProviderArg> for midtown::auth::AuthProvider {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            ProviderArg::Codex => midtown::auth::AuthProvider::Codex,
            ProviderArg::Zai => midtown::auth::AuthProvider::Zai,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum AgentCommand {
    /// Spawn a new coworker
    Spawn {
        /// Resume the previous Claude session (passes --continue to claude)
        #[arg(long)]
        resume: bool,
        /// Initial prompt to send after spawning (avoids separate nudge step)
        #[arg(long, short)]
        prompt: Option<String>,
        /// Execution provider for this coworker
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
        /// Load an agent definition file (~/.claude/agents/{name}.md or .claude/agents/{name}.md)
        #[arg(long)]
        agent: Option<String>,
        /// Route coworker messages to a specific channel
        #[arg(long)]
        channel: Option<String>,
        /// Route coworker messages to a specific thread
        #[arg(long)]
        thread: Option<String>,
        /// Immediately assign this task to the spawned coworker
        #[arg(long)]
        task: Option<String>,
    },
    /// Stop a coworker (send on break)
    Stop {
        /// Name of the coworker to stop
        name: String,
    },
    /// Show an agent's current output with rich rendering
    Show {
        /// Agent target (coworker name, task/<id>, pr/<number>, claude, etc.)
        target: String,
        /// Continuously tail and render new output as it arrives (headless sessions only)
        #[arg(long, short = 'w')]
        watch: bool,
    },
    /// Attach to a headless coworker's session.
    Attach {
        #[command(flatten)]
        target: AttachArgs,
    },
    /// Detach from an attached session (resume headless execution)
    Detach {
        /// Name of the coworker to detach
        name: String,
    },
    /// Nudge a coworker to check in
    Nudge {
        /// Name of the coworker to nudge
        name: String,
        /// Custom message (optional)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Fork the current session to handle a specific thread.
    ///
    /// Creates an independent fork of this session with inherited conversation
    /// history. The fork becomes the handler for user replies in that thread.
    Fork {
        /// The message ID of the thread root to fork for.
        #[arg(long = "thread-id")]
        thread_id: String,
        /// The session ID of the calling session. Falls back to $MIDTOWN_SESSION_ID.
        #[arg(long = "session-id")]
        session_id: Option<String>,
        /// Optional descriptive name for the fork (e.g. "investigate auth bug").
        #[arg(long)]
        name: Option<String>,
        /// Optional initial message to send to the fork. If provided, the fork
        /// receives this as its first nudge instead of the default framing.
        #[arg(long = "initial-message")]
        initial_message: Option<String>,
        /// Avatar color override (CSS color string, e.g., "#ff5f5f")
        #[arg(long)]
        color: Option<String>,
        /// Lucide icon name for avatar (e.g., "shield", "database")
        #[arg(long)]
        icon: Option<String>,
    },
    /// Clear a session: stop it and restart fresh with the same initial prompt.
    Clear {
        /// Session target (coworker name, task/<id>, pr/<number>, etc.)
        target: String,
    },
    /// Upload a local image to GitHub and return embeddable markdown
    UploadImage {
        /// Path to the local image file
        path: String,
        /// Alt text for the markdown image tag (default: "screenshot")
        #[arg(long, default_value = "screenshot")]
        alt: String,
    },
    /// List all agents and attachable sessions
    #[command(alias = "ls")]
    List,
    /// Register this session for task sharing with coworkers
    #[command(hide = true)]
    RegisterSession,
}

pub fn handle(cmd: &AgentCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        AgentCommand::Spawn {
            resume,
            prompt,
            provider,
            agent,
            channel,
            thread,
            task,
        } => {
            let resolved_provider = provider.map(Into::into).unwrap_or_else(|| {
                let project_name = midtown::paths::detect_repo_name().unwrap_or_default();
                midtown::config::get_execution_provider_for_role(
                    &project_name,
                    midtown::config::ExecutionRole::Coworker,
                )
            });
            client.coworker_spawn(
                *resume,
                prompt.as_deref(),
                resolved_provider,
                agent.as_deref(),
                channel.as_deref(),
                thread.as_deref(),
                task.as_deref(),
            )
        }
        AgentCommand::Stop { name } => client.coworker_break(name),
        AgentCommand::Show { target, watch } => handle_show(target, *watch, client),
        AgentCommand::Attach { target } => handle_attach(target, client),
        AgentCommand::Detach { name } => client.session_detach(name),
        AgentCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
        AgentCommand::Fork {
            thread_id,
            session_id,
            name,
            initial_message,
            color,
            icon,
        } => {
            let sid = session_id
                .clone()
                .or_else(|| std::env::var("MIDTOWN_SESSION_ID").ok())
                .ok_or_else(|| {
                    "Missing session ID. Pass --session-id or set $MIDTOWN_SESSION_ID.".to_string()
                })?;
            client.session_fork(
                thread_id,
                &sid,
                name.as_deref(),
                initial_message.as_deref(),
                color.as_deref(),
                icon.as_deref(),
            )
        }
        AgentCommand::Clear { target } => client.session_clear(target),
        AgentCommand::UploadImage { .. } => {
            // Handled before daemon connection in main.rs
            unreachable!("UploadImage is handled locally without daemon connection")
        }
        AgentCommand::List => client.session_list(),
        AgentCommand::RegisterSession => super::handle_register_session(),
    }
}

// ── Attach types ─────────────────────────────────────────────────────

#[derive(Args, Debug, Clone)]
pub(crate) struct AttachArgs {
    /// Attach target.
    ///
    /// Supported one-token forms:
    /// - `name/<coworker>`
    /// - `task/<id>`
    /// - `pr/<number>`
    /// - `claude/<session_id>`
    /// - `codex/<session_id>`
    ///
    /// Legacy `name:...`, `task:...`, `pr:...` is accepted.
    /// Bare `<coworker>` is interpreted as `name/<coworker>`.
    #[arg(value_name = "TARGET")]
    target: String,

    /// Optional second value for compatibility with two-token input,
    /// eg `name park`, `task 42`, `pr 123`.
    #[arg(value_name = "VALUE")]
    value: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct AttachCandidate {
    name: String,
    session_id: String,
    provider: String,
    platform: String,
    cwd: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    attached: bool,
    #[serde(default)]
    last_active: Option<String>,
    #[serde(default)]
    last_active_age_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ResolvePayload {
    candidates: Vec<AttachCandidate>,
    #[serde(default)]
    resolved_at_unix_ms: Option<u64>,
    #[serde(default)]
    resolved_at_monotonic_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AttachLaunchOptions<'a> {
    pub profile: Option<&'a str>,
    pub coworker_type: Option<&'a str>,
    pub channel: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AttachShellOptions<'a> {
    pub launch: AttachLaunchOptions<'a>,
    pub include_detach: bool,
}

#[derive(Debug)]
pub(crate) struct AttachLaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
}

// ── Attach handler ───────────────────────────────────────────────────

/// Handle attach: resolve target -> pause headless -> run interactive CLI -> auto-detach on exit.
fn handle_attach(target: &AttachArgs, client: &DaemonClient) -> Result<Response, String> {
    let mut target_str = normalize_attach_target(target)?;
    let mut retried_after_race = false;
    let mut attempted_auto_create = false;

    loop {
        // Step 1: Resolve target to attachable sessions.
        let resolved = match resolve_attach_candidates(client, &target_str) {
            Ok(resolved) => resolved,
            Err(err) if !attempted_auto_create && should_auto_create_session(&err) => {
                attempted_auto_create = true;
                let created_target = create_attach_target(client, &target_str)?;
                target_str = created_target;
                continue;
            }
            Err(err) => return Err(err),
        };
        let selected = choose_attach_candidate(&target_str, &resolved)?;

        // Step 2: Ask daemon to pause the selected headless session and return session info.
        let info = match client.session_attach(&format!("name/{}", selected.name)) {
            Ok(info) => info,
            Err(err) if is_attach_race_error(&err) && !retried_after_race => {
                retried_after_race = true;
                eprintln!(
                    "Selected session changed while attaching ({}). Re-resolving...",
                    err
                );
                continue;
            }
            Err(err) => return Err(err),
        };

        let session_id = info
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or("Daemon did not return session_id")?;
        let cwd = info
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or("Daemon did not return cwd")?;
        let name = info
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("Daemon did not return name")?;
        let provider = parse_provider(
            info.get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("claude"),
        );
        let profile = info
            .get("profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let coworker_type = info
            .get("coworker_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let channel = info
            .get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let profile_dir = profile
            .as_deref()
            .map(|name| midtown::auth::profile_dir_for(provider, name));
        if let Err(e) =
            midtown::platform_launch::run_platform_prelaunch_hook(provider, profile_dir.as_deref())
        {
            eprintln!(
                "Warning: Platform pre-launch hook failed (continuing): {}",
                e
            );
        }

        // Ensure worktree is set up before launching.
        let cwd = ensure_attach_worktree(name, cwd, coworker_type.as_deref() == Some("lead"))?;

        let launch_spec = build_attach_launch_spec(
            &cwd,
            name,
            provider,
            session_id,
            AttachLaunchOptions {
                profile: profile.as_deref(),
                coworker_type: coworker_type.as_deref(),
                channel: channel.as_deref(),
            },
        )?;

        // Step 3: Run the provider CLI directly in the current terminal.
        let status = Command::new(&launch_spec.program)
            .args(&launch_spec.args)
            .envs(&launch_spec.env)
            .current_dir(&cwd)
            .status()
            .map_err(|e| {
                // If launch fails, tell daemon to resume headless.
                match client.session_detach(name) {
                    Ok(_) => eprintln!(
                        "Attach launch failed; headless session resumed for {}.",
                        name
                    ),
                    Err(detach_err) => eprintln!(
                        "ERROR: Attach launch failed AND detach RPC failed for {}.\n\
                         Launch error: {}\n\
                         Detach error: {}\n\
                         Manual recovery: run `midtown agent detach {}`",
                        name, e, detach_err, name
                    ),
                }
                format!("Failed to launch interactive session: {}", e)
            })?;

        // Always detach on exit so the daemon resumes headless mode.
        if let Err(detach_err) = client.session_detach(name) {
            eprintln!(
                "ERROR: Interactive session exited but detach RPC failed for {}.\n\
                 Exit status: {:?}\n\
                 Detach error: {}\n\
                 Manual recovery: run `midtown agent detach {}`",
                name,
                status.code(),
                detach_err,
                name
            );
        }

        return Ok(Response::message(format!(
            "Attached to {} ({} / session {}). Session exited with status {:?}.",
            name,
            provider.as_str(),
            session_id,
            status.code()
        )));
    }
}

// ── Worktree management ──────────────────────────────────────────────

/// Ensure the worktree for an attach target exists and is up to date.
///
/// For the lead session, this updates the worktree to the main repo's current
/// HEAD so the lead always works against the latest code.
///
/// For coworkers, this validates that the daemon-provided worktree path exists.
/// The daemon creates task-based worktrees via Effect::EnsureWorktree before
/// spawning; this function simply validates and returns the CWD.
///
/// Returns the (possibly updated) worktree path to use as the CWD.
pub(crate) fn ensure_attach_worktree(
    _name: &str,
    daemon_cwd: &str,
    is_lead: bool,
) -> Result<String, String> {
    // Resolve the main repo root from daemon_cwd (which may itself be a worktree).
    let repo_root = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(daemon_cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let git_dir = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if git_dir == ".git" {
                    Some(std::path::PathBuf::from(daemon_cwd))
                } else {
                    std::path::Path::new(&git_dir)
                        .parent()
                        .map(|p| p.to_path_buf())
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from(daemon_cwd));

    let manager = match midtown::worktree::WorktreeManager::new(repo_root.clone()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Warning: Could not init worktree manager: {}", e);
            return Ok(daemon_cwd.to_string());
        }
    };

    if is_lead {
        match manager.create_lead_worktree() {
            Ok(path) => return Ok(path.to_string_lossy().to_string()),
            Err(e) => {
                eprintln!("Warning: Failed to update lead worktree: {}", e);
            }
        }
    } else {
        let cwd_path = std::path::Path::new(daemon_cwd);
        if cwd_path.exists() {
            return Ok(daemon_cwd.to_string());
        }
        eprintln!(
            "Warning: Coworker worktree {} does not exist, falling back to repo root",
            daemon_cwd
        );
        return Ok(repo_root.to_string_lossy().to_string());
    }

    Ok(daemon_cwd.to_string())
}

// ── Attach resolution helpers ────────────────────────────────────────

fn resolve_attach_candidates(
    client: &DaemonClient,
    target: &str,
) -> Result<ResolvePayload, String> {
    let value = client.session_resolve(target)?;
    let resolved: ResolvePayload = serde_json::from_value(value)
        .map_err(|e| format!("Invalid candidates payload from daemon: {}", e))?;
    if resolved.candidates.is_empty() {
        return Err(format!("No attachable sessions found for '{}'", target));
    }
    Ok(resolved)
}

fn should_auto_create_session(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    (lower.contains("no attachable sessions")
        || lower.contains("matched no persisted attachable sessions")
        || lower.contains("no persisted session"))
        && !lower.contains("invalid")
}

fn create_attach_target(client: &DaemonClient, target: &str) -> Result<String, String> {
    let provider = provider_from_target(target);
    eprintln!(
        "No existing attachable session matched '{}'; creating a new {} coworker session...",
        target,
        provider.as_str()
    );

    let spawn_response = client.coworker_spawn(false, None, provider, None, None, None, None)?;
    let spawned_name = extract_spawned_name(&spawn_response)?;

    let new_target = format!("name/{}", spawned_name);
    for _ in 0..100 {
        if resolve_attach_candidates(client, &new_target).is_ok() {
            return Ok(new_target);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    Err(format!(
        "Spawned coworker '{}' but its session was not attachable yet. Try again in a few seconds.",
        spawned_name
    ))
}

fn extract_spawned_name(response: &Response) -> Result<String, String> {
    match response {
        Response::Coworkers { coworkers } => coworkers
            .first()
            .map(|c| c.name.to_lowercase())
            .ok_or_else(|| "Spawn response contained no coworkers".to_string()),
        Response::Json { value } => value
            .get("coworkers")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|cw| cw.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .ok_or_else(|| "Spawn response JSON did not include coworker name".to_string()),
        Response::Message { message } => message
            .split(':')
            .next_back()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .ok_or_else(|| format!("Could not parse spawned coworker name from '{}'", message)),
        _ => Err("Unexpected response type from coworker spawn".to_string()),
    }
}

fn provider_from_target(target: &str) -> midtown::auth::AuthProvider {
    let lower = target.to_ascii_lowercase();
    if lower == "codex" || lower.starts_with("codex/") || lower.starts_with("openai/") {
        midtown::auth::AuthProvider::Codex
    } else {
        midtown::auth::AuthProvider::Claude
    }
}

fn choose_attach_candidate(
    target: &str,
    resolved: &ResolvePayload,
) -> Result<AttachCandidate, String> {
    let candidates = &resolved.candidates;
    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }

    if is_platform_session_target(target) {
        let options = candidates
            .iter()
            .map(|c| format!("{} ({}/{})", c.name, c.platform, c.session_id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Multiple sessions match explicit target '{}': {}. Use name/<coworker> to disambiguate.",
            target, options
        ));
    }

    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        let options = candidates
            .iter()
            .map(|c| format!("{} ({}/{})", c.name, c.platform, c.session_id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Multiple sessions match '{}': {}. Re-run with name/<coworker>.",
            target, options
        ));
    }

    eprintln!(
        "Multiple sessions match '{}'. Select one (snapshot unix={} mono={}ms):",
        target,
        resolved
            .resolved_at_unix_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        resolved
            .resolved_at_monotonic_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
    );
    for (idx, candidate) in candidates.iter().enumerate() {
        let age = candidate
            .last_active_age_ms
            .map(format_age_ms)
            .unwrap_or_else(|| "n/a".to_string());
        let health = if candidate.attached {
            "attached"
        } else if candidate.running {
            "running"
        } else {
            "paused"
        };
        eprintln!(
            "  {}. {} [{} via {} / {}] {} ({}, last_active_age={}, at={})",
            idx + 1,
            candidate.name,
            candidate.platform,
            candidate.provider,
            candidate.session_id,
            candidate.cwd,
            health,
            age,
            candidate.last_active.as_deref().unwrap_or("unknown"),
        );
    }

    loop {
        eprint!("Choice [1-{}]: ", candidates.len());
        std::io::stdout()
            .flush()
            .map_err(|e| format!("Failed to flush prompt: {}", e))?;

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read selection: {}", e))?;
        let trimmed = input.trim();
        let Ok(index) = trimmed.parse::<usize>() else {
            eprintln!("Enter a number between 1 and {}.", candidates.len());
            continue;
        };
        if (1..=candidates.len()).contains(&index) {
            return Ok(candidates[index - 1].clone());
        }
        eprintln!("Enter a number between 1 and {}.", candidates.len());
    }
}

fn is_attach_race_error(err: &str) -> bool {
    err.contains("is not running")
        || err.contains("No session ID found")
        || err.contains("already attached")
}

fn format_age_ms(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{}ms", ms);
    }
    if ms < 60_000 {
        return format!("{:.1}s", (ms as f64) / 1_000.0);
    }
    format!("{:.1}m", (ms as f64) / 60_000.0)
}

fn is_platform_session_target(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.starts_with("claude/") || lower.starts_with("codex/")
}

// ── Target normalization ─────────────────────────────────────────────

fn normalize_attach_target(args: &AttachArgs) -> Result<String, String> {
    let first = args.target.trim();
    if first.is_empty() {
        return Err(usage_attach().to_string());
    }

    if let Some(second) = args.value.as_deref() {
        let value = second.trim();
        if value.is_empty() {
            return Err(usage_attach().to_string());
        }

        let kind = normalize_target_kind(first)?;
        return Ok(format!("{}/{}", kind, value));
    }

    normalize_single_target(first)
}

fn normalize_single_target(raw: &str) -> Result<String, String> {
    if let Some((kind, value)) = raw.split_once('/') {
        let value = value.trim();
        if value.is_empty() {
            return Err(usage_attach().to_string());
        }
        return Ok(format!("{}/{}", normalize_target_kind(kind)?, value));
    }

    if let Some((kind, value)) = raw.split_once(':') {
        let value = value.trim();
        if value.is_empty() {
            return Err(usage_attach().to_string());
        }
        return Ok(format!("{}:{}", normalize_target_kind(kind)?, value));
    }

    let lower = raw.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "claude" | "codex" | "anthropic" | "antropic" | "openai"
    ) {
        return normalize_target_kind(raw);
    }
    if matches!(lower.as_str(), "name" | "task" | "pr") {
        return Err(usage_attach().to_string());
    }

    // Bare token defaults to coworker name.
    Ok(format!("name/{}", lower))
}

fn normalize_target_kind(kind: &str) -> Result<String, String> {
    let lower = kind.trim().to_ascii_lowercase();
    let normalized = match lower.as_str() {
        "name" | "task" | "pr" | "claude" | "codex" => lower,
        // Provider aliases to reduce friction in manual input.
        "anthropic" | "antropic" => "claude".to_string(),
        "openai" => "codex".to_string(),
        "zai" | "z.ai" => {
            return Err(
                "Invalid platform selector 'zai'. Use claude/<session_id> for z.ai sessions."
                    .to_string(),
            );
        }
        _ => {
            return Err(format!(
                "Invalid attach selector '{}'. {}",
                kind,
                usage_attach()
            ));
        }
    };
    Ok(normalized)
}

fn usage_attach() -> &'static str {
    "Usage: midtown agent attach <target>\n\
     Examples:\n\
       midtown agent attach codex\n\
       midtown agent attach claude\n\
       midtown agent attach name/park\n\
       midtown agent attach task/42\n\
       midtown agent attach pr/123\n\
       midtown agent attach claude/abc-123\n\
       midtown agent attach codex/thread-1\n\
       midtown agent attach park"
}

// ── Launch spec builders ─────────────────────────────────────────────

pub(crate) fn build_attach_launch_spec(
    cwd: &str,
    name: &str,
    provider: midtown::auth::AuthProvider,
    session_id: &str,
    options: AttachLaunchOptions<'_>,
) -> Result<AttachLaunchSpec, String> {
    let repo_name = midtown::paths::detect_repo_name_from_dir(Path::new(cwd))
        .ok_or_else(|| "Not in a git repository".to_string())?;

    let profile_dir = options
        .profile
        .map(|name| midtown::auth::profile_dir_for(provider, name))
        .unwrap_or_else(|| {
            midtown::auth::active_profile_dir_for_project_with_provider(&repo_name, provider)
        });

    // Determine agent type from coworker_type (provided by daemon's SessionRecord)
    let agent_type = match options.coworker_type {
        Some("lead") => "midtown-project-lead",
        Some("reviewer") => "midtown-code-reviewer",
        Some("channel-lead") => "midtown-channel-lead",
        _ => "midtown-code-author",
    };
    let channel_name_for_lead = options.channel.unwrap_or(name).to_string();

    // Build common env vars using the shared function
    let env_map = midtown::launch::build_agent_env_vars(
        name,
        &None, // channel not set for attach sessions
        provider,
        &profile_dir,
        &repo_name,
    );

    let sandbox_config = midtown::config::get_project_sandbox_config(&repo_name);
    let writable = midtown::sandbox::writable_dirs(
        Path::new(cwd),
        &[],
        &sandbox_config.allowed_paths,
        &repo_name,
    );

    let mut cmd_parts: Vec<String> = Vec::new();
    if cfg!(target_os = "macos")
        && let Ok((_profile_path, prefix)) = midtown::sandbox::sandbox_exec_prefix(&writable)
    {
        cmd_parts.push("sandbox-exec".to_string());
        cmd_parts.extend(prefix);
    }

    let mut launch_config =
        midtown::launch::LaunchConfig::new(name.to_string(), agent_type, &repo_name, None, None)
            .with_session_mode(midtown::launch::SessionMode::ResumeSession(
                session_id.to_string(),
            ))
            .with_auth_provider(provider)
            .with_auth_profile_dir(Some(profile_dir.clone()));
    // For channel leads, override model from channel config
    if agent_type == "midtown-channel-lead" {
        launch_config.model = midtown::config::get_channel_leads_config(&repo_name)
            .model_for_channel(&channel_name_for_lead);
        launch_config.channel = Some(channel_name_for_lead.clone());
    }

    let system_prompt = match agent_type {
        "midtown-project-lead" => midtown::agents::main_lead_system_prompt(&repo_name),
        "midtown-code-reviewer" => {
            midtown::agents::reviewer_system_prompt(name, &repo_name, &repo_name, provider, None)
        }
        "midtown-code-author" => midtown::agents::coworker_system_prompt(name, &repo_name, None),
        "midtown-channel-lead" => midtown::agents::channel_lead_system_prompt(
            &channel_name_for_lead,
            "",
            &repo_name,
            None,
            false,
        ),
        _ => midtown::agents::coworker_system_prompt(name, &repo_name, None),
    };

    // Build provider-specific headed CLI args.
    match provider {
        midtown::auth::AuthProvider::Claude | midtown::auth::AuthProvider::Zai => {
            // Write system prompt to temp file
            let prompt_file = std::env::temp_dir().join(format!(
                "midtown-attach-{}-{}.txt",
                name,
                std::process::id()
            ));
            std::fs::write(&prompt_file, &system_prompt)
                .map_err(|e| format!("Failed to write system prompt to temp file: {}", e))?;

            // Write role-appropriate settings file
            let settings_file = if agent_type == "midtown-project-lead" {
                midtown::settings::write_lead_settings_file()
                    .map_err(|e| format!("Failed to write lead settings file: {}", e))?
            } else {
                midtown::settings::write_coworker_settings_file()
                    .map_err(|e| format!("Failed to write coworker settings file: {}", e))?
            };

            let (cli_args, _) = launch_config.to_cli_args(&settings_file, &prompt_file, None);
            cmd_parts.extend(cli_args);
        }
        midtown::auth::AuthProvider::Codex => {
            let (cli_args, _) =
                midtown::platform::build_codex_headed_args(&launch_config, &system_prompt, None);
            cmd_parts.extend(cli_args);
        }
    }

    let (program, args) = cmd_parts
        .split_first()
        .ok_or_else(|| "Attach command is empty".to_string())?;

    Ok(AttachLaunchSpec {
        program: program.clone(),
        args: args.to_vec(),
        env: env_map,
    })
}

/// Build the shell command for split-pane attach flows (`midtown view` paths).
///
/// When `include_detach` is `true`, the shell command ends with
/// `midtown agent detach <name>`, which resumes the headless session when the
/// interactive pane closes.
pub(crate) fn build_attach_shell_command(
    cwd: &str,
    name: &str,
    provider: midtown::auth::AuthProvider,
    session_id: &str,
    options: AttachShellOptions<'_>,
) -> Result<String, String> {
    let launch_spec = build_attach_launch_spec(cwd, name, provider, session_id, options.launch)?;

    let env_parts: Vec<String> = launch_spec
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, shell_quote(v)))
        .collect();

    let provider_cmd = std::iter::once(&launch_spec.program)
        .chain(launch_spec.args.iter())
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");

    let attach_cmd = format!("sh -lc {}", shell_quote(&provider_cmd));

    if options.include_detach {
        let bin_command = midtown::config::get_bin_command();
        let detach_cmd = format!("{} agent detach {}", bin_command, shell_quote(name));
        Ok(format!(
            "export {}; {}; _midtown_rc=$?; {} >/dev/null 2>&1 || true; exit $_midtown_rc",
            env_parts.join(" "),
            attach_cmd,
            detach_cmd
        ))
    } else {
        Ok(format!("export {}; {}", env_parts.join(" "), attach_cmd,))
    }
}

// ── Provider parsing ─────────────────────────────────────────────────

fn parse_provider(raw: &str) -> midtown::auth::AuthProvider {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" | "antropic" => midtown::auth::AuthProvider::Claude,
        "codex" | "openai" => midtown::auth::AuthProvider::Codex,
        "zai" | "z.ai" => midtown::auth::AuthProvider::Zai,
        _ => midtown::auth::AuthProvider::Claude,
    }
}

/// Shell-quote a string using the `shell-escape` crate.
fn shell_quote(input: &str) -> String {
    shell_escape::escape(input.into()).into_owned()
}

// ── Show handler ─────────────────────────────────────────────────────

/// Handle `midtown agent show` with rich ANSI rendering and optional `--watch` mode.
///
/// Tries `session_view_raw` first (handles all target formats: name, task/N, pr/N, etc.).
/// If that fails, falls back to `coworker_view` for simple name lookups.
fn handle_show(target: &str, watch: bool, client: &DaemonClient) -> Result<Response, String> {
    // Try session_view_raw first — handles all target formats
    let raw = match client.session_view_raw(target) {
        Ok(raw) => raw,
        Err(_session_err) => {
            // Fall back to coworker_view for simple name lookups
            let response = client.coworker_view(target)?;
            let raw = match response {
                Response::Message { message } => message,
                other => return Ok(other),
            };
            let rendered = session_render::render_ansi(&raw);
            return Ok(Response::message(rendered.trim_end().to_string()));
        }
    };

    let output = raw
        .get("output")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let log_path = raw
        .get("log_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let snapshot_log_offset = raw.get("log_offset").and_then(|v| v.as_u64()).unwrap_or(0);

    // Render the initial snapshot
    let rendered = session_render::render_ansi(&output);

    if !watch {
        return Ok(Response::message(rendered.trim_end().to_string()));
    }

    // Watch mode: print initial snapshot then tail for new events
    print!("{}", rendered);
    std::io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;

    let log_path = log_path.ok_or_else(|| {
        "Daemon did not return a log path; --watch is not available for this session".to_string()
    })?;

    let path = std::path::PathBuf::from(&log_path);
    if !path.exists() {
        return Err(format!("Log file not found: {log_path}"));
    }

    let mut file_offset = snapshot_log_offset;

    eprintln!(
        "\x1b[2m── watching {} (Ctrl-C to stop) ──\x1b[0m",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("log")
    );

    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));

        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let current_size = metadata.len();

        if current_size <= file_offset {
            continue;
        }

        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut reader = std::io::BufReader::new(file);

        use std::io::Seek;
        if reader.seek(std::io::SeekFrom::Start(file_offset)).is_err() {
            continue;
        }

        let mut new_lines: Vec<String> = Vec::new();
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if !line.is_empty() {
                        new_lines.push(line.to_string());
                    }
                }
                Err(_) => break,
            }
        }

        file_offset = reader.stream_position().unwrap_or(current_size);

        for line in &new_lines {
            if let Some(rendered) = session_render::render_event_line(line) {
                print!("{}", rendered);
                let _ = std::io::stdout().flush();
            }
        }
    }
}

// ── Upload image ─────────────────────────────────────────────────────

/// Upload a local image file to GitHub and return `![alt](url)` markdown.
pub fn handle_upload_image(path: &str, alt: &str) -> Result<Response, String> {
    let image_path = std::path::Path::new(path);
    if !image_path.exists() {
        return Err(format!("File not found: {}", path));
    }

    let ext = image_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");

    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => {}
        _ => {
            return Err(format!(
                "Unsupported image format: .{}. Supported: png, jpg, jpeg, gif, webp",
                ext
            ));
        }
    }

    let github_url = upload_to_github(image_path, ext)?;
    Ok(Response::message(format!("![{}]({})", alt, github_url)))
}

fn upload_to_github(image_path: &std::path::Path, ext: &str) -> Result<String, String> {
    let token = std::env::var("GH_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .or_else(|_| {
            std::process::Command::new("gh")
                .args(["auth", "token"])
                .output()
                .map_err(|_| std::env::VarError::NotPresent)
                .and_then(|output| {
                    if output.status.success() {
                        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if token.is_empty() {
                            Err(std::env::VarError::NotPresent)
                        } else {
                            Ok(token)
                        }
                    } else {
                        Err(std::env::VarError::NotPresent)
                    }
                })
        })
        .map_err(|_| "No GitHub token found. Set GH_TOKEN or run `gh auth login`.".to_string())?;

    let repo_full_name = get_github_repo_name().ok_or(
        "Could not determine GitHub repository. Run from a git repo with a GitHub remote.",
    )?;

    let image_data =
        std::fs::read(image_path).map_err(|e| format!("Failed to read screenshot file: {}", e))?;

    let content_type = match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => unreachable!("extension validated in handle_upload_image"),
    };

    let filename = image_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("screenshot.png")
        .to_string();

    eprintln!("Uploading screenshot to GitHub...");

    let file_part = reqwest::blocking::multipart::Part::bytes(image_data)
        .file_name(filename)
        .mime_str(content_type)
        .map_err(|e| format!("Failed to build multipart form: {}", e))?;

    let form = reqwest::blocking::multipart::Form::new().part("file", file_part);

    let upload_url = format!(
        "https://uploads.github.com/repos/{}/issues/import/images",
        repo_full_name
    );

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(&upload_url)
        .header("Authorization", format!("token {}", token))
        .header("Accept", "application/json")
        .multipart(form)
        .send()
        .map_err(|e| format!("Failed to upload to GitHub: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("GitHub image upload failed ({}): {}", status, body));
    }

    let body: serde_json::Value = response
        .json()
        .map_err(|e| format!("Failed to parse GitHub upload response: {}", e))?;

    if let Some(url) = body.get("url").and_then(|v| v.as_str()) {
        eprintln!("Screenshot uploaded to GitHub: {}", url);
        return Ok(url.to_string());
    }

    Err(format!(
        "GitHub upload succeeded but response missing 'url' field: {}",
        body
    ))
}

fn get_github_repo_name() -> Option<String> {
    std::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[path = "agent_tests.rs"]
#[cfg(test)]
mod tests;
