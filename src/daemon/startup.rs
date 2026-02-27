//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker tracking across daemon restarts.
//! When the daemon starts:
//! - Recovers headless coworker sessions from persisted state and resumes them with --resume
//! - Cleans up zombie processes from previous daemon runs (orphaned PPID=1 or children of stale daemons)
//!
//! Workflow state is recovered when coworkers report via RPC.

use std::collections::{HashMap, HashSet};

use tokio::sync::RwLock;
use tracing::{info, warn};

/// Kill a stale daemon process that lost its PID lock but didn't exit.
///
/// Called after successfully acquiring the PID lock. Since we hold the exclusive
/// lock, the old process is definitively stale. This function:
/// 1. Verifies the process command contains "midtown" and references the same project workdir
///    (avoids killing PID-reused processes or daemons for other projects)
/// 2. Sends SIGTERM for graceful shutdown
/// 3. Polls up to 3 seconds for the process to exit
/// 4. Sends SIGKILL as a last resort
///
/// `project_workdir` is used to scope the verification to this project — a midtown daemon
/// for a different project should not be killed even if it happens to reuse the stale PID.
pub fn kill_stale_daemon(pid: u32, project_workdir: &std::path::Path) {
    if pid == std::process::id() {
        return;
    }

    if !verify_midtown_process(pid, project_workdir) {
        info!(
            "Stale PID {} is not this project's midtown process (PID reused or already exited), skipping",
            pid
        );
        return;
    }

    info!(
        "Killing stale daemon process {} (lost PID lock but still running)",
        pid
    );

    // SIGTERM first for graceful shutdown
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .output();

    // Poll up to 3 seconds for the process to die
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !process_exists(pid) {
            info!("Stale daemon {} exited after SIGTERM", pid);
            return;
        }
    }

    // Still alive — SIGKILL
    warn!(
        "Stale daemon {} didn't exit after SIGTERM, sending SIGKILL",
        pid
    );
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

/// Check if a process is a stale midtown daemon (not the current one).
///
/// Returns true if:
/// - pid != current_daemon_pid
/// - The process command line contains "midtown"
pub fn is_stale_midtown_daemon(pid: u32, current_daemon_pid: u32) -> bool {
    if pid == current_daemon_pid {
        return false;
    }

    // Use a minimal verify — any midtown process that isn't us and isn't the
    // current daemon PID is a stale daemon parent. We don't need project-scoping
    // here because we're checking PPID of claude children, not killing midtown directly.
    let output = match std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    String::from_utf8_lossy(&output.stdout).contains("midtown")
}

/// Check if a PID belongs to this project's midtown daemon process.
///
/// Returns true if the process exists, its command line contains "midtown",
/// and its command line references the given `project_workdir`. This prevents
/// accidentally killing a midtown daemon for a different project if the stale
/// PID is reused by another midtown process.
pub fn verify_midtown_process(pid: u32, project_workdir: &std::path::Path) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let cmdline = String::from_utf8_lossy(&output.stdout);
    if !cmdline.contains("midtown") {
        return false;
    }

    // Verify the process is associated with the same project by checking
    // that its cmdline references the project workdir. The daemon is launched
    // with `--workdir <repo>`, so the workdir path appears in its args.
    let workdir_str = project_workdir.to_string_lossy();
    cmdline.contains(workdir_str.as_ref())
}

