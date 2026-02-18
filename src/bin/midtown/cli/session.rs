//! CLI subcommands for `midtown session` — attach/detach headless coworker sessions.
//!
//! `midtown session attach` pauses a headless coworker and opens an interactive
//! terminal pane/session to resume that exact provider session.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use clap::{Args, Subcommand};

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommand {
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
    /// List attachable headless sessions
    List,
    /// View a session's current output
    View {
        /// Session target (coworker name, task/<id>, pr/<number>, claude, etc.)
        target: String,
    },
    /// Clear a session: stop it and restart fresh with the same initial prompt.
    Clear {
        /// Session target (coworker name, task/<id>, pr/<number>, etc.)
        target: String,
    },
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneHost {
    Zellij,
    Tmux,
    Ghostty,
    ITerm,
    Unknown,
}

impl PaneHost {
    fn detect() -> Self {
        detect_pane_host_from(|key| std::env::var(key).ok())
    }
}

struct PaneLauncher {
    host: PaneHost,
}

impl PaneLauncher {
    fn detect() -> Self {
        Self {
            host: PaneHost::detect(),
        }
    }

    fn launch(&self, cwd: &str, shell_command: &str) -> Result<String, String> {
        match self.host {
            PaneHost::Tmux => self.launch_tmux(cwd, shell_command),
            PaneHost::Zellij => self.launch_zellij(cwd, shell_command),
            PaneHost::Ghostty => self.launch_ghostty(cwd, shell_command),
            PaneHost::ITerm => self.launch_iterm(cwd, shell_command),
            PaneHost::Unknown => {
                self.launch_in_current_shell(cwd, shell_command)?;
                Ok("current terminal".to_string())
            }
        }
    }

    fn launch_tmux(&self, cwd: &str, shell_command: &str) -> Result<String, String> {
        let status = Command::new("tmux")
            .args(["split-window", "-h", "-c", cwd, "sh", "-lc", shell_command])
            .status()
            .map_err(|e| format!("Failed to run tmux split-window: {}", e))?;
        if !status.success() {
            return Err("tmux split-window failed".to_string());
        }
        Ok("tmux split pane".to_string())
    }

    fn launch_zellij(&self, cwd: &str, shell_command: &str) -> Result<String, String> {
        let status = Command::new("zellij")
            .args([
                "action",
                "new-pane",
                "-d",
                "right",
                "--cwd",
                cwd,
                "--",
                "sh",
                "-lc",
                shell_command,
            ])
            .status()
            .map_err(|e| format!("Failed to run zellij action new-pane: {}", e))?;
        if !status.success() {
            return Err("zellij action new-pane failed".to_string());
        }
        Ok("zellij pane".to_string())
    }

    fn launch_ghostty(&self, cwd: &str, shell_command: &str) -> Result<String, String> {
        // Ghostty split creation is configured by user keybinds, so discover
        // the active keybinding first and trigger it.
        let mut split_ok = false;
        if let Ok(output) = Command::new("ghostty").arg("+list-keybinds").output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(binding) = parse_ghostty_keybind_for_action(&stdout, "new_split:right")
                .or_else(|| parse_ghostty_keybind_for_action(&stdout, "new_split"))
            {
                split_ok = trigger_ghostty_keybinding(&binding).unwrap_or(false);
            }
        }

        // Best-effort fallback when keybinding dispatch is unavailable.
        if !split_ok {
            split_ok = Command::new("ghostty")
                .args(["+action", "new_split:right"])
                .status()
                .is_ok_and(|s| s.success());
        }

        // Ghostty's CLI split action does not reliably support command injection
        // across builds, so execute in the current terminal as a deterministic fallback.
        self.launch_in_current_shell(cwd, shell_command)?;

        if split_ok {
            Ok("ghostty (split + current pane fallback)".to_string())
        } else {
            Ok("ghostty (current pane fallback)".to_string())
        }
    }

    fn launch_iterm(&self, cwd: &str, shell_command: &str) -> Result<String, String> {
        if cfg!(target_os = "macos") && self.launch_iterm_split(cwd, shell_command)? {
            return Ok("iTerm split pane".to_string());
        }

        self.launch_in_current_shell(cwd, shell_command)?;
        Ok("iTerm (current pane fallback)".to_string())
    }

    fn launch_iterm_split(&self, cwd: &str, shell_command: &str) -> Result<bool, String> {
        let typed_cmd = format!("cd {} && {}", shell_quote(cwd), shell_command);
        let script = format!(
            r#"tell application \"iTerm2\"
    if (count of windows) = 0 then
        create window with default profile
    end if
    tell current window
        tell current session
            set newSession to (split horizontally with default profile)
            tell newSession
                write text \"{}\"
            end tell
        end tell
    end tell
end tell"#,
            escape_applescript_string(&typed_cmd)
        );

        let status = Command::new("osascript")
            .args(["-e", &script])
            .status()
            .map_err(|e| format!("Failed to run osascript for iTerm split: {}", e))?;

        Ok(status.success())
    }

    fn launch_in_current_shell(&self, cwd: &str, shell_command: &str) -> Result<(), String> {
        Command::new("sh")
            .args(["-lc", shell_command])
            .current_dir(cwd)
            .status()
            .map_err(|e| format!("Failed to run attach command in current shell: {}", e))?;
        Ok(())
    }
}

