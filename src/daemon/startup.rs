//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker tracking across daemon restarts.
//! When the daemon starts, it discovers running coworkers from tmux and
//! creates minimal records so they are tracked for health monitoring.
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

    // Take ownership of the headless_sessions map so we can drain it
    let sessions = {
        let mut state = persistent_state.lock().await;
        std::mem::take(&mut state.headless_sessions)
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

        // Kill the old orphaned process if it still exists
        if let Some(pid) = session_info.pid {
            info!("Killing orphaned process {} for {}", pid, name);
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .output();
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

        // Create resume effect
        effects.push(Effect::ResumeCoworker {
            name: name.clone(),
            session_id: session_info.session_id.clone(),
            config,
        });
    }

    // Clear the headless_sessions map after recovery (save empty state)
    {
        let state = persistent_state.lock().await;
        if let Err(e) = state.save_for_repo(repo_name) {
            warn!(
                "Failed to save empty headless_sessions after recovery: {}",
                e
            );
        }
    }

    effects
}