/// Check if a process exists (is still running).
fn process_exists(pid: u32) -> bool {
    // kill -0 checks if process exists without sending a signal
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if the daemon is running inside a sandbox context.
///
/// Returns `Some(warning_message)` if sandboxing is unavailable (already nested),
/// or `None` if sandboxing is available.
///
/// This prevents the crash loop from 2026-02-13 where the daemon was started
/// from within a sandboxed session, causing all coworker spawns
/// to fail with "Already inside a sandbox — cannot nest sandbox-exec".
#[cfg(target_os = "macos")]
pub fn check_sandbox_context() -> Option<String> {
    if !crate::sandbox::can_sandbox() {
        Some(
            "WARNING: Daemon is already inside a sandbox — cannot nest sandbox-exec. \
             Coworker sandboxing will be disabled. This typically happens when the daemon \
             is started from within a sandboxed session. To fix: stop the daemon and \
             restart from an unsandboxed shell."
                .to_string(),
        )
    } else {
        None
    }
}

/// Non-macOS platforms don't use sandbox-exec, so this check is a no-op.
#[cfg(not(target_os = "macos"))]
pub fn check_sandbox_context() -> Option<String> {
    None
}

/// Check Claude CLI authentication status before spawning any sessions.
///
/// Runs `claude auth status --output json` with `CLAUDE_CONFIG_DIR` pointing to the
/// project's active auth profile directory. Logs the result so operators get immediate
/// feedback on auth state at daemon startup rather than discovering failures reactively.
///
/// The daemon continues regardless — auth may be fixed via `midtown auth login` while
/// the daemon is running.
pub fn check_claude_auth_status(repo_name: &str) {
    let profile_dir = crate::auth::active_profile_dir_for_project_with_provider(
        repo_name,
        crate::auth::AuthProvider::Claude,
    );

    let output = match std::process::Command::new("claude")
        .args(["auth", "status", "--output", "json"])
        .env("CLAUDE_CONFIG_DIR", &profile_dir)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            warn!(
                "Failed to run `claude auth status`: {}. Is Claude CLI installed?",
                e
            );
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output: {"loggedIn": true/false, "email": "..."}
    let json: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to parse `claude auth status` output: {}. Raw: {}",
                e,
                stdout.trim()
            );
            return;
        }
    };

    let logged_in = json
        .get("loggedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let email = json.get("email").and_then(|v| v.as_str()).map(String::from);

    if logged_in {
        if let Some(ref email) = email {
            info!("Claude auth verified: {}", email);
        } else {
            info!("Claude auth verified (no email in response)");
        }
    } else {
        warn!(
            "Claude auth not valid. Sessions will fail until auth is fixed. \
             Run `midtown auth login <email>` to authenticate."
        );
    }
}

/// Refresh the GH_TOKEN environment variable by re-running `gh auth token`.
///
/// Called periodically from the daemon event loop to pick up token changes.
/// If the user runs `gh auth login` or `gh auth refresh` externally, the stored
/// token in the gh config changes — this function detects that and updates the
/// daemon's process environment so newly spawned child processes inherit the
/// fresh token.
///
/// Returns `true` if the token was updated, `false` if unchanged or on error.
pub fn refresh_gh_token(github_user: &str) -> bool {
    let output = match std::process::Command::new("gh")
        .args(["auth", "token", "--user", github_user])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "gh auth token --user {} failed during refresh: {}",
                github_user,
                stderr.trim()
            );
            return false;
        }
        Err(e) => {
            warn!("Failed to run `gh auth token` for refresh: {}", e);
            return false;
        }
    };

    let new_token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if new_token.is_empty() {
        warn!(
            "gh auth token --user {} returned empty token during refresh",
            github_user
        );
        return false;
    }

    let current_token = std::env::var("GH_TOKEN").unwrap_or_default();
    if new_token == current_token {
        return false;
    }

    // Token changed — update the process env var.
    // SAFETY: This runs on a blocking task spawned from the main event loop.
    // Tokio's spawn_blocking runs on a thread pool, but std::env::set_var is
    // only unsafe because concurrent reads could race. In practice, child
    // process spawns (which read env) are serialized through the effect executor.
    unsafe {
        std::env::set_var("GH_TOKEN", &new_token);
    }
    info!(
        "GH_TOKEN refreshed for user: {} (token length: {} → {})",
        github_user,
        current_token.len(),
        new_token.len()
    );
    true
}

use crate::coworker::CoworkerManager;
use crate::daemon::effects::Effect;
use crate::daemon::state::{DaemonPersistentState, SessionRecord};
use crate::launch::LaunchConfig;
use crate::rules::CoworkerRecord;

/// Create tracking records for coworkers discovered on startup.
///
/// For each running coworker, creates a minimal `CoworkerRecord` so the
/// daemon can monitor their health. Workflow phase and task ID will be
/// populated when the coworker next reports state via RPC.
pub async fn recover_coworker_records(
    _repo_name: &str,
    coworkers: &CoworkerManager,
    coworker_records: &RwLock<HashMap<String, CoworkerRecord>>,
) {
    let discovered_names: Vec<String> = coworkers.list().iter().map(|c| c.name.clone()).collect();

    if discovered_names.is_empty() {
        return;
    }

    let mut records = coworker_records.write().await;
    for name in &discovered_names {
        info!("Tracking discovered coworker: {}", name);
        records.insert(name.to_string(), CoworkerRecord::new_spawn());
    }
}

