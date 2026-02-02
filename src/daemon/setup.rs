//! Daemon initialization helpers: PID lock acquisition and worktree loading.
//!
//! These functions run once at daemon startup. Extracted from mod.rs to keep
//! the event loop module thin.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use fs2::FileExt;
use tracing::{info, warn};

use super::state::DaemonConfig;
use crate::worktree::WorktreeManager;

/// Load additional WorktreeManagers for multi-repo projects.
///
/// Reads the project config to find additional repos (beyond the primary/workdir)
/// and creates a WorktreeManager for each. Failures are logged but don't prevent
/// the daemon from starting - the primary repo always works.
pub(super) fn load_additional_worktree_managers(
    project_name: &str,
    config: &DaemonConfig,
) -> Vec<WorktreeManager> {
    let full_config = match crate::config::load_full_project_config(project_name) {
        Some(c) => c,
        None => return vec![],
    };

    let primary = config.workdir.to_string_lossy().to_string();
    let repos = full_config.project.repos();

    repos
        .into_iter()
        .filter(|r| {
            // Skip the primary repo (it's already handled by the main WorktreeManager)
            let repo_path = std::path::Path::new(r);
            repo_path
                .canonicalize()
                .ok()
                .and_then(|canon| {
                    std::path::Path::new(&primary)
                        .canonicalize()
                        .ok()
                        .map(|p| canon != p)
                })
                .unwrap_or_else(|| *r != primary.as_str())
        })
        .filter_map(
            |repo_path| match WorktreeManager::new(std::path::PathBuf::from(repo_path)) {
                Ok(mgr) => {
                    info!("Additional repo for coworker worktrees: {}", repo_path);
                    Some(mgr)
                }
                Err(e) => {
                    warn!("Failed to create worktree manager for {}: {}", repo_path, e);
                    None
                }
            },
        )
        .collect()
}

/// Acquire an exclusive lock on the PID file.
///
/// This enforces singleton behavior - only one daemon can run per repository.
/// The lock is held for the lifetime of the returned File handle.
///
/// Returns an error if another daemon is already running (lock already held).
pub(super) fn acquire_pid_lock(pid_path: &PathBuf) -> crate::Result<File> {
    // Ensure parent directory exists
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open or create the PID file
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(pid_path)?;

    // Try to acquire an exclusive lock (non-blocking)
    match file.try_lock_exclusive() {
        Ok(()) => {
            // We got the lock - write our PID
            let pid = std::process::id();
            file.set_len(0)?; // Truncate any old content
            writeln!(file, "{}", pid)?;
            file.sync_all()?;
            Ok(file)
        }
        Err(e) => {
            // Lock is held by another process
            // Try to read the existing PID for a better error message
            let existing_pid = std::fs::read_to_string(pid_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());

            let msg = match existing_pid {
                Some(pid) => format!(
                    "Another daemon is already running (PID {}). Stop it first with 'midtown stop'.",
                    pid
                ),
                None => format!(
                    "Another daemon is already running. Stop it first with 'midtown stop'. ({})",
                    e
                ),
            };

            Err(crate::Error::Io(std::io::Error::other(msg)))
        }
    }
}
