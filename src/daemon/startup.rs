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
    state.headless_sessions.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;

    /// Create a test HeadlessSessionInfo.
    fn test_session_info(
        name: &str,
        task_id: Option<u64>,
    ) -> crate::daemon::state::HeadlessSessionInfo {
        crate::daemon::state::HeadlessSessionInfo {
            session_id: format!("session-{}", name),
            last_active: Utc::now(),
            purpose: format!("test session for {}", name),
            pid: Some(99999), // Non-existent PID
            coworker_type: Some("dev".to_string()),
            task_id,
            pr_number: None,
            working_dir: Some("/tmp/test".to_string()),
            provider: None,
        }
    }

    #[tokio::test]
    async fn test_recover_headless_sessions_generates_resume_effects() {
        let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

        // Insert test sessions
        {
            let mut state = persistent_state.lock().await;
            state.headless_sessions.insert(
                "amsterdam".to_string(),
                test_session_info("amsterdam", Some(42)),
            );
            state.headless_sessions.insert(
                "columbus".to_string(),
                test_session_info("columbus", Some(43)),
            );
        }

        let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

        // Should generate exactly 2 ResumeCoworker effects (one per session)
        assert_eq!(
            effects.len(),
            2,
            "Should generate one ResumeCoworker per session"
        );

        for effect in &effects {
            match effect {
                Effect::ResumeCoworker {
                    name, session_id, ..
                } => {
                    assert!(
                        name == "amsterdam" || name == "columbus",
                        "Unexpected coworker name: {}",
                        name
                    );
                    assert!(
                        session_id.starts_with("session-"),
                        "Session ID should match what was persisted"
                    );
                }
                _ => panic!("Expected only ResumeCoworker effects, got {:?}", effect),
            }
        }
    }

    #[tokio::test]
    async fn test_recover_headless_sessions_does_not_kill_processes() {
        // This test verifies that recover_headless_sessions does NOT generate
        // any kill effects. The old behavior was to kill -9 the processes,
        // which defeated the purpose of session detachment.
        //
        // We verify this by checking that only ResumeCoworker effects are returned.
        // If kill behavior were added back, it would need to be an Effect variant,
        // and this test would catch the regression.
        let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

        {
            let mut state = persistent_state.lock().await;
            state
                .headless_sessions
                .insert("park".to_string(), test_session_info("park", Some(100)));
        }

        let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

        // All effects should be ResumeCoworker — no kill effects
        for effect in &effects {
            assert!(
                matches!(effect, Effect::ResumeCoworker { .. }),
                "Recovery should only produce ResumeCoworker effects (no kills), got: {:?}",
                effect
            );
        }
    }

    #[tokio::test]
    async fn test_recover_headless_sessions_empty() {
        let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
        let effects = recover_headless_sessions(&persistent_state, "test-repo").await;
        assert!(
            effects.is_empty(),
            "No sessions to recover should produce no effects"
        );
    }

    #[tokio::test]
    async fn test_recovering_coworker_names_returns_session_names() {
        let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

        {
            let mut state = persistent_state.lock().await;
            state.headless_sessions.insert(
                "amsterdam".to_string(),
                test_session_info("amsterdam", Some(42)),
            );
            state.headless_sessions.insert(
                "columbus".to_string(),
                test_session_info("columbus", Some(43)),
            );
        }

        let names = recovering_coworker_names(&persistent_state).await;
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"amsterdam".to_string()));
        assert!(names.contains(&"columbus".to_string()));
    }

    #[tokio::test]
    async fn test_recovering_coworker_names_empty_state() {
        let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
        let names = recovering_coworker_names(&persistent_state).await;
        assert!(names.is_empty());
    }
}