/// Scan for and kill any orphaned Claude headless processes not tracked by the daemon.
///
/// This cleanup runs on daemon startup to remove zombie processes left behind
/// from crashes or unclean shutdowns. Kills processes that:
/// - Match the midtown settings pattern (scoped to this installation)
/// - Are truly orphaned (PPID=1), OR
/// - Are children of a stale midtown daemon (parent is a midtown process that isn't the current daemon)
/// - Are not tmux processes
///
/// `session_pids_to_preserve` is an exclusion list of PIDs belonging to headless
/// sessions that should be recovered on startup. These processes are intentionally
/// detached and will die naturally from broken pipes — killing them defeats the
/// purpose of session survival across daemon restarts.
pub fn kill_zombie_claude_processes(
    current_daemon_pid: u32,
    session_pids_to_preserve: &HashSet<u32>,
) {
    info!("Scanning for zombie Claude headless processes...");

    // Use the same pattern as the rest of the codebase to scope to this midtown installation
    let pattern = "claude.*--settings.*/midtown/.*-settings\\.json";

    // Find PIDs matching the pattern
    let output = match std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            // pgrep returns non-zero if no processes found - this is normal
            return;
        }
        Err(e) => {
            warn!("Failed to run pgrep: {}", e);
            return;
        }
    };

    let pids_str = String::from_utf8_lossy(&output.stdout);
    let candidate_pids: Vec<u32> = pids_str
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();

    if candidate_pids.is_empty() {
        return;
    }

    // Filter to orphaned processes or children of stale daemons, excluding tmux
    let mut zombie_pids = Vec::new();
    for pid in candidate_pids {
        // Skip PIDs belonging to sessions we intend to recover. These processes
        // are intentionally detached — they will die naturally from the broken
        // pipe when their stdin/stdout is closed. Killing them defeats session
        // survival across daemon restarts.
        if session_pids_to_preserve.contains(&pid) {
            info!(
                "Skipping session-survival process {} (will be recovered with --resume)",
                pid
            );
            continue;
        }

        // Check if process is tmux (should not kill)
        let is_tmux = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .starts_with("tmux")
            })
            .unwrap_or(false);

        if is_tmux {
            continue;
        }

        let ppid = get_ppid(pid);

        // Kill if truly orphaned (PPID=1)
        if ppid == Some(1) {
            // Verify PID still belongs to a claude process before marking for kill.
            // Between pgrep and now, the PID could have been recycled.
            if verify_claude_process(pid) {
                zombie_pids.push(pid);
            }
            continue;
        }

        // Kill if parent is a stale midtown daemon
        if let Some(parent_pid) = ppid
            && is_stale_midtown_daemon(parent_pid, current_daemon_pid)
        {
            info!(
                "Process {} has stale midtown daemon parent {} (not current {})",
                pid, parent_pid, current_daemon_pid
            );
            // Verify PID still belongs to a claude process before marking for kill.
            if verify_claude_process(pid) {
                zombie_pids.push(pid);
            }
        }
    }

    if zombie_pids.is_empty() {
        return;
    }

    info!(
        "Found {} zombie Claude process(es), killing...",
        zombie_pids.len()
    );
    for pid in &zombie_pids {
        info!("Sending SIGTERM to zombie process: {}", pid);
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .output();
    }

    // Poll up to 2 seconds for processes to exit gracefully, exiting early if all die.
    // This mirrors kill_stale_daemon's responsive wait strategy.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if zombie_pids.iter().all(|pid| !process_exists(*pid)) {
            info!("All zombie processes exited after SIGTERM");
            return;
        }
    }

    // SIGKILL any survivors
    for pid in &zombie_pids {
        if process_exists(*pid) {
            warn!(
                "Zombie process {} didn't exit after SIGTERM, sending SIGKILL",
                pid
            );
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }
}

/// Get the parent PID of a process.
fn get_ppid(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Verify that a PID belongs to a claude process.
///
/// Returns true if the process exists and its command line contains "claude".
/// This prevents accidentally killing unrelated processes when PIDs are reused.
fn verify_claude_process(pid: u32) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let cmdline = String::from_utf8_lossy(&output.stdout);
    cmdline.contains("claude")
}

/// Extract the names of coworkers being recovered from session records.
///
/// Called during startup to pre-register recovering coworkers in the daemon's
/// tracking maps BEFORE executing recovery effects. This prevents the task
/// dispatch tick from seeing their in_progress tasks as "orphaned" and
/// spawning duplicate coworkers for the same task.
pub async fn recovering_coworker_names(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
) -> Vec<String> {
    let state = persistent_state.lock().await;
    let mut names = Vec::new();

    for record in state.sessions.values() {
        if record.resume_on_startup
            && record.is_running
            && let Some(name) = record
                .preferred_name
                .as_ref()
                .or(record.current_name.as_ref())
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }

    names
}

