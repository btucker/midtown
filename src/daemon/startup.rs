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
    session_manager: &SessionManager,
    coworkers: &CoworkerManager,
    coworker_records: &RwLock<HashMap<String, CoworkerRecord>>,
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

        // Step 2: Determine coworker role and build LaunchConfig
        let working_dir = match &info.working_dir {
            Some(dir) => std::path::PathBuf::from(dir),
            None => {
                warn!(
                    "Skipping recovery for '{}': no working_dir in saved session",
                    name
                );
                continue;
            }
        };

        let role = match info.coworker_type.as_deref() {
            Some("reviewer") => crate::launch::CoworkerRole::Reviewer,
            Some("dev") | None => crate::launch::CoworkerRole::Coworker,
            Some(other) => {
                warn!(
                    "Unknown coworker type '{}' for '{}', treating as dev",
                    other, name
                );
                crate::launch::CoworkerRole::Coworker
            }
        };

        let model = if matches!(role, crate::launch::CoworkerRole::Reviewer) {
            "opus".to_string()
        } else {
            "sonnet".to_string()
        };

        let task_mode = if info.pr_number.is_some() {
            // Reviewers use isolated task lists
            crate::launch::TaskMode::Isolated
        } else {
            // Dev coworkers share the team task list
            crate::launch::TaskMode::Shared {
                repo_name: repo_name.to_string(),
            }
        };

        let launch_config = crate::launch::LaunchConfig {
            name: name.clone(),
            session_mode: crate::launch::SessionMode::ResumeSession(info.session_id.clone()),
            task_mode,
            role,
            initial_prompt: None, // Resume mode doesn't need initial prompt
            additional_dirs: vec![],
            restrict_setting_sources: true,
            pr_number: info.pr_number,
            team_name: Some(crate::mailbox::team_name_for_repo(repo_name)),
            working_dir: Some(working_dir.clone()),
            model: model.clone(),
        };

        // Convert to headless config
        let mut headless_config = launch_config.to_headless_config();
        headless_config.cwd = Some(working_dir.to_string_lossy().to_string());

        // Step 3: Spawn the resumed session
        match session_manager.spawn(&name, &headless_config, None).await {
            Ok(()) => {
                info!(
                    "Resumed session '{}' (session_id={}, purpose={})",
                    name, info.session_id, info.purpose
                );

                // Step 4: Register in CoworkerManager
                // This creates the Coworker tracking record so the daemon can monitor the coworker
                let isolated_tasks = info.pr_number.is_some(); // Reviewers use isolated task lists
                if let Err(e) = coworkers.register(
                    &name,
                    working_dir.to_string_lossy().to_string(),
                    Some(info.session_id.clone()),
                    isolated_tasks,
                    model,
                ) {
                    warn!(
                        "Failed to register recovered coworker '{}' in CoworkerManager: {}",
                        name, e
                    );
                    // Continue anyway — session is running, just won't be fully tracked
                }

                // Step 5: Create CoworkerRecord with recovered metadata
                let mut records = coworker_records.write().await;
                let mut record = CoworkerRecord::new_spawn();
                if let Some(task_id) = info.task_id {
                    record.task_id = Some(task_id as u32);
                }
                records.insert(name.clone(), record);

                // Step 6: If this was a reviewer, restore the PR assignment
                if let Some(pr) = info.pr_number {
                    let mut ps = persistent_state.lock().await;
                    ps.github.assign_reviewer(
                        pr,
                        &name,
                        crate::github_state::AssignmentSource::Recovery,
                    );
                    // Save immediately so the assignment is persisted
                    if let Err(e) = ps.save_for_repo(repo_name) {
                        warn!("Failed to persist recovered PR assignment: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to resume session '{}' (session_id={}): {}",
                    name, info.session_id, e
                );
                // Continue to next session — partial recovery is better than none
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::HeadlessSessionInfo;
    use chrono::Utc;

    #[test]
    fn test_kill_nonexistent_process() {
        // PID 99999 should not exist on most systems
        let result = kill_process_if_alive(99999);

        // Should return Ok(false) — process was already dead
        match result {
            Ok(false) => {} // Expected
            Ok(true) => panic!("Unexpectedly killed PID 99999 (it shouldn't exist)"),
            Err(e) => panic!("kill command failed: {}", e),
        }
    }

    #[test]
    fn test_session_info_has_all_metadata() {
        let info = HeadlessSessionInfo {
            session_id: "test-session-123".to_string(),
            last_active: Utc::now(),
            purpose: "task !42: Implement feature X".to_string(),
            pid: Some(9999),
            coworker_type: Some("dev".to_string()),
            task_id: Some(42),
            pr_number: None,
            working_dir: Some("/path/to/worktree".to_string()),
        };

        // Verify fields are accessible
        assert_eq!(info.session_id, "test-session-123");
        assert_eq!(info.pid, Some(9999));
        assert_eq!(info.coworker_type, Some("dev".to_string()));
        assert_eq!(info.task_id, Some(42));
        assert_eq!(info.pr_number, None);
        assert_eq!(info.working_dir, Some("/path/to/worktree".to_string()));
    }

    #[test]
    fn test_reviewer_session_metadata() {
        let info = HeadlessSessionInfo {
            session_id: "reviewer-session-456".to_string(),
            last_active: Utc::now(),
            purpose: "reviewer for PR #123".to_string(),
            pid: Some(8888),
            coworker_type: Some("reviewer".to_string()),
            task_id: None,
            pr_number: Some(123),
            working_dir: Some("/path/to/reviewer/worktree".to_string()),
        };

        assert_eq!(info.coworker_type, Some("reviewer".to_string()));
        assert_eq!(info.pr_number, Some(123));
        assert_eq!(info.task_id, None);
    }

    #[test]
    fn test_session_info_backward_compatibility() {
        // Old sessions might not have the new fields
        let old_json = r#"{
            "session_id": "old-session",
            "last_active": "2026-01-01T00:00:00Z",
            "purpose": "old task"
        }"#;

        let info: HeadlessSessionInfo = serde_json::from_str(old_json).unwrap();

        // Should deserialize with defaults for missing fields
        assert_eq!(info.session_id, "old-session");
        assert_eq!(info.pid, None);
        assert_eq!(info.coworker_type, None);
        assert_eq!(info.task_id, None);
        assert_eq!(info.pr_number, None);
        assert_eq!(info.working_dir, None);
    }
}
