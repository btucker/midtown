//! Process management utilities for orphan cleanup and PID tracking.
//!
//! Provides functions for detecting and killing orphaned processes,
//! checking process liveness, and managing process trees. These are
//! general-purpose utilities used by the daemon for cleanup, not tied
//! to any specific terminal multiplexer.

use std::process::Command;

/// Prefix for all midtown sessions.
pub const SESSION_PREFIX: &str = "midtown-";

/// Zellij session lifecycle state as reported by `zellij list-sessions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZellijSessionState {
    Running,
    Exited,
}

/// Check if a process is still alive.
pub fn is_pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get the parent PID of a process.
pub fn get_ppid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;

    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Get all descendant PIDs of a process (children, grandchildren, etc).
///
/// Uses `pgrep -P` to find immediate children, then recursively finds their children.
pub fn get_descendant_pids(parent_pid: u32) -> Vec<u32> {
    let mut descendants = Vec::new();
    let mut to_check = vec![parent_pid];

    while let Some(pid) = to_check.pop() {
        // Find immediate children of this PID
        let output = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output();

        if let Ok(o) = output
            && o.status.success()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Ok(child_pid) = line.trim().parse::<u32>()
                    && !descendants.contains(&child_pid)
                {
                    descendants.push(child_pid);
                    to_check.push(child_pid); // Check for grandchildren
                }
            }
        }
    }

    descendants
}