/// Collect PIDs of sessions that should be recovered on startup.
///
/// These PIDs must be excluded from the zombie scanner — the sessions are
/// intentionally detached and will die naturally from broken pipes. Killing
/// them before session recovery runs defeats session survival.
pub async fn recoverable_session_pids(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
) -> HashSet<u32> {
    let state = persistent_state.lock().await;
    state
        .sessions
        .values()
        .filter(|record| record.resume_on_startup)
        .filter_map(|record| record.pid)
        .collect()
}

/// Recover coworker sessions from the session-centric `sessions` map.
///
/// Iterates `persistent_state.sessions` (SessionRecord), filters for
/// sessions that were actually running at shutdown (`is_running: true`),
/// deduplicates by name (keeping the most recently created session for
/// each coworker name), and emits `ResumeCoworker` effects.
///
/// Returns the set of recovered session_ids for deduplication with other
/// recovery paths (e.g., channel lead recovery).
pub async fn recover_from_session_records(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    repo_name: &str,
) -> (Vec<Effect>, HashSet<String>) {
    let mut effects = Vec::new();
    let mut recovered_session_ids = HashSet::new();

    let sessions = {
        let ps = persistent_state.lock().await;
        // Filter to sessions that were running at shutdown time.
        // The `sessions` map accumulates historical records; only those
        // with `is_running: true` were active when the daemon last persisted.
        let candidates: Vec<_> = ps
            .sessions
            .iter()
            .filter(|(_, record)| record.resume_on_startup && record.is_running)
            .map(|(session_id, record)| (session_id.clone(), record.clone()))
            .collect();

        // Deduplicate by name: a coworker name can appear in multiple
        // historical records. Keep only the most recently created session
        // for each name so we don't attempt redundant resume effects.
        let mut by_name: std::collections::HashMap<String, (String, SessionRecord)> =
            std::collections::HashMap::new();
        for (session_id, record) in candidates {
            let name = record
                .preferred_name
                .as_deref()
                .or(record.current_name.as_deref())
                .unwrap_or("unknown")
                .to_string();
            match by_name.entry(name) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert((session_id, record));
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if record.created_at > e.get().1.created_at {
                        e.insert((session_id, record));
                    }
                }
            }
        }
        by_name.into_values().collect::<Vec<_>>()
    };

    if sessions.is_empty() {
        return (effects, recovered_session_ids);
    }

    info!(
        "Recovering {} session(s) from session records",
        sessions.len()
    );

    for (session_id, record) in sessions {
        // Skip channel leads — recovered separately
        if record.coworker_type == "channel-lead" {
            info!(
                "Skipping channel lead session '{}' — recovered by channel lead recovery",
                session_id
            );
            continue;
        }

        let name = record
            .preferred_name
            .as_deref()
            .or(record.current_name.as_deref())
            .unwrap_or("unknown");

        info!(
            "Recovering session {} for {} (type={}, task={:?})",
            session_id, name, record.coworker_type, record.task_id
        );

        // Build launch config from SessionRecord
        let is_main_lead_session =
            record.coworker_type == "lead" || name == "lead" || name == repo_name;

        let mut config = if is_main_lead_session {
            // Lead session — uses lead system prompt, provider-compatible model,
            // unrestricted settings.
            // Must match recover_from_session_records() which also special-cases the lead.
            LaunchConfig::lead(repo_name, None)
        } else if record.is_reviewer {
            if let Some(pr_number) = record.pr_number {
                let reviewer_provider = crate::config::get_execution_provider_for_role(
                    repo_name,
                    crate::config::ExecutionRole::Reviewer,
                );
                // restart_count=0: we're recovering a session, not tracking restarts here
                LaunchConfig::reviewer(name, repo_name, pr_number, 0, reviewer_provider)
            } else {
                warn!("Reviewer session {} has no PR number, skipping", session_id);
                continue;
            }
        } else if let Some(ref task_id) = record.task_id {
            let initial_prompt = format!(
                "You've been assigned task !{}. Run `midtown task view {}` for full details.",
                task_id, task_id
            );
            LaunchConfig::coworker(
                name,
                repo_name,
                crate::launch::SessionMode::Fresh, // Will be overridden by ResumeCoworker effect
                Some(initial_prompt),
            )
        } else {
            LaunchConfig::coworker(name, repo_name, crate::launch::SessionMode::Fresh, None)
        };

        // Restore working directory from session record
        if !record.working_dir.is_empty() {
            config.working_dir = Some(std::path::PathBuf::from(&record.working_dir));
        }

        // Clear auth_profile_dir to re-resolve from project config
        config.auth_profile_dir = None;
        if config.persisted_initial_prompt.is_none() {
            config.persisted_initial_prompt = record
                .initial_prompt
                .clone()
                .or_else(|| config.initial_prompt.clone());
        }
        if matches!(config.role, crate::launch::CoworkerRole::Reviewer) {
            config.auth_provider = crate::config::get_execution_provider_for_role(
                repo_name,
                crate::config::ExecutionRole::Reviewer,
            );
        }
        config.model =
            super::helpers::resolve_model_for_role(repo_name, config.auth_provider, &config.role);

        recovered_session_ids.insert(session_id.clone());
        effects.push(Effect::ResumeCoworker {
            name: name.to_string(),
            session_id,
            config,
        });
    }

    (effects, recovered_session_ids)
}

