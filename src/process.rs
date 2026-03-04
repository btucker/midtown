//! Process management utilities for orphan cleanup and PID tracking.
//!
//! Provides functions for detecting and killing orphaned processes,
//! checking process liveness, and managing process trees.

use std::process::Command;

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
///
/// This is conservative: only truly orphaned processes are returned.
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

    // Filter to only orphaned processes (PPID=1)
    pids.into_iter()
        .filter(|&pid| get_ppid(pid) == Some(1))
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