/// Find orphaned processes matching a pattern.
///
/// Returns PIDs of processes that:
/// 1. Match the given regex pattern in their command line
/// 2. Have PPID=1 (orphaned - no legitimate parent)
/// 3. Are NOT tmux/zellij processes (to avoid killing terminal servers)
///
/// This is conservative: only truly orphaned processes are returned.
/// The tmux exclusion is critical because `tmux new-session` commands may
/// match patterns like "claude" in their arguments, but killing the tmux
/// server would destroy all coworker windows.
pub fn find_orphaned_processes(pattern: &str) -> Vec<u32> {
    // Find PIDs matching the pattern
    let output = match Command::new("pgrep").args(["-f", pattern]).output() {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();

    // Filter to only orphaned processes (PPID=1) that are NOT tmux/zellij
    pids.into_iter()
        .filter(|&pid| {
            // Must be orphaned (PPID=1)
            if get_ppid(pid) != Some(1) {
                return false;
            }
            // Must NOT be a tmux or zellij process
            let is_mux = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output()
                .ok()
                .map(|o| {
                    let comm = String::from_utf8_lossy(&o.stdout);
                    let comm = comm.trim();
                    comm.starts_with("tmux") || comm.starts_with("zellij")
                })
                .unwrap_or(false);
            if is_mux {
                tracing::debug!(pid = pid, "Skipping mux process in orphan cleanup");
                return false;
            }
            true
        })
        .collect()
}

/// Kill orphaned processes matching a pattern.
///
/// Sends SIGTERM first, waits briefly, then SIGKILL to any survivors.
/// Returns the number of processes killed.
///
/// Only kills processes that are truly orphaned (PPID=1) to avoid
/// killing legitimate processes the user may have started.
pub fn kill_orphaned_processes(pattern: &str) -> usize {
    let orphan_pids = find_orphaned_processes(pattern);

    if orphan_pids.is_empty() {
        return 0;
    }

    let count = orphan_pids.len();

    // Log what we're about to kill for debugging
    for &pid in &orphan_pids {
        // Get process command line for debugging
        let cmdline = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        tracing::warn!(
            pid = pid,
            cmdline = %cmdline,
            pattern = %pattern,
            "ORPHAN_CLEANUP: killing orphaned claude process"
        );
    }

    // Send SIGTERM to orphaned processes
    let pid_strings: Vec<String> = orphan_pids.iter().map(|p| p.to_string()).collect();
    let _ = Command::new("kill")
        .args(&pid_strings)
        .stderr(std::process::Stdio::null())
        .status();

    // Wait briefly for processes to exit
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Force kill any survivors
    let survivors: Vec<String> = orphan_pids
        .iter()
        .filter(|&&pid| is_pid_alive(pid))
        .map(|p| p.to_string())
        .collect();

    if !survivors.is_empty() {
        let _ = Command::new("kill")
            .arg("-9")
            .args(&survivors)
            .stderr(std::process::Stdio::null())
            .status();
    }

    count
}

/// Collect PIDs of all pane processes in a tmux session.
///
/// Returns (window_name, pid) pairs for every pane in the session.
pub fn session_pane_pids(session: &str) -> Vec<(String, u32)> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{window_name} #{pane_pid}",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let mut parts = line.splitn(2, ' ');
                let name = parts.next()?.to_string();
                let pid = parts.next()?.parse().ok()?;
                Some((name, pid))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Send SIGTERM to all pane processes in a tmux session, then SIGKILL any survivors.
///
/// Claude Code (node) installs a SIGHUP handler, so `tmux kill-session`
/// (which sends SIGHUP) leaves orphaned processes consuming memory and
/// potentially causing contention with other Claude instances. SIGTERM
/// triggers a clean shutdown.
///
/// Also kills child processes (Claude spawns node subprocesses) to ensure
/// complete cleanup even if the parent shell exits but children survive.
pub fn terminate_session_processes(session: &str) {
    let pids = session_pane_pids(session);
    if pids.is_empty() {
        return;
    }

    // Collect all pane PIDs and their descendants
    let mut all_pids: Vec<u32> = Vec::new();
    for (_, pid) in &pids {
        all_pids.push(*pid);
        // Also collect child processes (Claude's node subprocesses)
        all_pids.extend(get_descendant_pids(*pid));
    }
    all_pids.sort();
    all_pids.dedup();

    if all_pids.is_empty() {
        return;
    }

    // Send SIGTERM to all processes
    let pid_strings: Vec<String> = all_pids.iter().map(|p| p.to_string()).collect();
    let _ = Command::new("kill")
        .args(&pid_strings)
        .stderr(std::process::Stdio::null())
        .status();

    tracing::debug!(
        "Sent SIGTERM to {} processes in session {}",
        all_pids.len(),
        session
    );

    // Poll for processes to exit (up to 2 seconds)
    let poll_interval = std::time::Duration::from_millis(100);
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        std::thread::sleep(poll_interval);
        let survivors: Vec<u32> = all_pids
            .iter()
            .copied()
            .filter(|&p| is_pid_alive(p))
            .collect();
        if survivors.is_empty() {
            tracing::debug!("All processes in session {} exited cleanly", session);
            return;
        }
    }

    // Force kill any survivors
    let survivors: Vec<u32> = all_pids
        .iter()
        .copied()
        .filter(|&p| is_pid_alive(p))
        .collect();
    if !survivors.is_empty() {
        tracing::warn!(
            "Force killing {} processes that didn't exit: {:?}",
            survivors.len(),
            survivors
        );
        let pid_strings: Vec<String> = survivors.iter().map(|p| p.to_string()).collect();
        let _ = Command::new("kill")
            .arg("-9")
            .args(&pid_strings)
            .stderr(std::process::Stdio::null())
            .status();

        // Brief wait for SIGKILL to take effect
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn parse_zellij_session_state(session: &str, line: &str) -> Option<ZellijSessionState> {
    let name = line.split_whitespace().next()?;
    if name != session {
        return None;
    }
    if line.contains("(EXITED") {
        Some(ZellijSessionState::Exited)
    } else {
        Some(ZellijSessionState::Running)
    }
}

/// Get Zellij session state for a specific session name.
///
/// Parses `zellij list-sessions --no-formatting` output and returns:
/// - `Some(Running)` for active sessions
/// - `Some(Exited)` for resurrectable sessions
/// - `None` if not found
pub fn zellij_session_state(session: &str) -> Option<ZellijSessionState> {
    let output = Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .find_map(|line| parse_zellij_session_state(session, line))
        }
        _ => None,
    }
}

/// Check if a Zellij session with the given name exists (running or exited).
pub fn zellij_session_exists(session: &str) -> bool {
    zellij_session_state(session).is_some()
}

/// Check if a Zellij session with the given name is actively running.
pub fn zellij_running_session_exists(session: &str) -> bool {
    zellij_session_state(session) == Some(ZellijSessionState::Running)
}

/// Check if Zellij is available on the system.
pub fn zellij_is_available() -> bool {
    Command::new("zellij")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_state_running() {
        let line = "midtown-midtown [Created 2m ago]";
        assert_eq!(
            parse_zellij_session_state("midtown-midtown", line),
            Some(ZellijSessionState::Running)
        );
    }

    #[test]
    fn parse_session_state_exited() {
        let line = "midtown-midtown [Created 33m 29s ago] (EXITED - attach to resurrect)";
        assert_eq!(
            parse_zellij_session_state("midtown-midtown", line),
            Some(ZellijSessionState::Exited)
        );
    }

    #[test]
    fn parse_session_state_mismatch() {
        let line = "midtown-other [Created 1m ago]";
        assert_eq!(parse_zellij_session_state("midtown-midtown", line), None);
    }
}