/// Rebuild `channel_lead_sessions` from persisted SessionRecords.
///
/// Uses the canonical SessionRecord store instead of stale in-memory state.
/// This is required after daemon restarts, because `channel_lead_sessions` was
/// previously cleared on startup and never repopulated, which breaks channel lead
/// routing and thinking indicators.
pub async fn recover_channel_lead_session_mappings(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
) -> usize {
    let mut state = persistent_state.lock().await;
    let mut recovered = 0usize;
    let mut records_by_channel: HashMap<String, (chrono::DateTime<chrono::Utc>, SessionRecord)> =
        HashMap::new();

    // Start from a clean in-memory mapping and repopulate only from
    // persisted sessions that should be recoverable and look like root channel
    // leads (non-forked, resumable).
    state.channel_lead_sessions.clear();

    for record in state.sessions.values() {
        if record.coworker_type != "channel-lead" {
            continue;
        }
        if !record.resume_on_startup {
            continue;
        }

        // Only root channel leads (not forked topic sessions) are tracked in
        // `channel_lead_sessions`. Forked leads use `fork_bound_channels`.
        if record.bound_thread_id.is_some() {
            continue;
        }

        if record.session_id.is_empty() {
            continue;
        }

        let session_name = record
            .preferred_name
            .as_deref()
            .or(record.current_name.as_deref())
            .unwrap_or("");
        if session_name.is_empty() {
            continue;
        }

        let channel_name = record
            .channel
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(session_name);

        let should_replace = records_by_channel
            .get(channel_name)
            .is_none_or(|(created_at, _)| record.created_at > *created_at);
        if should_replace {
            records_by_channel.insert(
                channel_name.to_string(),
                (record.created_at, record.clone()),
            );
        }
    }

    for (channel_name, (_, record)) in records_by_channel {
        state
            .channel_lead_sessions
            .insert(channel_name, record.session_id.clone());
        recovered += 1;
    }

    if recovered > 0 {
        info!(
            "Recovered {} channel lead session mapping(s) from session records",
            recovered
        );
    }

    recovered
}

/// Clear is_running flags for sessions that were not recovered on startup.
///
/// On restart, `recover_from_session_records` resumes sessions where both
/// `is_running=true` and `resume_on_startup=true`. Sessions that are skipped
/// for any reason (non-resumable, reviewer sessions without PR numbers, or
/// dropped by the name-deduplication logic) retain their stale `is_running=true`
/// flag, causing dispatch to treat them as still active and skip pending tasks
/// indefinitely.
///
/// This function clears `is_running` to `false` for any session that:
/// - Has `is_running=true`
/// - Is NOT in `recovered_session_ids` (was not recovered by `recover_from_session_records`)
///
/// Channel lead sessions are included — they are on-demand and not recovered at startup.
/// Their `channel_lead_sessions` map is rebuilt in this module via
/// `recover_channel_lead_session_mappings()` prior to this stale-flag pass.
///
/// Call this after `recover_from_session_records` completes, before the event loop starts.
/// The caller is responsible for saving persistent state after this call.
pub async fn clear_stale_running_sessions(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    recovered_session_ids: &HashSet<String>,
) {
    let mut state = persistent_state.lock().await;
    let mut cleared = 0usize;

    for record in state.sessions.values_mut() {
        if !record.is_running {
            continue;
        }
        if recovered_session_ids.contains(&record.session_id) {
            continue;
        }
        info!(
            "Clearing stale is_running flag for session {} (name={:?}, type={}, resume_on_startup={})",
            record.session_id,
            record
                .preferred_name
                .as_deref()
                .or(record.current_name.as_deref()),
            record.coworker_type,
            record.resume_on_startup
        );
        record.is_running = false;
        cleared += 1;
    }

    if cleared > 0 {
        info!("Cleared stale is_running flag for {} session(s)", cleared);
    }
}

#[path = "startup_tests.rs"]
#[cfg(test)]
mod tests;