pub fn handle(cmd: &SessionCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        SessionCommand::Attach { target } => handle_attach(target, client),
        SessionCommand::Detach { name } => client.session_detach(name),
        SessionCommand::List => client.session_list(),
        SessionCommand::View { target } => client.session_view(target),
        SessionCommand::Clear { target } => client.session_clear(target),
    }
}

/// Handle attach: resolve target -> pause headless -> open interactive pane -> auto-detach on exit.
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
        let coworker_type = info
            .get("coworker_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Err(e) = midtown::platform_launch::run_platform_prelaunch_hook(provider) {
            eprintln!(
                "Warning: Platform pre-launch hook failed (continuing): {}",
                e
            );
        }

        // Ensure worktree is set up before launching.
        // For the lead, this updates the worktree to the current HEAD.
        // For coworkers, this ensures the worktree directory exists.
        let cwd = ensure_attach_worktree(name, cwd)?;

        let shell_command = build_attach_shell_command(
            &cwd,
            name,
            provider,
            session_id,
            coworker_type.as_deref(),
            true, // include_detach: standalone attach resumes headless when pane closes
        )?;
        let launcher = PaneLauncher::detect();

        // Step 3: Launch interactive session in a pane for the current terminal host.
        let where_opened = launcher.launch(&cwd, &shell_command).map_err(|e| {
            // If pane launch fails, tell daemon to resume headless.
            match client.session_detach(name) {
                Ok(_) => eprintln!(
                    "Attach launch failed; headless session resumed for {}.",
                    name
                ),
                Err(detach_err) => eprintln!(
                    "ERROR: Attach launch failed AND detach RPC failed for {}.\n\
                     Launch error: {}\n\
                     Detach error: {}\n\
                     Manual recovery: run `midtown session detach {}`",
                    name, e, detach_err, name
                ),
            }
            format!("Failed to launch interactive session: {}", e)
        })?;

        return Ok(Response::message(format!(
            "Attached to {} ({} / session {}). Opened in {}.",
            name,
            provider.as_str(),
            session_id,
            where_opened
        )));
    }
}

/// Ensure the worktree for an attach target exists and is up to date.
///
/// For the lead session, this updates the worktree to the main repo's current
/// HEAD so the lead always works against the latest code.
///
/// For coworkers, this ensures the worktree directory exists (creating it if
/// needed via the daemon's existing worktree manager).
///
/// Returns the (possibly updated) worktree path to use as the CWD.
pub(crate) fn ensure_attach_worktree(name: &str, daemon_cwd: &str) -> Result<String, String> {
    // Resolve the main repo root from daemon_cwd (which may itself be a worktree).
    // git-common-dir gives us the main repo's .git dir; its parent is the repo root.
    let repo_root = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(daemon_cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let git_dir = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if git_dir == ".git" {
                    // Already in the repo root
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

    let manager = match midtown::worktree::WorktreeManager::new(repo_root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Warning: Could not init worktree manager: {}", e);
            return Ok(daemon_cwd.to_string());
        }
    };

    if name == "lead" {
        match manager.create_lead_worktree() {
            Ok(path) => return Ok(path.to_string_lossy().to_string()),
            Err(e) => {
                eprintln!("Warning: Failed to update lead worktree: {}", e);
            }
        }
    } else {
        // For coworkers, ensure their worktree exists
        let wt_path = manager.worktree_path(name);
        if !wt_path.exists() {
            #[allow(deprecated)] // Legacy worktree layout for CLI session
            match manager.create(name) {
                Ok(path) => return Ok(path.to_string_lossy().to_string()),
                Err(e) => {
                    eprintln!("Warning: Failed to create worktree for {}: {}", name, e);
                }
            }
        }
    }

    Ok(daemon_cwd.to_string())
}

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

    let spawn_response = client.coworker_spawn(false, None, provider)?;
    let spawned_name = extract_spawned_name(&spawn_response)?;

    // Wait for the new headless session to persist a resumable session ID.
    // This is async in the daemon (stream init event), so poll briefly.
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
    "Usage: midtown session attach <target>\n\
     Examples:\n\
       midtown session attach codex\n\
       midtown session attach claude\n\
       midtown session attach name/park\n\
       midtown session attach task/42\n\
       midtown session attach pr/123\n\
       midtown session attach claude/abc-123\n\
       midtown session attach codex/thread-1\n\
       midtown session attach park"
}

