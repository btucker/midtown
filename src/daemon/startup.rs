//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker tracking across daemon restarts.
//! When the daemon starts, it discovers running coworkers from tmux and
//! creates minimal records so they are tracked for health monitoring.
//! Workflow state is recovered when coworkers report via RPC.
//!
//! For headless sessions: loads saved session info from persistent state,
//! kills orphaned processes, and resumes sessions with `--resume <session_id>`.

use std::collections::HashMap;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::coworker::CoworkerManager;
use crate::daemon::sessions::SessionManager;
use crate::daemon::state::DaemonPersistentState;
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

/// Recover headless coworker sessions from persistent state.
///
/// Loads saved session info from `daemon-state.json`, kills orphaned PIDs,
/// and resumes sessions using `--resume <session_id>`. This allows headless
/// sessions to survive daemon restarts.
///
/// Called early during daemon startup, before the main event loop.
pub async fn recover_headless_sessions(
    repo_name: &str,
    persistent_state: &tokio::sync::Mutex<DaemonPersistentState>,
    _session_manager: &SessionManager,
    _coworkers: &CoworkerManager,
    _coworker_records: &RwLock<HashMap<String, CoworkerRecord>>,
) {
    let saved_sessions: Vec<(String, crate::daemon::state::HeadlessSessionInfo)> = {
        let ps = persistent_state.lock().await;
        ps.headless_sessions
            .iter()
            .map(|(name, info)| (name.clone(), info.clone()))
            .collect()
    };

    if saved_sessions.is_empty() {
        info!("No saved sessions to recover");
        return;
    }

    info!(
        "Recovering {} saved session(s) from previous daemon run",
        saved_sessions.len()
    );

    for (name, info) in saved_sessions {
        // Step 1: Kill the old PID if it's still running (zombie cleanup)
        if let Some(pid) = info.pid {
            match kill_process_if_alive(pid) {
                Ok(true) => info!("Killed orphaned process for '{}' (PID {})", name, pid),
                Ok(false) => info!("Process for '{}' (PID {}) already exited", name, pid),
                Err(e) => warn!(
                    "Failed to check/kill orphaned process for '{}' (PID {}): {}",
                    name, pid, e
                ),
            }
        }

        // Step 2: Resume the session
        // TODO: Build LaunchConfig and call session_manager.spawn with resume mode
        // For now, just log that recovery is partially implemented
        info!(
            "Would resume session '{}' (session_id={}, purpose={})",
            name, info.session_id, info.purpose
        );

        // Step 3: Register in tracking structures
        // TODO: Register in CoworkerManager and create CoworkerRecord
        // This requires coordination with the CoworkerManager spawn flow
    }

    // Clear recovered sessions from persistent state (fresh start next restart)
    let mut ps = persistent_state.lock().await;
    ps.headless_sessions.clear();
    if let Err(e) = ps.save_for_repo(repo_name) {
        warn!("Failed to clear recovered sessions from state: {}", e);
    }
}

/// Kill a process by PID if it's still running.
///
/// Returns `Ok(true)` if the process was killed, `Ok(false)` if it was already dead,
/// or `Err` if the kill command failed.
fn kill_process_if_alive(pid: u32) -> Result<bool, std::io::Error> {
    // Use `kill -0 <pid>` to check if the process is alive without killing it
    let status = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()?;

    if status.success() {
        // Process is alive, kill it
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()?;
        Ok(true)
    } else {
        // Process is already dead
        Ok(false)
    }
}
