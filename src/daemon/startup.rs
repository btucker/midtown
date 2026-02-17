//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker tracking across daemon restarts.
//! When the daemon starts:
//! - Recovers headless coworker sessions from persisted state and resumes them with --resume
//! - Cleans up zombie processes from previous daemon runs (orphaned PPID=1 processes)
//!
//! Workflow state is recovered when coworkers report via RPC.

use std::collections::HashMap;

use tokio::sync::RwLock;
use tracing::{info, warn};

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

use crate::coworker::CoworkerManager;
use crate::daemon::effects::Effect;
use crate::daemon::state::DaemonPersistentState;
use crate::launch::{LaunchConfig, SessionMode};
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
/// from crashes or unclean shutdowns. Only kills processes that:
/// - Match the midtown settings pattern (scoped to this installation)
/// - Are truly orphaned (PPID=1)
/// - Are not tmux processes
pub fn kill_zombie_claude_processes() {
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

    // Filter to only truly orphaned processes (PPID=1) and exclude tmux
    let mut zombie_pids = Vec::new();
    for pid in candidate_pids {
        // Check if process is orphaned (PPID=1)
        let ppid = get_ppid(pid);
        if ppid != Some(1) {
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

        zombie_pids.push(pid);
    }

    if zombie_pids.is_empty() {
        return;
    }

    info!(
        "Found {} zombie Claude process(es), killing...",
        zombie_pids.len()
    );
    for pid in &zombie_pids {
        info!("Killing zombie process: {}", pid);
        let _ = std::process::Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output();
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

/// Recover headless coworker sessions from persisted state after daemon restart.
///
/// For each saved session:
/// 1. Let the old process die naturally (broken pipe from detached daemon)
/// 2. Generate a `ResumeCoworker` effect to spawn with --resume <session_id>
///
/// **Important**: We do NOT kill the old processes here. The previous daemon
/// detached its pipe handles during shutdown (`detach_on_drop`), so the child
/// processes will receive SIGPIPE or a write error on their stdout and exit
/// naturally. Killing them with SIGKILL is counterproductive — it defeats the
/// purpose of session detachment and can cause data loss.
///
/// Returns a Vec of effects to execute during startup.
pub async fn recover_headless_sessions(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    repo_name: &str,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Clone only sessions that should be resumed on startup.
    // Historical sessions remain persisted for manual `session attach`.
    let (sessions, total_persisted) = {
        let state = persistent_state.lock().await;
        let total = state.headless_sessions.len();
        let resumable = state
            .headless_sessions
            .iter()
            .filter_map(|(name, info)| {
                if info.resume_on_startup {
                    Some((name.clone(), info.clone()))
                } else {
                    None
                }
            })
            .collect::<std::collections::HashMap<_, _>>();
        (resumable, total)
    };

    if sessions.is_empty() {
        return effects;
    }

    info!(
        "Recovering {} headless session(s) from previous daemon run ({} persisted total)",
        sessions.len(),
        total_persisted
    );

    for (name, session_info) in sessions {
        info!(
            "Recovering session for {}: session_id={}, purpose={}",
            name, session_info.session_id, session_info.purpose
        );

        // Don't kill the old process — let it die naturally from the broken pipe.
        // When the previous daemon detached, it closed its end of stdin/stdout.
        // The child process will get SIGPIPE or a write error and exit on its own.
        // We'll spawn a fresh process with --resume to continue the session.
        if let Some(pid) = session_info.pid {
            if verify_claude_process(pid) {
                info!(
                    "Previous process {} for {} still running — will die naturally from broken pipe",
                    pid, name
                );
            } else {
                info!(
                    "Previous process {} for {} already exited (PID reused or dead)",
                    pid, name
                );
            }
        }

        // Build launch config based on coworker type and saved context
        let mut config = if name == "lead" {
            // Lead session — uses lead system prompt, unrestricted settings
            LaunchConfig::lead(repo_name)
        } else {
            match (
                session_info.coworker_type.as_deref(),
                session_info.task_id,
                session_info.pr_number,
            ) {
                (Some("dev"), Some(task_id), _) => {
                    // Dev coworker with task assignment
                    let initial_prompt = format!(
                        "You've been assigned task !{}. Run `midtown task view {}` for full details.",
                        task_id, task_id
                    );
                    LaunchConfig::coworker(
                        &name,
                        repo_name,
                        crate::launch::SessionMode::Fresh, // Will be overridden by ResumeCoworker effect
                        Some(initial_prompt),
                    )
                }
                (Some("reviewer"), _, Some(pr_num)) => {
                    // Reviewer coworker
                    LaunchConfig::reviewer(&name, pr_num)
                }
                _ => {
                    // Fallback: generic dev coworker
                    warn!(
                        "Session {} has incomplete metadata (type={:?}, task={:?}, pr={:?}), using generic config",
                        name,
                        session_info.coworker_type,
                        session_info.task_id,
                        session_info.pr_number
                    );
                    LaunchConfig::coworker(
                        &name,
                        repo_name,
                        crate::launch::SessionMode::Fresh,
                        None,
                    )
                }
            }
        };

        // Restore working directory. For the lead, always use the canonical
        // lead worktree path (~/.midtown/worktrees/<repo>/lead) rather than
        // the persisted path, which may be stale (e.g., legacy coworker path).
        if name == "lead" {
            let lead_wt = crate::paths::lead_worktree_path(repo_name);
            if lead_wt.exists() {
                config.working_dir = Some(lead_wt);
            }
        } else if let Some(ref working_dir) = session_info.working_dir {
            config.working_dir = Some(std::path::PathBuf::from(working_dir));
        }

        // Restore provider if persisted (defaults to Claude for old state files)
        if let Some(provider) = session_info.provider {
            config.auth_provider = provider;
        }

        // Re-resolve auth profile from project config rather than relying on
        // the persisted profile name, which may be stale or incorrectly extracted
        // (e.g., "claude" instead of "info@user.com" due to a prior path-extraction bug).
        // The project config is the authoritative source for auth profiles.
        config.auth_profile_dir = None; // Let spawn_coworker() re-resolve from project config

        // Create resume effect
        effects.push(Effect::ResumeCoworker {
            name: name.clone(),
            session_id: session_info.session_id.clone(),
            config,
        });
    }

    // NOTE: We preserve headless_sessions in persistent state so retain_alive()
    // can restore session_ids for recovered coworkers. The map will be overwritten
    // at the next shutdown with fresh data.
    //
    // This approach trades a theoretical double-recovery risk (if daemon restarts
    // rapidly before sessions init) for correct session_id restoration during normal
    // operation. The trade-off is acceptable because:
    // 1. Rapid daemon restarts are rare in practice
    // 2. Double-recovery is detected by SessionManager (session_id already alive)
    // 3. Correct session_id tracking is critical for reviewer spawning

    effects
}

/// Extract the names of coworkers being recovered from persisted headless sessions.
///
/// Called during startup to pre-register recovering coworkers in the daemon's
/// tracking maps BEFORE executing recovery effects. This prevents the task
/// dispatch tick from seeing their in_progress tasks as "orphaned" and
/// spawning duplicate coworkers for the same task.
pub async fn recovering_coworker_names(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
) -> Vec<String> {
    let state = persistent_state.lock().await;
    state
        .headless_sessions
        .iter()
        .filter_map(|(name, info)| {
            if info.resume_on_startup {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Recover channel lead sessions from persisted state after daemon restart.
///
/// For each active (non-archived) topic channel:
/// - If a session ID is persisted in `channel_lead_sessions`, emit a `SpawnCoworker`
///   with `SessionMode::ResumeSession` to resume it.
/// - If no session ID is persisted, emit a `SpawnCoworker` with `SessionMode::Fresh`
///   to start a new channel lead session.
///
/// Also emits a `SaveChannelLeadSession` effect (with an empty session ID placeholder)
/// for any channel that doesn't already have an entry in `channel_lead_sessions`, so
/// the backfill logic knows this channel has a lead session in flight.
///
/// The "midtown" main channel is excluded — it uses the Lead session, not a channel lead.
pub async fn recover_channel_lead_sessions(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    repo_name: &str,
) -> Vec<Effect> {
    let base_dir = crate::paths::projects_dir_for_repo(repo_name);
    recover_channel_lead_sessions_from(persistent_state, repo_name, &base_dir).await
}

/// Inner implementation for `recover_channel_lead_sessions`, separated for testability.
///
/// Takes an explicit `base_dir` to allow tests to use a temporary directory instead
/// of the real `~/.midtown/projects/<repo>/` path.
pub async fn recover_channel_lead_sessions_from(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    repo_name: &str,
    base_dir: &std::path::Path,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // List active (non-archived) channels
    let channels = match crate::channel::Channel::list(base_dir, false, None) {
        Ok(channels) => channels,
        Err(e) => {
            warn!("Failed to list channels for channel lead recovery: {}", e);
            return effects;
        }
    };

    // Filter to topic channels only (exclude main "midtown" channel)
    let topic_channels: Vec<_> = channels
        .into_iter()
        .filter(|c| !c.is_archived && c.name != "midtown")
        .collect();

    if topic_channels.is_empty() {
        return effects;
    }

    let channel_lead_sessions = {
        let ps = persistent_state.lock().await;
        ps.channel_lead_sessions.clone()
    };

    info!(
        "Recovering {} channel lead session(s): {:?}",
        topic_channels.len(),
        topic_channels.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    for channel_info in &topic_channels {
        let channel_name = &channel_info.name;

        let session_mode = if let Some(session_id) = channel_lead_sessions.get(channel_name)
            && !session_id.is_empty()
        {
            info!(
                "Resuming channel lead session for '{}': {}",
                channel_name, session_id
            );
            SessionMode::ResumeSession(session_id.clone())
        } else {
            info!(
                "No saved session for channel lead '{}', spawning fresh",
                channel_name
            );
            SessionMode::Fresh
        };

        let config = LaunchConfig::channel_lead(
            channel_name,
            repo_name,
            session_mode,
            "", // domain_context: empty at startup, accumulates via session persistence
        );

        effects.push(Effect::SpawnCoworker(config));

        // Register the channel in channel_lead_sessions if not already there
        // (empty session ID placeholder; backfilled when init event arrives)
        if !channel_lead_sessions.contains_key(channel_name.as_str()) {
            effects.push(Effect::SaveChannelLeadSession {
                channel_name: channel_name.clone(),
                session_id: String::new(), // placeholder; backfilled on init event
            });
        }
    }

    effects
}

#[path = "startup_tests.rs"]
#[cfg(test)]
mod tests;