/// Build the shell command to run in a pane for an interactive attach session.
///
/// When `include_detach` is `true`, the shell command ends with
/// `midtown session detach <name>`, which resumes the headless session when the
/// interactive pane closes.  Set this to `true` for `midtown session attach`
/// (standalone interactive use) and `false` for `midtown view`, which calls
/// `session_detach` explicitly when the chat UI exits — avoiding a race where
/// the pane's claude process exits before the chat UI and triggers an early
/// headless respawn that creates a dual-lead situation.
pub(crate) fn build_attach_shell_command(
    cwd: &str,
    name: &str,
    provider: midtown::auth::AuthProvider,
    session_id: &str,
    coworker_type: Option<&str>,
    include_detach: bool,
) -> Result<String, String> {
    let repo_name = midtown::paths::detect_repo_name_from_dir(Path::new(cwd))
        .ok_or_else(|| "Not in a git repository".to_string())?;

    let profile_dir =
        midtown::auth::active_profile_dir_for_project_with_provider(&repo_name, provider);

    // Determine role from coworker_type (provided by daemon's HeadlessSessionInfo)
    let role = if name == "lead" {
        midtown::launch::CoworkerRole::Lead
    } else if coworker_type == Some("reviewer") {
        midtown::launch::CoworkerRole::Reviewer
    } else {
        midtown::launch::CoworkerRole::Coworker
    };

    // Get team name for this repo
    let team_name = Some(midtown::mailbox::team_name_for_repo(&repo_name));

    // Build common env vars using the shared function
    let env_map = midtown::launch::build_agent_env_vars(
        name,
        &role,
        &team_name,
        &None, // channel not set for attach sessions
        provider,
        &profile_dir,
    );

    // Convert env map to shell-quoted env var assignments (key=value format, with shell_quote on values)
    let env_parts: Vec<String> = env_map
        .iter()
        .map(|(k, v)| format!("{}={}", k, shell_quote(v)))
        .collect();

    let sandbox_config = midtown::config::get_project_sandbox_config(&repo_name);
    let writable =
        midtown::sandbox::writable_dirs(Path::new(cwd), &[], &sandbox_config.allowed_paths);

    let mut cmd_parts: Vec<String> = Vec::new();
    if cfg!(target_os = "macos")
        && let Ok((_profile_path, prefix)) = midtown::sandbox::sandbox_exec_prefix(&writable)
    {
        cmd_parts.push("sandbox-exec".to_string());
        cmd_parts.extend(prefix);
    }

    // Build CLI args using LaunchConfig — single source of truth for all launch paths
    match provider {
        midtown::auth::AuthProvider::Claude | midtown::auth::AuthProvider::Zai => {
            let launch_config = midtown::launch::LaunchConfig {
                name: name.to_string(),
                session_mode: midtown::launch::SessionMode::Resume,
                role: role.clone(),
                initial_prompt: None,
                additional_dirs: vec![],
                pr_number: None,
                team_name: team_name.clone(),
                working_dir: None,
                model: match role {
                    midtown::launch::CoworkerRole::Lead
                    | midtown::launch::CoworkerRole::Reviewer => "opus".to_string(),
                    midtown::launch::CoworkerRole::Coworker
                    | midtown::launch::CoworkerRole::ChannelLead { .. } => "sonnet".to_string(),
                },
                channel: None,
                auth_profile_dir: Some(profile_dir.clone()),
                auth_provider: provider,
                persisted_initial_prompt: None,
            };

            // Write system prompt to temp file
            let system_prompt = match &launch_config.role {
                midtown::launch::CoworkerRole::Lead => midtown::agents::lead_system_prompt(),
                midtown::launch::CoworkerRole::Reviewer => {
                    midtown::agents::reviewer_system_prompt(name)
                }
                midtown::launch::CoworkerRole::Coworker => {
                    midtown::agents::coworker_system_prompt(name)
                }
                midtown::launch::CoworkerRole::ChannelLead {
                    channel_name,
                    domain_context,
                } => midtown::agents::channel_lead_system_prompt(channel_name, domain_context),
            };
            let prompt_file = std::env::temp_dir().join(format!(
                "midtown-attach-{}-{}.txt",
                name,
                std::process::id()
            ));
            std::fs::write(&prompt_file, system_prompt)
                .map_err(|e| format!("Failed to write system prompt to temp file: {}", e))?;

            // Write role-appropriate settings file
            let settings_file = if name == "lead" {
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
            cmd_parts.extend(vec![
                "codex".to_string(),
                "--resume".to_string(),
                session_id.to_string(),
            ]);
        }
    }

    // Build the provider command as a shell command string.
    // Each part is individually shell-escaped, then joined with spaces.
    // The resulting string will be passed to `sh -lc` as a single argument.
    let provider_cmd = cmd_parts
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");

    let bin_command = midtown::config::get_bin_command();
    let wrapped_attach_cmd = format!(
        "{} headed-wrapper run-agent --session {} --provider {} --cwd {} -- sh -lc {}",
        shell_quote(&bin_command),
        shell_quote(name),
        provider.as_str(),
        shell_quote(cwd),
        shell_quote(&provider_cmd),
    );

    if include_detach {
        let detach_cmd = format!("{} session detach {}", bin_command, shell_quote(name));
        Ok(format!(
            "export {}; {}; _midtown_rc=$?; {} >/dev/null 2>&1 || true; exit $_midtown_rc",
            env_parts.join(" "),
            wrapped_attach_cmd,
            detach_cmd
        ))
    } else {
        Ok(format!(
            "export {}; {}",
            env_parts.join(" "),
            wrapped_attach_cmd,
        ))
    }
}

fn parse_provider(raw: &str) -> midtown::auth::AuthProvider {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" | "antropic" => midtown::auth::AuthProvider::Claude,
        "codex" | "openai" => midtown::auth::AuthProvider::Codex,
        "zai" | "z.ai" => midtown::auth::AuthProvider::Zai,
        _ => midtown::auth::AuthProvider::Claude,
    }
}

