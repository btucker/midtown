//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker tracking across daemon restarts.
//! When the daemon starts:
//! - Discovers running coworkers from tmux and creates minimal records for health monitoring
//! - Recovers headless coworker sessions from persisted state and resumes them with --resume
//! - Cleans up zombie processes from previous daemon runs (orphaned PPID=1 processes)
//!
//! Workflow state is recovered when coworkers report via RPC.

use std::collections::HashMap;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::coworker::CoworkerManager;
use crate::daemon::effects::Effect;
use crate::daemon::state::DaemonPersistentState;
use crate::launch::LaunchConfig;
use crate::rules::CoworkerRecord;

/// Create tracking records for coworkers discovered in the tmux session.
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
/// 1. Kill the old orphaned process (zombie cleanup)
/// 2. Generate a `ResumeCoworker` effect to spawn with --resume <session_id>
/// 3. Clear the headless_sessions map after recovery
///
/// Returns a Vec of effects to execute during startup.
pub async fn recover_headless_sessions(
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    repo_name: &str,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Clone the headless_sessions map for iteration.
    // We preserve the original map so sync_with_tmux() can restore session_ids
    // for recovered coworkers during normal operation.
    let sessions = {
        let state = persistent_state.lock().await;
        state.headless_sessions.clone()
    };

    if sessions.is_empty() {
        return effects;
    }

    info!(
        "Recovering {} headless session(s) from previous daemon run",
        sessions.len()
    );

    for (name, session_info) in sessions {
        info!(
            "Recovering session for {}: session_id={}, purpose={}",
            name, session_info.session_id, session_info.purpose
        );

        // Kill the old orphaned process if it still exists and is a claude process
        if let Some(pid) = session_info.pid {
            // Verify the PID belongs to a claude process before killing
            // (PIDs can be reused between daemon restarts)
            let is_claude = verify_claude_process(pid);
            if is_claude {
                info!("Killing orphaned claude process {} for {}", pid, name);
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(pid.to_string())
                    .output();
            } else {
                warn!(
                    "PID {} for {} is not a claude process (or already dead), skipping kill",
                    pid, name
                );
            }
        }

        // Build launch config based on coworker type and saved context
        let mut config = match (
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
                    name, session_info.coworker_type, session_info.task_id, session_info.pr_number
                );
                LaunchConfig::coworker(&name, repo_name, crate::launch::SessionMode::Fresh, None)
            }
        };

        // Restore working directory if available
        if let Some(ref working_dir) = session_info.working_dir {
            config.working_dir = Some(std::path::PathBuf::from(working_dir));
        }

        // Restore provider if persisted (defaults to Claude for old state files)
        if let Some(provider) = session_info.provider {
            config.auth_provider = provider;
        }

        // Create resume effect
        effects.push(Effect::ResumeCoworker {
            name: name.clone(),
            session_id: session_info.session_id.clone(),
            config,
        });
    }

    // NOTE: We preserve headless_sessions in persistent state so sync_with_tmux()
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