/// Shell-quote a string using the `shell-escape` crate.
/// This properly handles all special characters for Unix shells.
fn shell_quote(input: &str) -> String {
    shell_escape::escape(input.into()).into_owned()
}

fn escape_applescript_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse_ghostty_keybind_for_action(list_keybinds_output: &str, action: &str) -> Option<String> {
    for line in list_keybinds_output.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("keybind = ") else {
            continue;
        };
        let Some((binding, bound_action)) = rest.split_once('=') else {
            continue;
        };
        if bound_action.trim() == action {
            return Some(binding.trim().to_string());
        }
    }
    None
}

fn trigger_ghostty_keybinding(binding: &str) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let mut modifiers: Vec<&str> = Vec::new();
        let mut key_token: Option<String> = None;

        for token in binding
            .split('+')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            match token.to_ascii_lowercase().as_str() {
                "super" | "cmd" | "command" => modifiers.push("command down"),
                "shift" => modifiers.push("shift down"),
                "alt" | "option" => modifiers.push("option down"),
                "ctrl" | "control" => modifiers.push("control down"),
                other => {
                    if key_token.is_some() {
                        return Ok(false);
                    }
                    key_token = Some(other.to_string());
                }
            }
        }

        let Some(key_token) = key_token else {
            return Ok(false);
        };

        let using_clause = if modifiers.is_empty() {
            String::new()
        } else {
            format!(" using {{{}}}", modifiers.join(", "))
        };

        let script = if key_token == "enter" {
            format!(
                "tell application \"System Events\" to key code 36{}",
                using_clause
            )
        } else {
            let key_text = if let Some(digit) = key_token.strip_prefix("digit_") {
                if digit.len() == 1 {
                    digit.to_string()
                } else {
                    return Ok(false);
                }
            } else if key_token.chars().count() == 1 {
                key_token
            } else {
                return Ok(false);
            };

            format!(
                "tell application \"System Events\" to keystroke \"{}\"{}",
                escape_applescript_string(&key_text),
                using_clause
            )
        };

        let status = Command::new("osascript")
            .args(["-e", &script])
            .status()
            .map_err(|e| format!("Failed to trigger Ghostty keybinding via osascript: {}", e))?;
        Ok(status.success())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = binding;
        Ok(false)
    }
}

fn detect_pane_host_from(get_env: impl Fn(&str) -> Option<String>) -> PaneHost {
    if get_env("ZELLIJ").is_some() {
        return PaneHost::Zellij;
    }
    if get_env("TMUX").is_some() {
        return PaneHost::Tmux;
    }

    let term_program = get_env("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term_program == "ghostty" {
        return PaneHost::Ghostty;
    }
    if term_program == "iterm.app" {
        return PaneHost::ITerm;
    }

    let lc_terminal = get_env("LC_TERMINAL")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if lc_terminal == "iterm2" {
        return PaneHost::ITerm;
    }

    PaneHost::Unknown
}

#[path = "session_tests.rs"]
#[cfg(test)]
mod tests;
